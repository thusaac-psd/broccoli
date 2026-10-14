//! Plugin-owned upgrades of private persisted documents, run during init.
use broccoli_server_sdk::prelude::*;
use serde_json::{Value, json};

const VERSION_KEY: &str = "schema_version";
const VERSION: u64 = 2;

pub fn run(host: &Host) -> Result<(), SdkError> {
    let version = host
        .storage
        .get_one(VERSION_KEY)?
        .map(|v| serde_json::from_str::<u64>(&v))
        .transpose()?
        .unwrap_or(0);
    if version > VERSION {
        return Err(SdkError::Other(
            "Stored bracket schema is newer than this plugin".into(),
        ));
    }
    if version == VERSION {
        return Ok(());
    }
    for prefix in ["setup:", "match:"] {
        let mut after: Option<String> = None;
        loop {
            let entries = host.storage.scan(prefix, after.as_deref())?;
            if entries.is_empty() {
                break;
            }
            after = entries.keys().max().cloned();
            for key in entries.keys() {
                let mut upgraded = false;
                for _ in 0..100 {
                    let Some(snapshot) = host.storage.snapshot(key)? else {
                        upgraded = true;
                        break;
                    };
                    let mut document: Value = serde_json::from_str(&snapshot.value)?;
                    upgrade_document(prefix, &mut document)?;
                    let new = serde_json::to_string(&document)?;
                    if new == snapshot.value
                        || host
                            .storage
                            .compare_and_set_snapshot(key, Some(&snapshot), &new)?
                    {
                        upgraded = true;
                        break;
                    }
                }
                if !upgraded {
                    return Err(SdkError::Other(
                        "Bracket migration CAS retry limit exceeded".into(),
                    ));
                }
            }
        }
    }
    // The old host feedback protocol carried an enum-specific result. Restore
    // this plugin's public verdict before marking the upgrade complete.
    for record in host
        .submission
        .feedback(&SubmissionFeedbackQuery::default())?
    {
        if record
            .metadata
            .get("previous_outcome")
            .and_then(Value::as_str)
            == Some("too_late")
            && record.withheld
        {
            host.submission.publish_feedback(
                record.submission_id,
                &SubmissionFeedbackPublication {
                    withheld: false,
                    release_slot: true,
                    result: Some(SubmissionFeedbackResult {
                        verdict: Verdict::Other("TooLate".into()),
                        score: 0.0,
                    }),
                },
            )?;
        }
    }
    host.storage.set(&[(VERSION_KEY, &VERSION.to_string())])?;
    Ok(())
}

