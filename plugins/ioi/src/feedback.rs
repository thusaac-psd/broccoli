#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::permissions as perm;
use broccoli_server_sdk::prelude::*;

use crate::config::{ContestConfig, FeedbackLevel};
#[cfg(target_arch = "wasm32")]
use crate::load_token_state;
#[cfg(any(target_arch = "wasm32", test))]
use crate::scoreboard::full_scoreboard_visible_for_phase;
#[cfg(any(target_arch = "wasm32", test))]
use crate::tokens::TokenState;

#[cfg(target_arch = "wasm32")]
pub(crate) fn can_view_privileged_submission_feedback(req: &PluginHttpRequest) -> bool {
    req.has_permission(perm::CONTEST_MANAGE) || req.has_permission(perm::SUBMISSION_VIEW_ALL)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn viewer_has_token_feedback_for_submission(
    host: &Host,
    req: &PluginHttpRequest,
    contest_id: i32,
    submission_id: i32,
) -> Result<bool, SdkError> {
    let Some(user_id) = req.user_id() else {
        return Ok(false);
    };

    let token_state = load_token_state(host, contest_id, user_id)?;
    Ok(token_state.tokened_submission_ids.contains(&submission_id))
}

// -- Submission visibility decisions -------------------------------------
//
// Replaces the old `filter_submission_for_viewer` host-fn hook, which handed
// the host a plugin-authored submission JSON blob after only a shape check --
// a plugin could alter a verdict, a score, or otherwise author content by
// WRITING into it. A field mask can only blank a value, never author one:
// `subtask_scores`/`total_only` used to WRITE `verdict: "Skipped"` and
// `score: 0.0` into every element of `test_case_results`. The mask below
// NULLS those two fields instead; the IOI frontend renders a null verdict /
// null score exactly like the old `"Skipped"` / `0.0` did (see
// `IoiSubmissionResult.tsx`). Equivalence is defined at the rendered level,
// not the wire level.
//
// Invoked by the host's visibility kernel (`[[server.queries]] topic =
// "visibility"`) for every resource in a query batch, for EVERY plugin
// registered on that topic -- not just IOI contests, and not just
// submissions. `decide_visibility_decisions` therefore defaults to Allow for
// anything it has no opinion about (kind != "submission", no contest_id, or a
// submission belonging to a non-IOI contest): Allow is the identity element
// of the host's `Decision::meet` lattice, so it can never widen what the host
// or another plugin already decided.
//
// Unlike ICPC (binary "hidden or not", so the owner is always unconditional
// Allow), IOI narrows even the OWNER's own view by `feedback_level` -- a
// contestant does not automatically see their own per-test-case results
// while the configured level withholds them. A spent per-submission token
// overrides this and unlocks Allow for that one submission, mirroring the
// old `apply_feedback_filter`'s tokened-owner bypass.

/// Field mask covering every per-test-case field the old
/// `redact_submission_for_level` used to blank/overwrite for
/// `subtask_scores`/`total_only`. `verdict`/`score` are now NULLED (never
/// authored as `"Skipped"`/`0.0`); the frontend restores the equivalent
/// rendering. Uses the `*` wildcard (Task 18 grammar) to reach every element
/// of `result.test_case_results` with one path per field, rather than one
/// path per test case.
#[cfg(any(target_arch = "wasm32", test))]
fn per_test_case_mask_fields() -> Vec<String> {
    vec![
        "result.test_case_results.*.verdict".to_string(),
        "result.test_case_results.*.score".to_string(),
        "result.test_case_results.*.time_used".to_string(),
        "result.test_case_results.*.memory_used".to_string(),
        "result.test_case_results.*.input".to_string(),
        "result.test_case_results.*.expected_output".to_string(),
        "result.test_case_results.*.stdout".to_string(),
        "result.test_case_results.*.stderr".to_string(),
        "result.test_case_results.*.checker_output".to_string(),
    ]
}

/// Field mask covering every field the old `redact_submission_for_level`
/// blanked for `FeedbackLevel::None`, unioned across every host DTO shape it
/// can apply to: list items carry verdict/score/time_used/memory_used at the
/// top level and have no `result` key at all; detail responses nest the same
/// four fields, plus compile_output/error_message/test_case_results, under
/// `result`; the judgement-history endpoint applies the mask to a synthetic
/// `SubmissionResponse { result: ... }` wrapper built around the flat
/// `SubmissionJudgementResponse`, so the `result.*` paths reach it too.
/// `apply_mask` (`packages/server/src/visibility/mask.rs`) is a documented
/// no-op on a path that does not exist in the shape actually being masked --
/// a missing key, a type mismatch, or `*` on a non-array never inserts
/// anything -- so one union list is safe for all three shapes: whichever
/// shape the host happens to be rendering, only the paths that actually
/// exist in THAT shape are blanked. Blanking the whole
/// `result.test_case_results` array produces `[]`, the same empty array the
/// old code hand-wrote.
#[cfg(any(target_arch = "wasm32", test))]
fn none_level_mask_fields() -> Vec<String> {
    vec![
        "verdict".to_string(),
        "score".to_string(),
        "time_used".to_string(),
        "memory_used".to_string(),
        "result.verdict".to_string(),
        "result.score".to_string(),
        "result.time_used".to_string(),
        "result.memory_used".to_string(),
        "result.compile_output".to_string(),
        "result.error_message".to_string(),
        "result.test_case_results".to_string(),
    ]
}

/// The mask for a given `FeedbackLevel`. `Full` blanks nothing, so it is
/// `Allow` -- the identity decision, not a `Redact` with an empty field list.
/// `SubtaskScores` and `TotalOnly` are byte-identical, exactly like the old
/// `redact_submission_for_level`'s `FeedbackLevel::SubtaskScores |
/// FeedbackLevel::TotalOnly` arm: the distinction between the two levels is
/// enforced by IOI's own `/subtask-scores` route (`api::
/// handle_submission_subtask_scores`), not by this generic submission-DTO
/// mask.
#[cfg(any(target_arch = "wasm32", test))]
fn mask_for_level(level: FeedbackLevel) -> WireDecision {
    match level {
        FeedbackLevel::Full => WireDecision::Allow {},
        FeedbackLevel::SubtaskScores | FeedbackLevel::TotalOnly => WireDecision::Redact {
            fields: per_test_case_mask_fields(),
        },
        FeedbackLevel::None => WireDecision::Redact {
            fields: none_level_mask_fields(),
        },
    }
}

/// Row shape for the batched query below: one row per submission resource in
/// the batch, keyed by submission id.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, serde::Deserialize)]
struct SubmissionVisibilityRow {
    submission_id: i32,
    user_id: i32,
    contest_type: Option<String>,
    phase: String,
}

/// Core decision logic. Exercised directly by tests via `Host::mock()` (no
/// wasm32 target required) -- there is no e2e test pinning every branch of
/// this function's behavior, so these unit tests are the evidence it still
/// works.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn decide_visibility_decisions(
    host: &Host,
    req: &VisibilityQueryInput,
) -> Result<Vec<WireDecision>, SdkError> {
    // Admin / view-all bypass, for the whole batch at once -- it does not
    // depend on any individual resource.
    if req
        .subject
        .permissions
        .iter()
        .any(|p| p == perm::SUBMISSION_VIEW_ALL)
    {
        return Ok(req
            .resources
            .iter()
            .map(|_| WireDecision::Allow {})
            .collect());
    }

    // Resources this plugin has any opinion about at all: `kind ==
    // "submission"` with a resolvable id and a known contest_id. Everything
    // else defaults to Allow without touching the database.
    let by_index: Vec<Option<i32>> = req
        .resources
        .iter()
        .map(|resource| {
            if resource.kind == "submission" && resource.contest_id.is_some() {
                resource.id.parse::<i32>().ok()
            } else {
                None
            }
        })
        .collect();

    let mut submission_ids: Vec<i32> = by_index.iter().filter_map(|id| *id).collect();
    if submission_ids.is_empty() {
        return Ok(req
            .resources
            .iter()
            .map(|_| WireDecision::Allow {})
            .collect());
    }
    submission_ids.sort_unstable();
    submission_ids.dedup();

    // ONE batched query for every submission id in the batch -- never one
    // query per submission, which would reintroduce an N+1 on the
    // submission-list hot path. `contest_type` is returned (rather than
    // filtered in the WHERE clause) so a submission belonging to a non-IOI
    // contest can be told apart from a genuine contest/submission data
    // mismatch: the former must Allow (this plugin has no opinion on
    // non-IOI contests), the latter fails hidden like the old per-submission
    // query's "contest row missing" branch did.
    let mut p = Params::new();
    let placeholders: Vec<String> = submission_ids.iter().map(|id| p.bind(*id)).collect();
    let sql = format!(
        "SELECT s.id AS submission_id, s.user_id, c.contest_type, \
                CASE WHEN NOW() < c.start_time THEN 'before' \
                     WHEN NOW() > c.end_time THEN 'after' \
                     ELSE 'during' END AS phase \
         FROM submission s \
         JOIN contest c ON c.id = s.contest_id \
         WHERE s.id IN ({})",
        placeholders.join(",")
    );
    let rows: Vec<SubmissionVisibilityRow> = host.db.query_with_args(&sql, &p.into_args())?;
    let rows_by_id: HashMap<i32, SubmissionVisibilityRow> =
        rows.into_iter().map(|r| (r.submission_id, r)).collect();

    // Distinct IOI contest ids actually present in the batch: config is
    // loaded once per DISTINCT contest, never once per submission.
    let mut ioi_contest_ids: Vec<i32> = req
        .resources
        .iter()
        .zip(&by_index)
        .filter_map(|(resource, id)| {
            let sub_id = (*id)?;
            let row = rows_by_id.get(&sub_id)?;
            if row.contest_type.as_deref() == Some("ioi") {
                resource.contest_id
            } else {
                None
            }
        })
        .collect();
    ioi_contest_ids.sort_unstable();
    ioi_contest_ids.dedup();

    let mut configs: HashMap<i32, ContestConfig> = HashMap::new();
    for contest_id in &ioi_contest_ids {
        configs.insert(*contest_id, contest::load_config(host, *contest_id)?);
    }

    // Token state: a query batch always has exactly one subject, so this is
    // ONE batched storage read across every distinct contest id in the batch
    // -- never one read per submission.
    let viewer_id = req.subject.user_id;
    let mut token_states: HashMap<i32, TokenState> = HashMap::new();
    if let Some(viewer) = viewer_id
        && !ioi_contest_ids.is_empty()
    {
        let keys: Vec<String> = ioi_contest_ids
            .iter()
            .map(|cid| format!("tokens:{cid}:{viewer}"))
            .collect();
        let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        let raw = host.storage.get(&key_refs)?;
        for (contest_id, key) in ioi_contest_ids.iter().zip(keys.iter()) {
            if let Some(json) = raw.get(key) {
                token_states.insert(*contest_id, serde_json::from_str(json).unwrap_or_default());
            }
        }
    }

    let decisions = req
        .resources
        .iter()
        .zip(&by_index)
        .map(|(resource, id)| {
            let Some(sub_id) = id else {
                return WireDecision::Allow {};
            };
            let Some(row) = rows_by_id.get(sub_id) else {
                // Contest/submission mismatch: fail hidden rather than leak.
                return mask_for_level(FeedbackLevel::None);
            };
            if row.contest_type.as_deref() != Some("ioi") {
                // Not an IOI contest: this plugin has no opinion.
                return WireDecision::Allow {};
            }
            let Some(contest_id) = resource.contest_id else {
                return WireDecision::Allow {};
            };
            let config = configs.get(&contest_id).cloned().unwrap_or_default();

            if viewer_id == Some(row.user_id) {
                // Owner. A spent token unlocks full feedback for this ONE
                // submission regardless of feedback_level; otherwise the
                // owner is narrowed by feedback_level exactly like a peer
                // would be once the scoreboard opens up.
                let tokened = token_states
                    .get(&contest_id)
                    .map(|s| s.tokened_submission_ids.contains(sub_id))
                    .unwrap_or(false);
                if tokened {
                    return WireDecision::Allow {};
                }
                return mask_for_level(config.feedback_level);
            }

            // Peer: scoreboard-integrity gate (mirrors the old filter's
            // rationale). When the full scoreboard is not visible to a
            // non-owner in this phase -- e.g. the default `admins_only`
            // during the live contest -- a peer must not read another
            // contestant's verdict/score/per-test-case results through the
            // submission endpoint. `feedback_level` alone would leak exactly
            // the data the scoreboard withholds, so force the None-level
            // mask regardless of the configured feedback_level.
            if !full_scoreboard_visible_for_phase(&row.phase, false, config.scoreboard_visibility) {
                return mask_for_level(FeedbackLevel::None);
            }
            mask_for_level(config.feedback_level)
        })
        .collect();

    Ok(decisions)
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    #[test]
    fn per_test_case_mask_covers_every_field_the_old_code_wrote_or_blanked() {
        // Old `redact_submission_for_level`'s SubtaskScores/TotalOnly arm:
        // verdict + score (overwritten with "Skipped"/0.0, now nulled) and
        // seven more fields (nulled then, nulled now).
        let fields = per_test_case_mask_fields();
        for f in [
            "result.test_case_results.*.verdict",
            "result.test_case_results.*.score",
            "result.test_case_results.*.time_used",
            "result.test_case_results.*.memory_used",
            "result.test_case_results.*.input",
            "result.test_case_results.*.expected_output",
            "result.test_case_results.*.stdout",
            "result.test_case_results.*.stderr",
            "result.test_case_results.*.checker_output",
        ] {
            assert!(fields.contains(&f.to_string()), "missing field {f}");
        }
        assert_eq!(fields.len(), 9, "no extra fields beyond the old write set");
    }

    #[test]
    fn none_level_mask_covers_every_field_the_old_code_blanked_on_either_shape() {
        let fields = none_level_mask_fields();
        for f in [
            "verdict",
            "score",
            "time_used",
            "memory_used",
            "result.verdict",
            "result.score",
            "result.time_used",
            "result.memory_used",
            "result.compile_output",
            "result.error_message",
            "result.test_case_results",
        ] {
            assert!(fields.contains(&f.to_string()), "missing field {f}");
        }
        assert_eq!(fields.len(), 11, "no extra fields beyond the old blank set");
    }

    #[test]
    fn mask_for_full_is_allow_not_an_empty_redact() {
        assert!(matches!(
            mask_for_level(FeedbackLevel::Full),
            WireDecision::Allow {}
        ));
    }

    #[test]
    fn mask_for_subtask_scores_and_total_only_are_byte_identical() {
        // Mirrors the old code's `FeedbackLevel::SubtaskScores |
        // FeedbackLevel::TotalOnly` single match arm: the two levels produce
        // exactly the same submission-DTO redaction; the difference between
        // them lives entirely in the plugin's own `/subtask-scores` route.
        let a = mask_for_level(FeedbackLevel::SubtaskScores);
        let b = mask_for_level(FeedbackLevel::TotalOnly);
        match (a, b) {
            (WireDecision::Redact { fields: fa }, WireDecision::Redact { fields: fb }) => {
                let mut fa = fa;
                let mut fb = fb;
                fa.sort();
                fb.sort();
                assert_eq!(fa, fb);
            }
            other => panic!("expected both to be Redact, got {other:?}"),
        }
    }

    #[test]
    fn mask_for_none_is_redact_with_the_none_level_fields() {
        match mask_for_level(FeedbackLevel::None) {
            WireDecision::Redact { fields } => {
                let mut got = fields;
                got.sort();
                let mut want = none_level_mask_fields();
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    fn subject(user_id: Option<i32>) -> QuerySubject {
        QuerySubject {
            user_id,
            authenticated: user_id.is_some(),
            permissions: Vec::new(),
        }
    }

    fn submission_resource(id: i32, contest_id: i32) -> QueryResource {
        QueryResource {
            kind: "submission".to_string(),
            id: id.to_string(),
            contest_id: Some(contest_id),
            problem_id: None,
        }
    }

    fn seed_row(host: &Host, submission_id: i32, user_id: i32, contest_type: &str, phase: &str) {
        host.db.queue_query_result(serde_json::json!([{
            "submission_id": submission_id,
            "user_id": user_id,
            "contest_type": contest_type,
            "phase": phase,
        }]));
    }

    fn seed_ioi_config(host: &Host, contest_id: i32, config: serde_json::Value) {
        host.config
            .seed("contest", &contest_id.to_string(), "contest", config);
    }

    #[test]
    fn decide_visibility_admin_bypass_allows_everything_without_querying() {
        let host = Host::mock();
        let mut sub = subject(Some(99));
        sub.permissions.push(perm::SUBMISSION_VIEW_ALL.to_string());
        let req = VisibilityQueryInput {
            subject: sub,
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10), submission_resource(8, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(
            decisions
                .iter()
                .all(|d| matches!(d, WireDecision::Allow {}))
        );
        assert!(host.db.queries().is_empty());
    }

    #[test]
    fn decide_visibility_allows_non_submission_resources_and_contest_less_resources_without_querying()
     {
        let host = Host::mock();
        let req = VisibilityQueryInput {
            subject: subject(Some(99)),
            action: "view".to_string(),
            context: QueryContext { contest_id: None },
            resources: vec![
                QueryResource {
                    kind: "contest".to_string(),
                    id: "10".to_string(),
                    contest_id: Some(10),
                    problem_id: None,
                },
                QueryResource {
                    kind: "submission".to_string(),
                    id: "7".to_string(),
                    contest_id: None, // standalone submission, no contest.
                    problem_id: None,
                },
            ],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert_eq!(decisions.len(), 2);
        assert!(
            decisions
                .iter()
                .all(|d| matches!(d, WireDecision::Allow {}))
        );
        assert!(
            host.db.queries().is_empty(),
            "must not query the database when nothing needs it"
        );
    }

    #[test]
    fn decide_visibility_allows_a_submission_from_a_non_ioi_contest() {
        let host = Host::mock();
        seed_row(&host, 7, 2, "icpc", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(99)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Allow {}));
    }

    #[test]
    fn decide_visibility_fails_hidden_on_contest_submission_mismatch() {
        // No row at all for the submission id: the old code's "contest row
        // missing -- fail hidden rather than leak" branch.
        let host = Host::mock();
        host.db.queue_query_result(serde_json::json!([]));

        let req = VisibilityQueryInput {
            subject: subject(Some(99)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = none_level_mask_fields();
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_fails_hidden_even_for_a_viewer_who_would_be_the_owner_when_the_row_is_missing()
     {
        // Deliberate, safe-direction ordering deviation from the old code
        // (mirrors ICPC's `decide_visibility`): the old
        // `filter_submission_for_viewer` was handed the submission's own
        // JSON (already carrying its `user_id`) and checked ownership FIRST.
        // `decide_visibility` is handed only a resource id and has no
        // `user_id` to compare until AFTER the batched query returns a row.
        // When the query returns no row at all, there is nothing to compare
        // the viewer against, so the missing-row fail-hidden branch runs
        // unconditionally -- even for a viewer who would in fact be the
        // submission's owner if a row existed. This can only over-hide
        // (Redact), never leak (Allow).
        let host = Host::mock();
        host.db.queue_query_result(serde_json::json!([]));

        let req = VisibilityQueryInput {
            subject: subject(Some(2)), // would be the owner, if a row existed.
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(
            matches!(decisions[0], WireDecision::Redact { .. }),
            "a missing row must fail hidden even for a viewer who would otherwise be the owner, got {:?}",
            decisions[0]
        );
    }

    #[test]
    fn decide_visibility_allows_the_owner_at_full_feedback() {
        let host = Host::mock();
        seed_ioi_config(&host, 10, serde_json::json!({ "feedback_level": "full" }));
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(2)), // viewer IS the submission owner.
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Allow {}));
    }

    #[test]
    fn decide_visibility_redacts_the_owner_at_subtask_scores_with_the_per_test_case_mask() {
        let host = Host::mock();
        seed_ioi_config(
            &host,
            10,
            serde_json::json!({ "feedback_level": "subtask_scores" }),
        );
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(2)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = per_test_case_mask_fields();
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_redacts_the_owner_at_total_only_with_the_per_test_case_mask() {
        let host = Host::mock();
        seed_ioi_config(
            &host,
            10,
            serde_json::json!({ "feedback_level": "total_only" }),
        );
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(2)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = per_test_case_mask_fields();
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_redacts_the_owner_at_none_with_the_none_level_mask() {
        // This is the exact scenario the failing e2e test pins: the OWNER,
        // under `feedback_level: "none"`, must still be narrowed -- `meet`
        // between the kernel's unconditional owner Allow and IOI's Redact is
        // Redact, so `score` renders as `null` even for the submitter.
        let host = Host::mock();
        seed_ioi_config(&host, 10, serde_json::json!({ "feedback_level": "none" }));
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(2)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = none_level_mask_fields();
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_tokened_owner_bypasses_feedback_level() {
        let host = Host::mock();
        seed_ioi_config(&host, 10, serde_json::json!({ "feedback_level": "none" }));
        seed_row(&host, 7, 2, "ioi", "during");
        host.storage
            .set(&[(
                "tokens:10:2",
                &serde_json::json!({ "used": 1, "tokened_submission_ids": [7] }).to_string(),
            )])
            .unwrap();

        let req = VisibilityQueryInput {
            subject: subject(Some(2)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Allow {}));
    }

    #[test]
    fn decide_visibility_untokened_submission_is_unaffected_by_a_token_on_another_submission() {
        let host = Host::mock();
        seed_ioi_config(&host, 10, serde_json::json!({ "feedback_level": "none" }));
        seed_row(&host, 7, 2, "ioi", "during");
        host.storage
            .set(&[(
                "tokens:10:2",
                &serde_json::json!({ "used": 1, "tokened_submission_ids": [999] }).to_string(),
            )])
            .unwrap();

        let req = VisibilityQueryInput {
            subject: subject(Some(2)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Redact { .. }));
    }

    #[test]
    fn decide_visibility_redacts_a_peer_to_none_level_when_the_scoreboard_is_hidden_in_this_phase()
    {
        // Scoreboard-integrity gate: even though feedback_level is "full",
        // a peer must not see another contestant's results while the
        // scoreboard itself withholds them (default admins_only, during).
        let host = Host::mock();
        seed_ioi_config(
            &host,
            10,
            serde_json::json!({
                "feedback_level": "full",
                "scoreboard_visibility": "admins_only",
            }),
        );
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(99)), // NOT the owner.
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = none_level_mask_fields();
                want.sort();
                assert_eq!(
                    got, want,
                    "scoreboard-hidden peer must get the None-level mask regardless of feedback_level"
                );
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_allows_a_peer_the_configured_level_once_the_scoreboard_is_visible() {
        let host = Host::mock();
        seed_ioi_config(
            &host,
            10,
            serde_json::json!({
                "feedback_level": "full",
                "scoreboard_visibility": "all_contest_viewers",
            }),
        );
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(99)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert!(matches!(decisions[0], WireDecision::Allow {}));
    }

    #[test]
    fn decide_visibility_redacts_a_peer_per_feedback_level_once_the_scoreboard_is_visible() {
        // The scoreboard being visible does not itself grant full feedback;
        // the configured feedback_level still applies to peers too.
        let host = Host::mock();
        seed_ioi_config(
            &host,
            10,
            serde_json::json!({
                "feedback_level": "subtask_scores",
                "scoreboard_visibility": "all_contest_viewers",
            }),
        );
        seed_row(&host, 7, 2, "ioi", "during");

        let req = VisibilityQueryInput {
            subject: subject(Some(99)),
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = per_test_case_mask_fields();
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_issues_exactly_one_batched_query_for_the_whole_batch() {
        // The critical N+1 guard: a batch of MANY submissions must cost ONE
        // query, not one query per submission.
        let host = Host::mock();
        seed_ioi_config(&host, 10, serde_json::json!({ "feedback_level": "none" }));
        host.db.queue_query_result(serde_json::json!([
            { "submission_id": 7, "user_id": 2, "contest_type": "ioi", "phase": "during" },
            { "submission_id": 8, "user_id": 3, "contest_type": "ioi", "phase": "during" },
            { "submission_id": 9, "user_id": 4, "contest_type": "ioi", "phase": "during" },
        ]));

        let req = VisibilityQueryInput {
            subject: subject(Some(2)), // owns submission 7, peer to 8 and 9.
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![
                submission_resource(7, 10),
                submission_resource(8, 10),
                submission_resource(9, 10),
            ],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert_eq!(decisions.len(), 3);
        let queries = host.db.queries();
        assert_eq!(
            queries.len(),
            1,
            "must issue exactly one query for the whole batch, got: {queries:?}"
        );
        assert!(
            queries[0].sql.contains("IN ("),
            "must be a single IN(...) batch query: {}",
            queries[0].sql
        );

        // The batching property alone is not evidence of correctness: pin
        // what the three decisions actually are. All three are
        // feedback_level "none" with the scoreboard hidden (default
        // admins_only, during) -- submission 7's owner (2) is narrowed by
        // feedback_level same as submissions 8 and 9's peer viewing.
        for (i, decision) in decisions.iter().enumerate() {
            match decision {
                WireDecision::Redact { fields } => {
                    let mut got = fields.clone();
                    got.sort();
                    let mut want = none_level_mask_fields();
                    want.sort();
                    assert_eq!(got, want, "decision {i} mask mismatch");
                }
                other => panic!("decision {i}: expected Redact, got {other:?}"),
            }
        }
    }
}
