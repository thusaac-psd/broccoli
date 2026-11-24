#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

// Host/PluginHttpRequest/SdkError/WireDecision/VisibilityQueryInput/Params are
// used by both the wasm32-gated handlers below and the #[cfg(test)] unit
// tests in `visibility_tests`, which exercise `decide_visibility_decisions`
// directly via `Host::mock()`; gate the same way so a native (test/clippy)
// build doesn't see this as unused.
#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::prelude::*;
#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::visibility::{
    ContestSubmission, decide_contest_submissions, hide_outcome,
};
#[cfg(any(target_arch = "wasm32", test))]
use serde::de::IgnoredAny;

// ContestConfig/FeedbackLevel are used by `decide_visibility_decisions` and
// `mask_for_level` (both wasm32-gated production code) and directly by the
// #[cfg(test)] unit tests below; gate the same way so a native (test/clippy)
// build doesn't see this as unused.
#[cfg(any(target_arch = "wasm32", test))]
use crate::config::{ContestConfig, FeedbackLevel};
#[cfg(target_arch = "wasm32")]
use crate::load_token_state;
#[cfg(any(target_arch = "wasm32", test))]
use crate::scoreboard::full_scoreboard_visible_for_phase;
#[cfg(any(target_arch = "wasm32", test))]
use crate::tokens::TokenState;

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
// Visibility is a field mask, which can only blank a value, never author one.
// At the `subtask_scores`/`total_only` levels the mask nulls `verdict` and
// `score` in every element of `test_case_results`; the IOI frontend renders a
// null verdict / null score as a skipped case (see `IoiSubmissionResult.tsx`).
//
// Invoked by the host's visibility kernel (`[[server.queries]] topic =
// "visibility"`) for every resource in a query batch, for EVERY plugin
// registered on that topic -- not just IOI contests, and not just
// submissions. `decide_contest_submissions` owns the rules every contest
// plugin shares (Allow for what this plugin has no opinion about, fail hidden
// on a mismatch, one batched read); this plugin decides its own submissions.
//
// Unlike ICPC (binary "hidden or not", so the owner is always unconditional
// Allow), IOI narrows even the OWNER's own view by `feedback_level` -- a
// contestant does not automatically see their own per-test-case results
// while the configured level withholds them. A spent per-submission token
// overrides this and unlocks Allow for that one submission.

/// Field mask covering every per-test-case field withheld at
/// `subtask_scores`/`total_only`: nine fields, `verdict`/`score` among them,
/// all nulled. Uses the `*` wildcard to reach every element
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

/// The mask for a given `FeedbackLevel`. `Full` blanks nothing, so it is
/// `Allow` -- the identity decision, not a `Redact` with an empty field list.
/// `SubtaskScores` and `TotalOnly` are byte-identical: the distinction between the two levels is
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
        // Nothing of the result shows: the host presents the submission as
        // withheld, so no client reads it as judged without a verdict.
        FeedbackLevel::None => hide_outcome(),
    }
}

/// Core decision logic. Exercised directly by tests via `Host::mock()` (no
/// wasm32 target required). That organisers see unredacted judgements, which
/// the host rule decides, is pinned end-to-end by
/// `ioi_feedback_organiser_without_submission_view_all_sees_unredacted_judgement`
/// in `packages/server/tests/e2e/plugins/ioi.rs`.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn decide_visibility_decisions(
    host: &Host,
    req: &VisibilityQueryInput,
) -> Result<Vec<WireDecision>, SdkError> {
    // The host never asks this about staff (`contest:manage` or
    // `submission:view_all`): organisers see the real data by the host's
    // rule, so nothing here decides for them.
    let viewer_id = req.subject.user_id;
    decide_contest_submissions(
        host,
        req,
        "ioi",
        "",
        &mask_for_level(FeedbackLevel::None),
        |contest_ids| {
            let mut configs: HashMap<i32, ContestConfig> = HashMap::new();
            for contest_id in contest_ids {
                configs.insert(*contest_id, contest::load_config(host, *contest_id)?);
            }
            // Token state: a query batch always has exactly one subject, so
            // this is ONE batched storage read across every distinct contest
            // in the batch.
            let mut token_states: HashMap<i32, TokenState> = HashMap::new();
            if let Some(viewer) = viewer_id
                && !contest_ids.is_empty()
            {
                let keys: Vec<String> = contest_ids
                    .iter()
                    .map(|cid| format!("tokens:{cid}:{viewer}"))
                    .collect();
                let key_refs: Vec<&str> = keys.iter().map(String::as_str).collect();
                let raw = host.storage.get(&key_refs)?;
                for (contest_id, key) in contest_ids.iter().zip(keys.iter()) {
                    if let Some(json) = raw.get(key) {
                        token_states
                            .insert(*contest_id, serde_json::from_str(json).unwrap_or_default());
                    }
                }
            }
            Ok((configs, token_states))
        },
        |(configs, token_states), submission: &ContestSubmission<'_, IgnoredAny>| {
            let Some(config) = configs.get(&submission.contest_id) else {
                // Every IOI contest of the batch has its config loaded above;
                // a contest without one is a mismatch: fail hidden.
                return mask_for_level(FeedbackLevel::None);
            };

            if viewer_id == Some(submission.user_id) {
                // Owner. A spent token unlocks full feedback for this ONE
                // submission regardless of feedback_level; otherwise the
                // owner is narrowed by feedback_level exactly like a peer
                // would be once the scoreboard opens up.
                let tokened = token_states
                    .get(&submission.contest_id)
                    .is_some_and(|s| s.tokened_submission_ids.contains(&submission.submission_id));
                if tokened {
                    return WireDecision::Allow {};
                }
                return mask_for_level(config.feedback_level);
            }

            // Peer: scoreboard-integrity gate. When the full scoreboard is
            // not visible to a non-owner in this phase -- e.g. the default
            // `admins_only` during the live contest -- a peer must not read
            // another contestant's verdict/score/per-test-case results
            // through the submission endpoint. `feedback_level` alone would
            // leak exactly the data the scoreboard withholds, so force the
            // None-level mask regardless of the configured feedback_level.
            // The host stamps the phase of every contest it resolved; one it
            // did not resolve fails hidden.
            let Some(phase) = submission.resource.contest_phase else {
                return mask_for_level(FeedbackLevel::None);
            };
            if !full_scoreboard_visible_for_phase(
                phase.stage(),
                false,
                config.scoreboard_visibility,
            ) {
                return mask_for_level(FeedbackLevel::None);
            }
            mask_for_level(config.feedback_level)
        },
    )
}

