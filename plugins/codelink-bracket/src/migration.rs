//! Plugin-owned upgrades of private persisted documents, run during init.
use broccoli_server_sdk::prelude::*;
use serde_json::{Value, json};

const VERSION_KEY: &str = "schema_version";
const VERSION: u64 = 4;

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
        crate::storage::scan_documents(host, prefix, |key, _| {
            for _ in 0..100 {
                let Some(snapshot) = host.storage.snapshot(key)? else {
                    return Ok(());
                };
                let mut document: Value = serde_json::from_str(&snapshot.value)?;
                upgrade_document(prefix, &mut document)?;
                if prefix == "setup:" {
                    upgrade_stage_rules(host, key, &mut document)?;
                }
                let new = serde_json::to_string(&document)?;
                if new == snapshot.value
                    || host
                        .storage
                        .compare_and_set_snapshot(key, Some(&snapshot), &new)?
                {
                    return Ok(());
                }
            }
            Err(SdkError::Other(
                "Bracket migration CAS retry limit exceeded".into(),
            ))
        })?;
    }
    // The old host feedback protocol carried an enum-specific result. Restore
    // this plugin's public verdict before marking the upgrade complete.
    for record in host
        .submission
        .feedback_all(&SubmissionFeedbackQuery::default())?
    {
        if record
            .metadata
            .get("previous_outcome")
            .and_then(Value::as_str)
            == Some("too_late")
            && record.withheld
        {
            host.submission.release_feedback(
                record.submission_id,
                ReleaseResult::Override {
                    verdict: Verdict::Other("TooLate".into()),
                    score: 0.0,
                },
            )?;
        }
    }
    host.storage.set(&[(VERSION_KEY, &VERSION.to_string())])?;
    Ok(())
}

