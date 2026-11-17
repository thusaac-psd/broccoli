//! Shared submission-detail visibility helpers for contest plugins.
//!
//! The per-contest-type plugins (icpc, ioi, ...) each echo a submission's
//! free-text fields back in their submission-detail responses. Capping those
//! fields is scoreboard/payload-integrity logic that MUST be identical across
//! plugins, so it lives here rather than being copy-pasted into each. (The
//! plugin-specific parts - icpc's freeze-hiding, ioi's feedback-level redaction
//! - stay in their own crates.)

/// The submission field whose hiding withholds the submission's whole
/// outcome. A visibility decision that redacts this path presents the
/// submission to that viewer as withheld (`withheld: {"reason": "hidden"}`)
/// with no result (no verdict, score, resources or test case results), and a
/// submission list filtered or sorted by status does not place it, so neither
/// reveals the outcome.
pub const SUBMISSION_STATUS_PATH: &str = "status";

/// The decision that withholds a submission's whole outcome from a viewer
/// ([`SUBMISSION_STATUS_PATH`]), as a scoreboard freeze or a feedback level
/// that shows nothing does: every client reads the submission as withheld,
/// never as judged without a result.
pub fn hide_outcome() -> crate::types::WireDecision {
    crate::types::WireDecision::Redact {
        fields: vec![SUBMISSION_STATUS_PATH.into()],
    }
}

/// Byte cap for a free-text field echoed back in a submission-detail response,
/// so a pathological compiler dump / checker output / test-case blob cannot
/// bloat the API payload. Shared so the bound cannot drift between plugins.
pub const DETAIL_TEXT_RESPONSE_LIMIT_BYTES: usize = 65_536;

/// Truncate a string to at most [`DETAIL_TEXT_RESPONSE_LIMIT_BYTES`] bytes,
/// never splitting a multi-byte UTF-8 character.
fn cap_detail_text(mut value: String) -> String {
    if value.len() <= DETAIL_TEXT_RESPONSE_LIMIT_BYTES {
        return value;
    }

    let mut end = DETAIL_TEXT_RESPONSE_LIMIT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

fn cap_json_string_field(map: &mut serde_json::Map<String, serde_json::Value>, field: &str) {
    let Some(value) = map.get_mut(field) else {
        return;
    };
    let Some(text) = value.as_str() else {
        return;
    };
    if text.len() <= DETAIL_TEXT_RESPONSE_LIMIT_BYTES {
        return;
    }
    *value = serde_json::Value::String(cap_detail_text(text.to_string()));
}

/// Cap the free-text fields of a submission-detail JSON value in place:
/// `result.compile_output` / `result.error_message`, and each test case's
/// `input` / `expected_output` / `stdout` / `stderr` / `checker_output`.
pub fn cap_submission_detail_texts(submission: &mut serde_json::Value) {
    let Some(result) = submission.get_mut("result").and_then(|v| v.as_object_mut()) else {
        return;
    };

    for field in ["compile_output", "error_message"] {
        cap_json_string_field(result, field);
    }

    let Some(test_case_results) = result
        .get_mut("test_case_results")
        .and_then(|v| v.as_array_mut())
    else {
        return;
    };

    for test_case_result in test_case_results {
        let Some(test_case_result) = test_case_result.as_object_mut() else {
            continue;
        };
        for field in [
            "input",
            "expected_output",
            "stdout",
            "stderr",
            "checker_output",
        ] {
            cap_json_string_field(test_case_result, field);
        }
    }
}

/// A submission of a visibility batch that belongs to a contest of the
/// asking plugin's own contest type, as [`decide_contest_submissions`] hands
/// it to the plugin's decision.
#[cfg(feature = "guest")]
pub struct ContestSubmission<'a, R> {
    pub resource: &'a crate::types::QueryResource,
    /// The contest the resource names, of the plugin's contest type.
    pub contest_id: i32,
    pub submission_id: i32,
    /// The submission's author.
    pub user_id: i32,
    /// The plugin's own columns of the submission's row.
    pub row: &'a R,
}

#[cfg(feature = "guest")]
#[derive(serde::Deserialize)]
struct BatchRow<R> {
    submission_id: i32,
    user_id: i32,
    contest_type: Option<String>,
    #[serde(flatten)]
    row: R,
}

