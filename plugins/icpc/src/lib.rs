pub mod config;
pub mod evaluate;
pub mod persist;
pub mod standings;

/// Whether a viewer looking at someone ELSE's submission must have its judged
/// outcome hidden. Enforces on the generic GET /submissions endpoints the same
/// two scoreboard-integrity rules `handle_standings` enforces on its own view:
/// - private standings (`public_standings = false`): during the contest a
///   contestant sees only their own results, so other teams' verdicts stay
///   hidden until the contest ends;
/// - scoreboard freeze: while the freeze window is open (and not revealed),
///   any submission made at/after the window start is pending everywhere.
#[cfg(any(target_arch = "wasm32", test))]
fn must_hide_other_submission(
    public_standings: bool,
    freeze_minutes: i32,
    phase: &str,
    revealed: bool,
    duration_ms: i64,
    now_elapsed_ms: i64,
    submission_elapsed_ms: i64,
) -> bool {
    if (phase == "before" || phase == "during") && !public_standings {
        return true;
    }
    crate::config::freeze_window_open(freeze_minutes, phase, revealed, duration_ms, now_elapsed_ms)
        && submission_elapsed_ms
            >= crate::config::freeze_window_start_ms(freeze_minutes, duration_ms)
}

/// Field mask covering every field `hide_submission_result` used to blank on
/// EITHER host shape, unioned into one list. The host applies a mask with
/// `apply_mask` (`packages/server/src/visibility/mask.rs`), which is a
/// documented no-op on a path that does not exist - a missing key, a type
/// mismatch, or `*` on a non-array never inserts anything. That is what
/// makes one union mask safe to use for both shapes `decide_visibility` is
/// never told apart (list items carry verdict/score/time_used/memory_used at
/// the top level and have no `result` key at all; detail responses nest the
/// same four fields, plus compile_output/error_message/test_case_results,
/// under `result`): whichever shape the host happens to be rendering, only
/// the paths that actually exist in THAT shape are blanked, and the paths
/// belonging to the other shape are silently skipped.
#[cfg(any(target_arch = "wasm32", test))]
fn hidden_result_mask_fields() -> Vec<String> {
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

#[cfg(any(target_arch = "wasm32", test))]
fn current_submissions_sql(contest_id_placeholder: &str, user_filter: &str) -> String {
    format!(
        "SELECT s.id AS submission_id, s.user_id, s.problem_id, \
                sj.status::text AS status, sj.verdict::text AS verdict, \
                EXTRACT(EPOCH FROM (s.created_at - c.start_time)) * 1000 AS elapsed_ms \
         FROM submission s \
         JOIN submission_judgement sj ON sj.submission_id = s.id \
          AND sj.is_current = TRUE AND sj.is_finalized = TRUE \
         JOIN contest c ON c.id = s.contest_id \
         JOIN contest_problem cp ON cp.contest_id = s.contest_id \
          AND cp.problem_id = s.problem_id \
         WHERE s.contest_id = {contest_id_placeholder}{user_filter}"
    )
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    // duration 300min, freeze 60min -> freeze window opens at 240min elapsed.
    const DUR: i64 = 300 * 60_000;
    const NOW_IN_WINDOW: i64 = 250 * 60_000;
    const PRE_FREEZE_SUB: i64 = 100 * 60_000;
    const IN_FREEZE_SUB: i64 = 245 * 60_000;

    #[test]
    fn private_standings_hide_other_teams_during_contest_only() {
        // during the contest: hidden regardless of the freeze.
        assert!(must_hide_other_submission(
            false,
            0,
            "during",
            false,
            DUR,
            PRE_FREEZE_SUB,
            50 * 60_000
        ));
        assert!(must_hide_other_submission(
            false, 0, "before", false, DUR, 0, 0
        ));
        // after the contest the standings open up (no freeze configured).
        assert!(!must_hide_other_submission(
            false,
            0,
            "after",
            false,
            DUR,
            400 * 60_000,
            50 * 60_000
        ));
    }

    #[test]
    fn public_standings_show_pre_freeze_verdicts() {
        assert!(!must_hide_other_submission(
            true,
            60,
            "during",
            false,
            DUR,
            NOW_IN_WINDOW,
            PRE_FREEZE_SUB
        ));
    }

    #[test]
    fn freeze_hides_in_window_submissions_until_revealed() {
        // in the window, not revealed: hidden, through the 'after' phase.
        assert!(must_hide_other_submission(
            true,
            60,
            "during",
            false,
            DUR,
            NOW_IN_WINDOW,
            IN_FREEZE_SUB
        ));
        assert!(must_hide_other_submission(
            true,
            60,
            "after",
            false,
            DUR,
            400 * 60_000,
            IN_FREEZE_SUB
        ));
        // revealed: visible again.
        assert!(!must_hide_other_submission(
            true,
            60,
            "after",
            true,
            DUR,
            400 * 60_000,
            IN_FREEZE_SUB
        ));
        // before the window opens nothing is frozen.
        assert!(!must_hide_other_submission(
            true,
            60,
            "during",
            false,
            DUR,
            100 * 60_000,
            PRE_FREEZE_SUB
        ));
    }

    #[test]
    fn hidden_result_mask_covers_every_list_shape_field_hide_used_to_blank() {
        // Old `hide_submission_result` list-item branch: verdict, score,
        // time_used, memory_used, and nothing else (status was deliberately
        // left alone so the pending '?' cell still renders).
        let fields = hidden_result_mask_fields();
        for f in ["verdict", "score", "time_used", "memory_used"] {
            assert!(
                fields.contains(&f.to_string()),
                "missing list-shape field {f}"
            );
        }
        assert!(
            !fields.iter().any(|f| f == "status"),
            "status must stay visible"
        );
    }

    #[test]
    fn hidden_result_mask_covers_every_detail_shape_field_hide_used_to_blank() {
        // Old `hide_submission_result` detail branch: the same four, plus
        // compile_output/error_message/test_case_results, all nested under
        // `result`.
        let fields = hidden_result_mask_fields();
        for f in [
            "result.verdict",
            "result.score",
            "result.time_used",
            "result.memory_used",
            "result.compile_output",
            "result.error_message",
            "result.test_case_results",
        ] {
            assert!(
                fields.contains(&f.to_string()),
                "missing detail-shape field {f}"
            );
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

    fn seed_freeze_row(
        host: &Host,
        submission_id: i32,
        user_id: i32,
        contest_type: &str,
        phase: &str,
        duration_ms: i64,
        now_elapsed_ms: i64,
        submission_elapsed_ms: i64,
    ) {
        host.db.queue_query_result(serde_json::json!([{
            "submission_id": submission_id,
            "user_id": user_id,
            "contest_type": contest_type,
            "phase": phase,
            "duration_ms": duration_ms as f64,
            "now_elapsed_ms": now_elapsed_ms as f64,
            "submission_elapsed_ms": submission_elapsed_ms as f64,
        }]));
    }

    #[test]
    fn decide_visibility_redacts_the_exact_hide_submission_result_field_set_for_a_frozen_peer_submission()
     {
        let host = Host::mock();
        host.config.seed(
            "contest",
            "10",
            "contest",
            serde_json::json!({
                "public_standings": true,
                "freeze_minutes": 60,
            }),
        );
        seed_freeze_row(
            &host,
            7,
            2,
            "icpc",
            "during",
            DUR,
            NOW_IN_WINDOW,
            IN_FREEZE_SUB,
        );

        let req = VisibilityQueryInput {
            subject: subject(Some(99)), // viewer is NOT the submission owner (2).
            action: "view".to_string(),
            context: QueryContext {
                contest_id: Some(10),
            },
            resources: vec![submission_resource(7, 10)],
        };
        let decisions = decide_visibility_decisions(&host, &req).unwrap();
        assert_eq!(decisions.len(), 1);
        match &decisions[0] {
            WireDecision::Redact { fields } => {
                let mut got = fields.clone();
                got.sort();
                let mut want = hidden_result_mask_fields();
                want.sort();
                assert_eq!(
                    got, want,
                    "decide_visibility must redact EXACTLY what hide_submission_result blanked"
                );
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn decide_visibility_allows_the_owner_even_while_frozen() {
        let host = Host::mock();
        host.config.seed(
            "contest",
            "10",
            "contest",
            serde_json::json!({
                "public_standings": true,
                "freeze_minutes": 60,
            }),
        );
        seed_freeze_row(
            &host,
            7,
            2,
            "icpc",
            "during",
            DUR,
            NOW_IN_WINDOW,
            IN_FREEZE_SUB,
        );

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
    fn decide_visibility_allows_a_submission_from_a_non_icpc_contest() {
        // Every visibility plugin is queried for every resource regardless of
        // contest type - ICPC must default to Allow rather than incorrectly
        // hiding another contest type's submissions.
        let host = Host::mock();
        seed_freeze_row(
            &host,
            7,
            2,
            "ioi",
            "during",
            DUR,
            NOW_IN_WINDOW,
            IN_FREEZE_SUB,
        );

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
    fn decide_visibility_issues_exactly_one_batched_query_for_the_whole_batch() {
        // The critical N+1 guard: a batch of MANY submissions must cost ONE
        // query, not one query per submission.
        let host = Host::mock();
        host.config.seed(
            "contest",
            "10",
            "contest",
            serde_json::json!({
                "public_standings": true,
                "freeze_minutes": 60,
            }),
        );
        host.db.queue_query_result(serde_json::json!([
            {
                "submission_id": 7, "user_id": 2, "contest_type": "icpc",
                "phase": "during", "duration_ms": DUR as f64,
                "now_elapsed_ms": NOW_IN_WINDOW as f64,
                "submission_elapsed_ms": IN_FREEZE_SUB as f64,
            },
            {
                "submission_id": 8, "user_id": 3, "contest_type": "icpc",
                "phase": "during", "duration_ms": DUR as f64,
                "now_elapsed_ms": NOW_IN_WINDOW as f64,
                "submission_elapsed_ms": PRE_FREEZE_SUB as f64,
            },
            {
                "submission_id": 9, "user_id": 4, "contest_type": "icpc",
                "phase": "during", "duration_ms": DUR as f64,
                "now_elapsed_ms": NOW_IN_WINDOW as f64,
                "submission_elapsed_ms": PRE_FREEZE_SUB as f64,
            },
        ]));

        let req = VisibilityQueryInput {
            subject: subject(Some(99)),
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
    }

    #[test]
    fn decide_visibility_fails_hidden_on_contest_submission_mismatch() {
        // No row at all for the submission id: the old code's "fail hidden
        // rather than leak" branch.
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
        assert!(matches!(decisions[0], WireDecision::Redact { .. }));
    }
}

#[cfg(test)]
mod sql_tests {
    use super::*;

    #[test]
    fn current_submission_query_can_be_restricted_to_visible_user_and_contest_problems() {
        let sql = current_submissions_sql("$1", " AND s.user_id = $2");

        assert!(
            sql.contains("JOIN contest_problem cp"),
            "query should join contest_problem to avoid scanning unrelated problem rows: {sql}"
        );
        assert!(
            sql.contains("AND s.user_id = $2"),
            "query should be restrictable to the visible contestant row: {sql}"
        );
    }
}

#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::permissions as perm;
#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
#[cfg(any(target_arch = "wasm32", test))]
use serde::Deserialize;
#[cfg(target_arch = "wasm32")]
use serde::Serialize;

#[cfg(any(target_arch = "wasm32", test))]
use crate::config::ContestConfig;
#[cfg(target_arch = "wasm32")]
use crate::config::{ProblemState, freeze_view, freeze_window_start_ms};
#[cfg(target_arch = "wasm32")]
use crate::evaluate::{
    evaluate_short_circuit_detached, handle_detached_eval_callback, recover_detached_callback_error,
};
#[cfg(target_arch = "wasm32")]
use crate::standings::{
    StandingsSubmission, compute_first_solvers, compute_pending, compute_problem_states,
    counts_as_live,
};

// -- Plugin entry points -------------------------------------------------

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();
    host.registry.register_contest_type(
        "icpc",
        "handle_icpc_submission",
        "handle_icpc_code_run",
    )?;
    host.log.info("ICPC contest plugin registered")?;
    Ok("ok".into())
}

// -- Submission visibility decisions -------------------------------------
//
// Invoked by the host's visibility kernel (`[[server.queries]] topic =
// "visibility"`) for every resource in a query batch, for EVERY plugin
// registered on that topic - not just ICPC contests, and not just
// submissions. `decide_visibility_decisions` therefore defaults to Allow for
// anything it has no opinion about (kind != "submission", no contest_id, or
// a submission belonging to a non-ICPC contest): Allow is the identity
// element of the host's `Decision::meet` lattice, so it can never widen what
// the host or another plugin already decided.
//
// Replaces the old `filter_submission_for_viewer` host-fn hook. Enforces on
// the generic GET /submissions endpoints (list, detail, judgement history)
// the SAME two scoreboard-integrity rules `handle_standings` enforces on its
// own view - private standings during the contest, and the scoreboard
// freeze - via a `Redact` field mask instead of a host-fn-supplied JSON
// mutation. `must_hide_other_submission` is the same predicate that backed
// the old mechanism, unchanged; only the output changed shape.

/// Row shape for the batched freeze/private-standings query below: one row
/// per submission resource in the batch, keyed by submission id.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Deserialize)]
struct SubmissionFreezeRow {
    submission_id: i32,
    user_id: i32,
    contest_type: Option<String>,
    phase: String,
    duration_ms: Option<f64>,
    now_elapsed_ms: Option<f64>,
    submission_elapsed_ms: Option<f64>,
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn decide_visibility(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: VisibilityQueryInput = serde_json::from_str(&input)?;
    let decisions = decide_visibility_decisions(&host, &req)?;
    Ok(serde_json::to_string(&VisibilityQueryOutput { decisions })?)
}

/// Core decision logic. Exercised directly by tests via `Host::mock()` (no
/// wasm32 target required) - there is no e2e test pinning ICPC's freeze
/// behaviour, so these unit tests are the evidence it still works.
#[cfg(any(target_arch = "wasm32", test))]
fn decide_visibility_decisions(
    host: &Host,
    req: &VisibilityQueryInput,
) -> Result<Vec<WireDecision>, SdkError> {
    // Admin / view-all bypass, for the whole batch at once - it does not
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

    // ONE batched query for every submission id in the batch - never one
    // query per submission, which would reintroduce an N+1 on the
    // submission-list hot path. `contest_type` is returned (rather than
    // filtered in the WHERE clause) so a submission belonging to a non-ICPC
    // contest can be told apart from a genuine contest/submission data
    // mismatch: the former must Allow (this plugin has no opinion on
    // non-ICPC contests), the latter fails hidden like the old per-submission
    // query's `None` branch did.
    let mut p = Params::new();
    let placeholders: Vec<String> = submission_ids.iter().map(|id| p.bind(*id)).collect();
    let sql = format!(
        "SELECT s.id AS submission_id, s.user_id, c.contest_type, \
                CASE WHEN NOW() < c.start_time THEN 'before' \
                     WHEN NOW() > c.end_time THEN 'after' \
                     ELSE 'during' END AS phase, \
                EXTRACT(EPOCH FROM (c.end_time - c.start_time)) * 1000 AS duration_ms, \
                EXTRACT(EPOCH FROM (NOW() - c.start_time)) * 1000 AS now_elapsed_ms, \
                EXTRACT(EPOCH FROM (s.created_at - c.start_time)) * 1000 AS submission_elapsed_ms \
         FROM submission s \
         JOIN contest c ON c.id = s.contest_id \
         WHERE s.id IN ({})",
        placeholders.join(",")
    );
    let rows: Vec<SubmissionFreezeRow> = host.db.query_with_args(&sql, &p.into_args())?;
    let rows_by_id: HashMap<i32, SubmissionFreezeRow> =
        rows.into_iter().map(|r| (r.submission_id, r)).collect();

    // Distinct ICPC contest ids actually present in the batch: config and
    // the reveal flag are loaded once per DISTINCT contest, never once per
    // submission.
    let mut icpc_contest_ids: Vec<i32> = req
        .resources
        .iter()
        .zip(&by_index)
        .filter_map(|(resource, id)| {
            let sub_id = (*id)?;
            let row = rows_by_id.get(&sub_id)?;
            if row.contest_type.as_deref() == Some("icpc") {
                resource.contest_id
            } else {
                None
            }
        })
        .collect();
    icpc_contest_ids.sort_unstable();
    icpc_contest_ids.dedup();

    let mut configs: HashMap<i32, ContestConfig> = HashMap::new();
    let mut revealed: HashMap<i32, bool> = HashMap::new();
    for contest_id in icpc_contest_ids {
        configs.insert(contest_id, contest::load_config(host, contest_id)?);
        // Same fail-frozen reveal handling as handle_standings: a storage
        // read error is swallowed to `revealed = false` - never accidentally
        // unfreeze.
        let is_revealed = host
            .storage
            .get_one(&format!("reveal:{contest_id}"))
            .ok()
            .flatten()
            .as_deref()
            == Some("1");
        revealed.insert(contest_id, is_revealed);
    }

    let viewer_id = req.subject.user_id;
    let mask_fields = hidden_result_mask_fields();

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
                return WireDecision::Redact {
                    fields: mask_fields.clone(),
                };
            };
            if row.contest_type.as_deref() != Some("icpc") {
                // Not an ICPC contest: this plugin has no opinion.
                return WireDecision::Allow {};
            }
            // A team always sees its own results, even while frozen.
            if viewer_id == Some(row.user_id) {
                return WireDecision::Allow {};
            }
            let Some(contest_id) = resource.contest_id else {
                return WireDecision::Allow {};
            };
            let config = configs.get(&contest_id).cloned().unwrap_or_default();
            let is_revealed = revealed.get(&contest_id).copied().unwrap_or(false);
            if must_hide_other_submission(
                config.public_standings,
                config.freeze_minutes,
                &row.phase,
                is_revealed,
                row.duration_ms.unwrap_or(0.0) as i64,
                row.now_elapsed_ms.unwrap_or(0.0) as i64,
                row.submission_elapsed_ms.unwrap_or(0.0).max(0.0) as i64,
            ) {
                WireDecision::Redact {
                    fields: mask_fields.clone(),
                }
            } else {
                WireDecision::Allow {}
            }
        })
        .collect();

    Ok(decisions)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_icpc_submission(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: OnSubmissionInput = serde_json::from_str(&input)?;

    let output = match req.contest_id {
        None => run_standalone_judge(&host, &req),
        Some(contest_id) => {
            host.log.info(&format!(
                "ICPC: Judging submission {} for problem {} in contest {}",
                req.submission_id, req.problem_id, contest_id
            ))?;
            match run_judge(&host, &req) {
                Ok(out) => out,
                Err(SdkError::StaleEpoch) => OnSubmissionOutput {
                    success: true,
                    error_message: None,
                },
                Err(e) => OnSubmissionOutput {
                    success: false,
                    error_message: Some(format!("{e:?}")),
                },
            }
        }
    };
    Ok(serde_json::to_string(&output)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_icpc_code_run(input: String) -> FnResult<String> {
    let host = Host::new();
    Ok(broccoli_server_sdk::evaluator::handle_code_run(
        &host, &input,
    )?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn on_icpc_eval_result(input: String) -> FnResult<String> {
    let host = Host::new();
    let input: DetachedEvaluateCallbackInput = serde_json::from_str(&input)?;
    // Snapshot the session state so a mid-stream persist failure can still
    // resolve the submission instead of stranding it. Mirrors the graceful
    // error handling that `run_judge` gives the initial-judge path: without
    // this, the bare `?` below would abort the WASM callback on any error
    // (including a benign StaleEpoch from a concurrent rejudge), leaving the
    // submission stuck on "Judging" with no terminal verdict.
    let state_snapshot = input.state.clone();
    let output = match handle_detached_eval_callback(&host, input) {
        Ok(out) => out,
        Err(SdkError::StaleEpoch) => {
            // A newer judgement superseded this session - stop cleanly. The
            // newer epoch already owns the submission; nothing to finalize.
            let _ = host
                .log
                .info("ICPC: detached callback epoch stale, cancelling");
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
        Err(e) => {
            // Transient persist failure mid-stream. Funnel the submission to a
            // terminal SystemError rather than leaving it on "Judging".
            let _ = host.log.info(&format!(
                "ICPC: detached callback failed ({e:?}); finalizing as SystemError"
            ));
            recover_detached_callback_error(&host, &state_snapshot);
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
    };
    Ok(serde_json::to_string(&output)?)
}

// -- Core judging logic --------------------------------------------------

#[cfg(target_arch = "wasm32")]
fn run_judge(host: &Host, req: &OnSubmissionInput) -> Result<OnSubmissionOutput, SdkError> {
    let test_cases = req.test_cases.clone();

    if test_cases.is_empty() {
        let _ = host
            .log
            .info("ICPC: No test cases found; marking as SystemError (not a solve)");
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: req.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            // A problem with NO test cases is a misconfiguration, not a solve.
            // Marking it Accepted would credit EVERY team a free solve on the ICPC
            // standings (compute_problem_states counts any Accepted verdict) and
            // hand the earliest submitter a first-solve balloon. Surface it as a
            // SystemError so it is visible and never counts as solved.
            verdict: Some(Some(Verdict::SystemError)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            compile_output: None,
            error_code: Some(Some("NO_TEST_CASES".to_string())),
            error_message: Some(Some(
                "Problem has no test cases; cannot be judged".to_string(),
            )),
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }
        return Ok(OnSubmissionOutput {
            success: true,
            error_message: None,
        });
    }

    match evaluate_short_circuit_detached(host, req, &test_cases, req.submission_id) {
        Ok(out) => Ok(out),
        Err(SdkError::StaleEpoch) => {
            let _ = host.log.info(&format!(
                "ICPC: Submission {} epoch {} is stale, stopping",
                req.submission_id, req.judge_epoch
            ));
            return Ok(OnSubmissionOutput {
                success: true,
                error_message: None,
            });
        }
        Err(e) => return Err(e),
    }
}

#[cfg(target_arch = "wasm32")]
fn run_standalone_judge(host: &Host, req: &OnSubmissionInput) -> OnSubmissionOutput {
    let _ = host.log.info(&format!(
        "ICPC: Judging standalone submission {} for problem {}",
        req.submission_id, req.problem_id
    ));

    if req.test_cases.is_empty() {
        return persist_empty_standalone(host, req).unwrap_or_else(|e| match e {
            SdkError::StaleEpoch => OnSubmissionOutput {
                success: true,
                error_message: None,
            },
            other => OnSubmissionOutput {
                success: false,
                error_message: Some(format!("{other:?}")),
            },
        });
    }

    match evaluate_short_circuit_detached(host, req, &req.test_cases, req.submission_id) {
        Ok(out) => out,
        Err(SdkError::StaleEpoch) => OnSubmissionOutput {
            success: true,
            error_message: None,
        },
        Err(e) => OnSubmissionOutput {
            success: false,
            error_message: Some(format!("{e:?}")),
        },
    }
}

#[cfg(target_arch = "wasm32")]
fn persist_empty_standalone(
    host: &Host,
    req: &OnSubmissionInput,
) -> Result<OnSubmissionOutput, SdkError> {
    let _ = host
        .log
        .info("ICPC: No test cases found, marking standalone submission as judged with score 0");
    let affected = host.submission.update(&SubmissionUpdate {
        submission_id: req.submission_id,
        judgement_id: req.judgement_id,
        judge_epoch: req.judge_epoch,
        status: Some(SubmissionStatus::Judged),
        verdict: Some(Some(Verdict::Accepted)),
        score: Some(0.0),
        time_used: Some(None),
        memory_used: Some(None),
        compile_output: None,
        error_code: None,
        error_message: None,
    })?;

    if affected == 0 {
        return Err(SdkError::StaleEpoch);
    }

    Ok(OnSubmissionOutput {
        success: true,
        error_message: None,
    })
}

// -- API: GET /contests/{contest_id}/info --------------------------------

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_contest_info(input: String) -> FnResult<String> {
    run_api_handler(&input, handle_contest_info)
}

#[cfg(target_arch = "wasm32")]
fn handle_contest_info(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("icpc")?;
    let config: ContestConfig = contest::load_config(host, contest_id)?;

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({
            "penalty_minutes": config.penalty_minutes,
            "count_compile_error": config.count_compile_error,
            "show_test_details": config.show_test_details,
        })),
    })
}

// -- API: GET /contests/{contest_id}/standings ---------------------------

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_standings(input: String) -> FnResult<String> {
    run_api_handler(&input, handle_standings)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_reveal(input: String) -> FnResult<String> {
    run_api_handler(&input, handle_reveal)
}

/// Contest (duration_ms, now_elapsed_ms) in ms since start. Computed the same way
/// as a submission's `elapsed_ms` (float epoch * 1000, truncated) so the freeze
/// boundary compares like-with-like. Non-negative in the 'during'/'after' phases.
#[cfg(target_arch = "wasm32")]
fn query_contest_times(host: &Host, contest_id: i32) -> Result<(i64, i64), ApiError> {
    #[derive(Deserialize)]
    struct ContestTimes {
        duration_ms: Option<f64>,
        now_elapsed_ms: Option<f64>,
    }
    let mut p = Params::new();
    let sql = format!(
        "SELECT EXTRACT(EPOCH FROM (end_time - start_time)) * 1000 AS duration_ms, \
                EXTRACT(EPOCH FROM (NOW() - start_time)) * 1000 AS now_elapsed_ms \
         FROM contest WHERE id = {}",
        p.bind(contest_id)
    );
    let times: Option<ContestTimes> = host.db.query_one_with_args(&sql, &p.into_args())?;
    Ok((
        times.as_ref().and_then(|t| t.duration_ms).unwrap_or(0.0) as i64,
        times.as_ref().and_then(|t| t.now_elapsed_ms).unwrap_or(0.0) as i64,
    ))
}

/// Manually reveal (unfreeze) the scoreboard. Organizer-only, and only once the
/// freeze window has actually opened (or the contest ended) so a premature click
/// cannot silently disable the freeze. Sets a persistent per-contest flag that
/// `handle_standings` checks. Idempotent.
#[cfg(target_arch = "wasm32")]
fn handle_reveal(host: &Host, req: &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("icpc")?;
    if !req.has_permission(perm::CONTEST_MANAGE) {
        return Err(PluginHttpResponse::error(
            403,
            "Revealing the scoreboard requires contest:manage",
        )
        .into());
    }
    let config: ContestConfig = contest::load_config(host, contest_id)?;
    let phase = info.phase.as_str();
    let mut window_open = false;
    if config.freeze_minutes > 0 && (phase == "during" || phase == "after") {
        let (duration_ms, now_elapsed_ms) = query_contest_times(host, contest_id)?;
        window_open = now_elapsed_ms >= freeze_window_start_ms(config.freeze_minutes, duration_ms);
    }
    if !window_open {
        return Err(PluginHttpResponse::error(
            409,
            "The scoreboard freeze window has not opened yet; nothing to reveal",
        )
        .into());
    }
    let key = format!("reveal:{contest_id}");
    host.storage.set(&[(key.as_str(), "1")])?;
    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({ "revealed": true })),
    })
}

#[cfg(target_arch = "wasm32")]
fn handle_standings(host: &Host, req: &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("icpc")?;
    let config: ContestConfig = contest::load_config(host, contest_id)?;

    // Fetch contest problems in order
    #[derive(Deserialize)]
    struct ContestProblem {
        problem_id: i32,
        label: Option<String>,
    }
    let mut p = Params::new();
    let sql = format!(
        "SELECT problem_id, label FROM contest_problem WHERE contest_id = {} ORDER BY position",
        p.bind(contest_id)
    );
    let problems: Vec<ContestProblem> = host.db.query_with_args(&sql, &p.into_args())?;

    // Build problem labels: use explicit label if set, otherwise A, B, C...
    let problem_labels: Vec<String> = problems
        .iter()
        .enumerate()
        .map(|(i, p)| {
            p.label
                .as_deref()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .unwrap_or_else(|| {
                    // A, B, C, ... Z, AA, AB, ...
                    let c = (b'A' + (i as u8) % 26) as char;
                    if i < 26 {
                        c.to_string()
                    } else {
                        format!("{}{}", (b'A' + (i as u8) / 26 - 1) as char, c)
                    }
                })
        })
        .collect();
    let problem_ids: Vec<i32> = problems.iter().map(|p| p.problem_id).collect();

    // Fetch participants (during before/during phase, only fetch the requesting user
    // unless they have contest:manage so organizers can supervise live scoring).
    #[derive(Deserialize)]
    struct Participant {
        user_id: i32,
        username: String,
    }
    let phase = &info.phase;
    let can_view_all = req.has_permission(perm::CONTEST_MANAGE);
    // Restrict a contestant to their own row during the contest UNLESS the
    // organizer opted into a public live scoreboard. Organizers always see all.
    let is_restricted =
        (phase == "before" || phase == "during") && !can_view_all && !config.public_standings;
    let restricted_user_id = if is_restricted {
        match req.user_id() {
            Some(uid) => Some(uid),
            None => {
                return Ok(PluginHttpResponse {
                    status: 200,
                    headers: None,
                    body: Some(serde_json::json!({
                        "phase": phase,
                        "penalty_minutes": config.penalty_minutes,
                        "problem_labels": problem_labels,
                        "rows": [],
                    })),
                });
            }
        }
    } else {
        None
    };

    // Freeze: in the final `freeze_minutes` a contestant's board stops updating and
    // submissions during the window show as pending "?". Organizers always see the
    // real board. Manual reveal: once an organizer reveals, the board unfreezes for
    // everyone; until then the freeze holds through the contest end ('after'). A
    // storage read error is swallowed to `revealed = false` - fail frozen, never
    // accidentally unfreeze.
    let reveal_flag = host
        .storage
        .get_one(&format!("reveal:{contest_id}"))
        .ok()
        .flatten();
    let revealed = reveal_flag.as_deref() == Some("1");
    // Query contest times whenever a freeze could be relevant - a contestant's
    // frozen split OR an organizer's reveal gating both need the window.
    let freeze_possible =
        config.freeze_minutes > 0 && !revealed && (phase == "during" || phase == "after");
    let (duration_ms, now_elapsed_ms) = if freeze_possible {
        query_contest_times(host, contest_id)?
    } else {
        (0, 0)
    };
    let fview = freeze_view(
        config.freeze_minutes,
        phase,
        can_view_all,
        revealed,
        duration_ms,
        now_elapsed_ms,
    );
    let freeze_start_ms = fview.freeze_start_ms;
    let frozen = fview.frozen;

    let mut p = Params::new();
    let user_filter = if let Some(uid) = restricted_user_id {
        format!(" AND cu.user_id = {}", p.bind(uid))
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT cu.user_id, u.username \
         FROM contest_user cu \
         JOIN \"user\" u ON u.id = cu.user_id \
         WHERE cu.contest_id = {}{user_filter} \
         ORDER BY cu.registered_at ASC",
        p.bind(contest_id)
    );
    let participants: Vec<Participant> = host.db.query_with_args(&sql, &p.into_args())?;

    #[derive(Deserialize)]
    struct CurrentSubmission {
        submission_id: i32,
        user_id: i32,
        problem_id: i32,
        status: String,
        verdict: Option<Verdict>,
        elapsed_ms: Option<f64>,
    }
    let mut p = Params::new();
    let contest_id_placeholder = p.bind(contest_id);
    let submission_user_filter = if let Some(uid) = restricted_user_id {
        format!(" AND s.user_id = {}", p.bind(uid))
    } else {
        String::new()
    };
    let sql = current_submissions_sql(&contest_id_placeholder, &submission_user_filter);
    let current_submissions: Vec<CurrentSubmission> =
        host.db.query_with_args(&sql, &p.into_args())?;
    let standings_submissions: Vec<StandingsSubmission> = current_submissions
        .into_iter()
        .map(|row| StandingsSubmission {
            submission_id: row.submission_id,
            user_id: row.user_id,
            problem_id: row.problem_id,
            verdict: row.verdict,
            status: row.status,
            elapsed_ms: row.elapsed_ms.unwrap_or(0.0).max(0.0) as i64,
        })
        .collect();
    // Freeze split: rank/solved/penalty come from the LIVE (pre-freeze) set only;
    // other teams' during-freeze submissions become pending markers. A contestant
    // always sees their OWN submissions un-frozen (real verdicts on their row), so
    // only OTHER teams are hidden. When not frozen, freeze_start_ms = i64::MAX so
    // everything is live and this is a no-op.
    let own_uid = req.user_id();
    let (pre_freeze, during_freeze): (Vec<StandingsSubmission>, Vec<StandingsSubmission>) =
        standings_submissions
            .into_iter()
            .partition(|s| counts_as_live(s, freeze_start_ms, own_uid));
    let all_states = compute_problem_states(&pre_freeze, config.count_compile_error);
    let pending_map = compute_pending(&during_freeze, &all_states);
    // First-to-solve balloon owner per problem. Global by nature, so it is
    // suppressed (empty) in the restricted private-standings view, which only
    // loaded the viewer's own submissions - see `compute_first_solvers`.
    let first_solvers = compute_first_solvers(&all_states, restricted_user_id.is_some());

    // Build entries
    #[derive(Serialize)]
    struct ProblemCell {
        attempts: i32,
        solved: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        time: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        penalty: Option<i32>,
        #[serde(skip_serializing_if = "Option::is_none")]
        first_solve: Option<bool>,
        /// Submissions made during the freeze on a not-yet-solved problem; the
        /// frontend shows "?" instead of the verdict. Absent when not frozen.
        #[serde(skip_serializing_if = "Option::is_none")]
        pending: Option<i32>,
    }

    #[derive(Serialize)]
    struct StandingsEntry {
        rank: usize,
        user_id: i32,
        username: String,
        solved: i32,
        penalty: i32,
        problems: HashMap<String, ProblemCell>,
    }

    let mut entries: Vec<StandingsEntry> = Vec::new();

    for participant in &participants {
        let mut solved = 0;
        let mut total_penalty = 0;
        let mut problem_cells = HashMap::new();

        for (i, &pid) in problem_ids.iter().enumerate() {
            let state: ProblemState = all_states
                .get(&(participant.user_id, pid))
                .cloned()
                .unwrap_or_default();

            let label = &problem_labels[i];
            let pending = pending_map.get(&(participant.user_id, pid)).copied();

            if state.solved {
                solved += 1;
                let pen = state.penalty_minutes(config.penalty_minutes);
                total_penalty += pen;
                let time_min = state.solve_time_ms.unwrap_or(0).div_euclid(60_000) as i32;

                problem_cells.insert(
                    label.clone(),
                    ProblemCell {
                        attempts: state.attempts,
                        solved: true,
                        time: Some(time_min),
                        penalty: Some(pen),
                        first_solve: None, // filled in second pass
                        pending: None,
                    },
                );
            } else if state.attempts > 0 || pending.is_some() {
                // Not solved before the freeze: show pre-freeze wrong attempts, plus
                // a pending "?" marker if there are frozen submissions.
                problem_cells.insert(
                    label.clone(),
                    ProblemCell {
                        attempts: state.attempts,
                        solved: false,
                        time: None,
                        penalty: None,
                        first_solve: None,
                        pending,
                    },
                );
            }
            // No attempts and nothing pending -> empty cell (not inserted).
        }

        entries.push(StandingsEntry {
            rank: 0,
            user_id: participant.user_id,
            username: participant.username.clone(),
            solved,
            penalty: total_penalty,
            problems: problem_cells,
        });
    }

    // Mark first solves (empty map in the restricted view -> no balloons).
    for entry in &mut entries {
        for (i, &pid) in problem_ids.iter().enumerate() {
            let label = &problem_labels[i];
            if let Some(cell) = entry.problems.get_mut(label)
                && cell.solved
                && first_solvers.get(&pid) == Some(&entry.user_id)
            {
                cell.first_solve = Some(true);
            }
        }
    }

    // Sort: solved DESC, penalty ASC, username ASC
    entries.sort_by(|a, b| {
        b.solved
            .cmp(&a.solved)
            .then_with(|| a.penalty.cmp(&b.penalty))
            .then_with(|| a.username.cmp(&b.username))
    });

    // Assign ranks (ties get same rank)
    for i in 0..entries.len() {
        if i > 0
            && entries[i].solved == entries[i - 1].solved
            && entries[i].penalty == entries[i - 1].penalty
        {
            entries[i].rank = entries[i - 1].rank;
        } else {
            entries[i].rank = i + 1;
        }
    }

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({
            "phase": phase,
            "frozen": frozen,
            "revealed": revealed,
            // Organizer-only Reveal button - shown only while the board is actually
            // frozen (window open, not yet revealed), so it can't disable the freeze early.
            "can_reveal": fview.can_reveal,
            "penalty_minutes": config.penalty_minutes,
            "problem_labels": problem_labels,
            "rows": entries,
        })),
    })
}