/// v3 moves the original 16 seed slots into the setup document. Matches keep
/// their ids and all progress; every created stage is frozen on upgrade.
fn upgrade_stage_rules(host: &Host, key: &str, document: &mut Value) -> Result<(), SdkError> {
    let contest: i32 = key
        .strip_prefix("setup:")
        .unwrap()
        .parse()
        .map_err(|_| SdkError::Other("Invalid setup key".into()))?;
    let object = document.as_object_mut().unwrap();
    if !object.contains_key("seeds") {
        let keys: Vec<_> = (0..16)
            .map(|pos| crate::storage::slot_key(contest, 1, pos))
            .collect();
        let slots = host
            .storage
            .get(&keys.iter().map(String::as_str).collect::<Vec<_>>())?;
        let seeds = keys
            .iter()
            .map(|key| {
                slots
                    .get(key)
                    .and_then(|value| value.parse::<i32>().ok())
                    .ok_or_else(|| SdkError::Other(format!("Missing or invalid stored seed {key}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        object.insert("seeds".into(), json!(seeds));
    }
    if !object.contains_key("opened_rounds") {
        let prefix = format!("match:{contest}:");
        let mut opened = 0;
        crate::storage::scan_documents(host, &prefix, |_, raw| {
            let state: Value = serde_json::from_str(raw)?;
            let round = state
                .get("round")
                .and_then(Value::as_u64)
                .ok_or_else(|| SdkError::Other("Missing stored match round".into()))?;
            opened = opened.max(round);
            Ok(())
        })?;
        object.insert("opened_rounds".into(), json!(opened));
    }
    // v4 enables majority FT for future stages. Opened stages retain the
    // previous play-all rule by setting an FT equal to their BO cap.
    let opened = object
        .get("opened_rounds")
        .and_then(Value::as_u64)
        .ok_or_else(|| SdkError::Other("Invalid stored opened stage count".into()))?
        as usize;
    let rounds = object
        .get_mut("rounds")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| SdkError::Other("Invalid stored bracket stages".into()))?;
    for (index, round) in rounds.iter_mut().enumerate() {
        let stage = round
            .as_object_mut()
            .ok_or_else(|| SdkError::Other("Invalid stored bracket stage".into()))?;
        if stage.get("wins_to_advance").is_none_or(Value::is_null) {
            let games = stage
                .get("group_a")
                .and_then(Value::as_array)
                .ok_or_else(|| SdkError::Other("Invalid stored bracket problem group".into()))?
                .len();
            let target = if index < opened { games } else { games / 2 + 1 };
            stage.insert("wins_to_advance".into(), json!(target));
        }
    }
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
    fn version_three_preserves_opened_rules_and_enables_ft_for_future_stages() {
        let host = Host::mock();
        let mut setup = crate::model::Setup {
            stage_clocks: Default::default(),
            third_place: None,
            seeds: (1..=8).collect(),
            opened_rounds: 1,
            rounds: vec![
                crate::model::RoundDef {
                    group_a: vec![1, 2, 3],
                    group_b: vec![4, 5, 6],
                    tiebreak: vec![7],
                    xiaoju_seconds: 60,
                    tiebreak_seconds: 60,
                    ..Default::default()
                };
                3
            ],
            ..Default::default()
        };
        setup.rounds[1].group_a.extend([8, 9]);
        setup.rounds[2].wins_to_advance = Some(1);
        let mut raw = serde_json::to_value(&setup).unwrap();
        raw["rounds"][0]
            .as_object_mut()
            .unwrap()
            .remove("wins_to_advance");
        host.storage
            .set(&[(VERSION_KEY, "3"), ("setup:7", &raw.to_string())])
            .unwrap();
        run(&host).unwrap();
        let migrated = crate::storage::load_setup(&host, 7).unwrap().unwrap();
        assert_eq!(
            migrated
                .rounds
                .iter()
                .map(|r| r.win_target())
                .collect::<Vec<_>>(),
            vec![3, 3, 1]
        );
        assert_eq!(migrated.opened_rounds, 1);
        let saved = host.storage.data();
        run(&host).unwrap();
        assert_eq!(host.storage.data(), saved);
    }

    #[test]
    fn version_two_preserves_match_progress_and_freezes_partial_stage_openings() {
        let host = Host::mock();
        let mut old = serde_json::to_value(crate::model::Setup {
            stage_clocks: Default::default(),
            third_place: None,
            rounds: vec![crate::model::RoundDef::default(); 4],
            seeds: (10..26).collect(),
            ..Default::default()
        })
        .unwrap();
        old.as_object_mut().unwrap().remove("seeds");
        old.as_object_mut().unwrap().remove("opened_rounds");
        host.storage
            .set(&[("schema_version", "2"), ("setup:7", &old.to_string())])
            .unwrap();
        for pos in 0..16 {
            host.storage
                .set(&[(
                    &crate::storage::slot_key(7, 1, pos),
                    &(10 + i32::from(pos)).to_string(),
                )])
                .unwrap();
        }
        let state = crate::model::MatchState {
            round: 2,
            score_a: 2,
            extra_time_remaining_ms: 17000,
            game_durations: Some(crate::model::GameDurations {
                xiaoju_seconds: 70,
                tiebreak_seconds: 90,
            }),
            ..Default::default()
        };
        let raw = serde_json::to_string(&state).unwrap();
        host.storage.set(&[("match:7:8", &raw)]).unwrap();
        run(&host).unwrap();
        let setup = crate::storage::load_setup(&host, 7).unwrap().unwrap();
        assert_eq!(setup.seeds, (10..26).collect::<Vec<_>>());
        assert_eq!(setup.opened_rounds, 2);
        assert_eq!(
            crate::storage::load_match(&host, 7, 8).unwrap().unwrap(),
            state
        );
        let saved = host.storage.data();
        run(&host).unwrap();
        assert_eq!(host.storage.data(), saved);
    }

    #[test]
    fn interrupted_seed_upgrade_retains_version_and_resumes_idempotently() {
        let host = Host::mock();
        let mut setup = serde_json::to_value(crate::model::Setup::default()).unwrap();
        setup.as_object_mut().unwrap().remove("seeds");
        setup.as_object_mut().unwrap().remove("opened_rounds");
        host.storage
            .set(&[("schema_version", "2"), ("setup:7", &setup.to_string())])
            .unwrap();
        for pos in 0..15 {
            host.storage
                .set(&[(&crate::storage::slot_key(7, 1, pos), &pos.to_string())])
                .unwrap();
        }
        assert!(run(&host).is_err());
        assert_eq!(
            host.storage.get_one(VERSION_KEY).unwrap().as_deref(),
            Some("2")
        );
        host.storage.set(&[("slot:7:1:15", "15")]).unwrap();
        run(&host).unwrap();
        let migrated = crate::storage::load_setup(&host, 7).unwrap().unwrap();
        assert_eq!(migrated.seeds, (0..16).collect::<Vec<_>>());
        assert_eq!(migrated.opened_rounds, 0);
        let saved = host.storage.data();
        run(&host).unwrap();
        assert_eq!(host.storage.data(), saved);
    }

    #[test]
    fn upgrades_private_documents_once_and_keeps_current_settings() {
        let host = Host::mock();
        let setup = json!({"rounds":[{"group_a":vec![1,2,3],"group_b":vec![4,5,6],"tiebreak":[7]},
            {"xiaoju_seconds":900,"group_a":vec![8,9,10],"group_b":vec![11,12,13],"tiebreak":[14]}],
            "xiaoju_seconds":300,"round_intermission_seconds":40});
        let game = json!({"round":1,"xiaoju_seconds":0,"admission_revision":9,"admission_by_user":{"1":3},
            "xiaoju":[{"opened_at_ms":1000,"deadline_ms":91000}],"winner":1});
        host.storage
            .set(&[
                ("setup:1", &setup.to_string()),
                ("match:1:0", &game.to_string()),
            ])
            .unwrap();
        for pos in 0..16 {
            host.storage
                .set(&[(
                    &crate::storage::slot_key(1, 1, pos),
                    &(i32::from(pos) + 1).to_string(),
                )])
                .unwrap();
        }
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
        let publications = host.submission.feedback_actions();
        assert_eq!(publications.len(), 1);
        assert_eq!(
            match &publications[0].1 {
                SubmissionFeedbackAction::Release(ReleaseResult::Override { verdict, .. }) =>
                    verdict,
                _ => panic!("expected override"),
            },
            &Verdict::Other("TooLate".into())
        );
        assert_eq!(
            host.storage.get_one(VERSION_KEY).unwrap().as_deref(),
            Some("4")
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