/// The decisions of a contest-type plugin's visibility query, for the rules
/// every such plugin shares; the plugin decides only its own submissions.
///
/// A resource that is not a submission of a contest is allowed, as is a
/// submission of a contest of another type: the plugin has no opinion on
/// either, and `Allow` never widens what the host or another plugin decided.
/// A submission with no row in a contest gets `hidden`: the batch names it
/// with a contest it cannot belong to, so it fails hidden rather than leak. Every other submission is decided by `decide`.
///
/// The batch costs one query for every submission row, whatever its size:
/// `s.id AS submission_id, s.user_id, c.contest_type` from `submission s`
/// joined to its `contest c`, plus `columns` (a comma-separated select list
/// over `s` and `c`, or empty) read into `R`; pass `serde::de::IgnoredAny`
/// for no columns. `load` runs once, with the distinct contests of the
/// plugin's type in the batch, for what the plugin reads per contest (its
/// config, stored state), so nothing is read once per submission.
#[cfg(feature = "guest")]
pub fn decide_contest_submissions<R, X>(
    host: &crate::Host,
    req: &crate::types::VisibilityQueryInput,
    contest_type: &str,
    columns: &str,
    hidden: &crate::types::WireDecision,
    load: impl FnOnce(&[i32]) -> Result<X, crate::error::SdkError>,
    decide: impl Fn(&X, &ContestSubmission<'_, R>) -> crate::types::WireDecision,
) -> Result<Vec<crate::types::WireDecision>, crate::error::SdkError>
where
    R: serde::de::DeserializeOwned,
{
    use std::collections::HashMap;

    use crate::types::WireDecision;

    let asked: Vec<Option<(i32, i32)>> = req
        .resources
        .iter()
        .map(|resource| {
            let contest_id = resource.contest_id?;
            (resource.kind == "submission")
                .then(|| resource.id.parse::<i32>().ok())
                .flatten()
                .map(|id| (id, contest_id))
        })
        .collect();
    let mut submission_ids: Vec<i32> = asked.iter().flatten().map(|(id, _)| *id).collect();
    if submission_ids.is_empty() {
        return Ok(vec![WireDecision::Allow {}; req.resources.len()]);
    }
    submission_ids.sort_unstable();
    submission_ids.dedup();

    let mut p = crate::db::Params::new();
    let placeholders: Vec<String> = submission_ids.iter().map(|id| p.bind(*id)).collect();
    let columns = if columns.trim().is_empty() {
        String::new()
    } else {
        format!(", {columns}")
    };
    let sql = format!(
        "SELECT s.id AS submission_id, s.user_id, c.contest_type{columns} \
         FROM submission s \
         JOIN contest c ON c.id = s.contest_id \
         WHERE s.id IN ({})",
        placeholders.join(","),
    );
    let rows: Vec<BatchRow<R>> = host.db.query_with_args(&sql, &p.into_args())?;
    let rows: HashMap<i32, BatchRow<R>> = rows.into_iter().map(|r| (r.submission_id, r)).collect();
    let own = |row: &BatchRow<R>| row.contest_type.as_deref() == Some(contest_type);

    let mut contest_ids: Vec<i32> = asked
        .iter()
        .flatten()
        .filter(|(id, _)| rows.get(id).is_some_and(own))
        .map(|(_, contest_id)| *contest_id)
        .collect();
    contest_ids.sort_unstable();
    contest_ids.dedup();
    let loaded = load(&contest_ids)?;

    Ok(req
        .resources
        .iter()
        .zip(&asked)
        .map(|(resource, asked)| {
            let Some((submission_id, contest_id)) = *asked else {
                return WireDecision::Allow {};
            };
            let Some(row) = rows.get(&submission_id) else {
                return hidden.clone();
            };
            if !own(row) {
                return WireDecision::Allow {};
            }
            decide(
                &loaded,
                &ContestSubmission {
                    resource,
                    contest_id,
                    submission_id,
                    user_id: row.user_id,
                    row: &row.row,
                },
            )
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_oversized_detail_texts() {
        let big = "x".repeat(DETAIL_TEXT_RESPONSE_LIMIT_BYTES + 10);
        let mut sub = serde_json::json!({
            "result": {
                "compile_output": big,
                "test_case_results": [{"checker_output": "y".repeat(DETAIL_TEXT_RESPONSE_LIMIT_BYTES * 2)}]
            }
        });
        cap_submission_detail_texts(&mut sub);
        let capped = sub["result"]["compile_output"].as_str().unwrap();
        assert_eq!(capped.len(), DETAIL_TEXT_RESPONSE_LIMIT_BYTES);
        let tc = sub["result"]["test_case_results"][0]["checker_output"]
            .as_str()
            .unwrap();
        assert_eq!(tc.len(), DETAIL_TEXT_RESPONSE_LIMIT_BYTES);
    }

    #[test]
    fn cap_respects_char_boundaries() {
        // Multi-byte char straddling the limit must not split.
        let mut s = "a".repeat(DETAIL_TEXT_RESPONSE_LIMIT_BYTES - 1);
        s.push('é'); // 2 bytes, crosses the boundary
        s.push_str("tail");
        let capped = cap_detail_text(s);
        assert!(capped.len() <= DETAIL_TEXT_RESPONSE_LIMIT_BYTES);
        assert!(capped.is_char_boundary(capped.len()));
    }
}

#[cfg(all(test, feature = "guest"))]
mod contest_submission_tests {
    use std::cell::RefCell;

    use serde::de::IgnoredAny;
    use serde_json::json;

    use super::*;
    use crate::Host;
    use crate::types::{
        QueryContext, QueryResource, QuerySubject, VisibilityQueryInput, WireDecision,
    };

    fn resource(kind: &str, id: &str, contest_id: Option<i32>) -> QueryResource {
        QueryResource {
            kind: kind.into(),
            id: id.into(),
            contest_id,
            problem_id: None,
            contest_phase: None,
        }
    }

    fn batch(resources: Vec<QueryResource>) -> VisibilityQueryInput {
        VisibilityQueryInput {
            subject: QuerySubject {
                user_id: Some(1),
                authenticated: true,
                permissions: vec![],
            },
            action: "read".into(),
            context: QueryContext { contest_id: None },
            resources,
        }
    }

    fn hidden() -> WireDecision {
        hide_outcome()
    }

    #[test]
    fn the_plugin_decides_only_submissions_of_its_own_contest_type() {
        let host = Host::mock();
        host.db.queue_query_result(json!([
            {"submission_id": 10, "user_id": 2, "contest_type": "mine"},
            {"submission_id": 11, "user_id": 3, "contest_type": "other"},
            {"submission_id": 12, "user_id": 4, "contest_type": "mine"}
        ]));
        let req = batch(vec![
            resource("problem", "5", Some(7)),
            resource("submission", "10", Some(7)),
            resource("submission", "11", Some(8)),
            resource("submission", "13", Some(7)),
            resource("submission", "14", None),
            resource("submission", "12", Some(9)),
            resource("submission", "10", Some(7)),
        ]);
        let loaded = RefCell::new(Vec::new());
        let decisions = decide_contest_submissions(
            &host,
            &req,
            "mine",
            "",
            &hidden(),
            |contest_ids| {
                loaded.borrow_mut().push(contest_ids.to_vec());
                Ok(())
            },
            |(), submission: &ContestSubmission<'_, IgnoredAny>| WireDecision::Redact {
                fields: vec![format!("{}:{}", submission.contest_id, submission.user_id)],
            },
        )
        .unwrap();

        let own = |field: &str| WireDecision::Redact {
            fields: vec![field.into()],
        };
        assert_eq!(
            decisions,
            vec![
                WireDecision::Allow {},
                own("7:2"),
                WireDecision::Allow {},
                hidden(),
                WireDecision::Allow {},
                own("9:4"),
                own("7:2"),
            ]
        );
        assert_eq!(
            *loaded.borrow(),
            vec![vec![7, 9]],
            "loaded once, per contest"
        );
        assert_eq!(host.db.queries().len(), 1, "one read for the batch");
    }

    #[test]
    fn a_batch_without_contest_submissions_reads_nothing() {
        let host = Host::mock();
        let req = batch(vec![
            resource("contest", "7", Some(7)),
            resource("submission", "3", None),
        ]);
        let decisions = decide_contest_submissions(
            &host,
            &req,
            "mine",
            "",
            &hidden(),
            |_| -> Result<(), crate::error::SdkError> { panic!("nothing to load") },
            |(), _: &ContestSubmission<'_, IgnoredAny>| hidden(),
        )
        .unwrap();
        assert_eq!(decisions, vec![WireDecision::Allow {}; 2]);
        assert!(host.db.queries().is_empty());
    }
}