fn upgrade_document(prefix: &str, document: &mut Value) -> Result<(), SdkError> {
    let object = document
        .as_object_mut()
        .ok_or_else(|| SdkError::Other("Invalid bracket document".into()))?;
    if prefix == "setup:" {
        if let Some(seconds) = object.remove("xiaoju_seconds") {
            let intermission = object
                .remove("round_intermission_seconds")
                .ok_or_else(|| SdkError::Other("Missing stored bracket intermission".into()))?;
            let rounds = object
                .get_mut("rounds")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| SdkError::Other("Invalid stored bracket stages".into()))?;
            for (index, round) in rounds.iter_mut().enumerate() {
                let stage = round
                    .as_object_mut()
                    .ok_or_else(|| SdkError::Other("Invalid stored bracket stage".into()))?;
                stage
                    .entry("xiaoju_seconds")
                    .or_insert_with(|| seconds.clone());
                stage
                    .entry("tiebreak_seconds")
                    .or_insert_with(|| seconds.clone());
                stage.entry("break_before_seconds").or_insert_with(|| {
                    if index == 0 {
                        json!(0)
                    } else {
                        intermission.clone()
                    }
                });
            }
        }
    } else {
        object.remove("admission_revision");
        object.remove("admission_by_user");
        if !object.contains_key("game_durations") {
            let seconds = object
                .get("xiaoju_seconds")
                .and_then(Value::as_i64)
                .filter(|s| *s > 0);
            let games = object.get("xiaoju").and_then(Value::as_array);
            let durations = if let Some(game) = games.and_then(|g| g.first()) {
                let seconds = seconds
                    .or_else(|| {
                        Some(
                            (game.get("deadline_ms")?.as_i64()?
                                - game.get("opened_at_ms")?.as_i64()?)
                                / 1000,
                        )
                    })
                    .ok_or_else(|| SdkError::Other("Missing stored game duration".into()))?;
                json!({"xiaoju_seconds":seconds,"tiebreak_seconds":seconds})
            } else {
                Value::Null
            };
            object.insert("game_durations".into(), durations);
        }
        object.remove("xiaoju_seconds");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upgrades_private_documents_once_and_keeps_current_settings() {
        let host = Host::mock();
        let setup = json!({"rounds":[{"group_a":[1,2,3],"group_b":[4,5,6],"tiebreak":[7]},
            {"xiaoju_seconds":900,"group_a":[8,9,10],"group_b":[11,12,13],"tiebreak":[14]}],
            "xiaoju_seconds":300,"round_intermission_seconds":40});
        let game = json!({"xiaoju_seconds":0,"admission_revision":9,"admission_by_user":{"1":3},
            "xiaoju":[{"opened_at_ms":1000,"deadline_ms":91000}],"winner":1});
        host.storage
            .set(&[
                ("setup:1", &setup.to_string()),
                ("match:1:0", &game.to_string()),
            ])
            .unwrap();
        run(&host).unwrap();
        let stored: Value =
            serde_json::from_str(&host.storage.get_one("setup:1").unwrap().unwrap()).unwrap();
        assert_eq!(stored["rounds"][0]["xiaoju_seconds"], 300);
        assert_eq!(stored["rounds"][0]["break_before_seconds"], 0);
        assert_eq!(stored["rounds"][1]["xiaoju_seconds"], 900);
        assert_eq!(stored["rounds"][1]["tiebreak_seconds"], 300);
        assert_eq!(stored["rounds"][1]["break_before_seconds"], 40);
        let stored: Value =
            serde_json::from_str(&host.storage.get_one("match:1:0").unwrap().unwrap()).unwrap();
        assert_eq!(stored["game_durations"]["xiaoju_seconds"], 90);
        assert!(stored.get("admission_revision").is_none());
        assert_eq!(stored["winner"], 1);
        let once = host.storage.data();
        run(&host).unwrap();
        assert_eq!(host.storage.data(), once);
    }
    #[test]
    fn failed_public_result_upgrade_does_not_mark_the_schema_complete() {
        let host = Host::mock();
        host.submission.set_feedback(vec![SubmissionFeedbackRecord {
            submission_id: 9,
            withheld: true,
            metadata: json!({"previous_outcome":"too_late"}),
            ..Default::default()
        }]);
        host.submission
            .queue_feedback_error(SdkError::Other("publication unavailable".into()));
        assert!(run(&host).is_err());
        assert!(host.storage.get_one(VERSION_KEY).unwrap().is_none());
        run(&host).unwrap();
        let publications = host.submission.feedback_publications();
        assert_eq!(publications.len(), 1);
        assert_eq!(
            publications[0].1.result.as_ref().unwrap().verdict,
            Verdict::Other("TooLate".into())
        );
        assert_eq!(
            host.storage.get_one(VERSION_KEY).unwrap().as_deref(),
            Some("2")
        );
    }

    #[test]
    fn migrates_more_than_one_scan_page() {
        let host = Host::mock();
        for i in 0..105 {
            host.storage
                .set(&[(
                    &format!("match:{i}:0"),
                    r#"{"xiaoju":[],"xiaoju_seconds":0}"#,
                )])
                .unwrap();
        }
        run(&host).unwrap();
        for i in 0..105 {
            let value = host
                .storage
                .get_one(&format!("match:{i}:0"))
                .unwrap()
                .unwrap();
            assert!(value.contains("game_durations"));
        }
    }
}