#[cfg(test)]
mod visibility_tests {
    use super::*;
    use broccoli_server_sdk::visibility::SUBMISSION_STATUS_PATH;

    #[test]
    fn per_test_case_mask_covers_every_withheld_per_test_case_field() {
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
        assert_eq!(fields.len(), 9, "no fields beyond the withheld set");
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
        // The two levels produce exactly the same submission-DTO redaction;
        // the difference between them lives entirely in the plugin's own
        // `/subtask-scores` route.
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

    /// At `none` the submission's whole outcome is withheld, so its owner
    /// reads it as withheld (and the CLI says hidden), never as judged.
    #[test]
    fn the_none_level_withholds_the_whole_outcome() {
        assert_eq!(mask_for_level(FeedbackLevel::None), hide_outcome());
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
            contest_phase: Some(ContestPhase::Running),
        }
    }

    fn seed_row(host: &Host, submission_id: i32, user_id: i32, contest_type: &str) {
        host.db.queue_query_result(serde_json::json!([{
            "submission_id": submission_id,
            "user_id": user_id,
            "contest_type": contest_type,
        }]));
    }

    fn seed_ioi_config(host: &Host, contest_id: i32, config: serde_json::Value) {
        host.config
            .seed("contest", &contest_id.to_string(), "contest", config);
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
                    contest_phase: None,
                },
                QueryResource {
                    kind: "submission".to_string(),
                    id: "7".to_string(),
                    contest_id: None, // standalone submission, no contest.
                    problem_id: None,
                    contest_phase: None,
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
        seed_row(&host, 7, 2, "icpc");

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
        // No row at all for the submission id: fail hidden rather than leak.
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
                let mut want = vec![SUBMISSION_STATUS_PATH.to_string()];
                want.sort();
                assert_eq!(got, want);
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_fails_hidden_even_for_a_viewer_who_would_be_the_owner_when_the_row_is_missing()
     {
        // `decide_visibility` is handed only a resource id, so it has no
        // `user_id` to compare until the batched query returns a row. With no
        // row there is nothing to compare the viewer against, and the
        // missing-row branch fails hidden even for a viewer who would be the
        // owner. This can only over-hide (Redact), never leak (Allow).
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
        seed_row(&host, 7, 2, "ioi");

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
        seed_row(&host, 7, 2, "ioi");

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
        seed_row(&host, 7, 2, "ioi");

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
        seed_row(&host, 7, 2, "ioi");

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
                let mut want = vec![SUBMISSION_STATUS_PATH.to_string()];
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
        seed_row(&host, 7, 2, "ioi");
        host.storage
            .set(&[(
                "tokens:10:2",
                &serde_json::json!({ "tokened_submission_ids": [7] }).to_string(),
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
        seed_row(&host, 7, 2, "ioi");
        host.storage
            .set(&[(
                "tokens:10:2",
                &serde_json::json!({ "tokened_submission_ids": [999] }).to_string(),
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
        seed_row(&host, 7, 2, "ioi");

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
                let mut want = vec![SUBMISSION_STATUS_PATH.to_string()];
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
        seed_row(&host, 7, 2, "ioi");

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
        seed_row(&host, 7, 2, "ioi");

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
            { "submission_id": 7, "user_id": 2, "contest_type": "ioi" },
            { "submission_id": 8, "user_id": 3, "contest_type": "ioi" },
            { "submission_id": 9, "user_id": 4, "contest_type": "ioi" },
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
                    let mut want = vec![SUBMISSION_STATUS_PATH.to_string()];
                    want.sort();
                    assert_eq!(got, want, "decision {i} mask mismatch");
                }
                other => panic!("decision {i}: expected Redact, got {other:?}"),
            }
        }
    }
}
