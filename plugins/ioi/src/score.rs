// Every item below is reachable only from wasm32-gated production code
// and/or the #[cfg(test)] unit tests at the bottom of this file; this file
// has no top-level (unconditional) use of the SDK prelude, HashMap, or serde
// derive macros, so gate these imports the same way a native (test/clippy)
// build doesn't see them as unused.
#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::prelude::*;
#[cfg(any(target_arch = "wasm32", test))]
use serde::{Deserialize, Serialize};

// Only reachable from the wasm32-gated `run_judge`/`compute_official_task_score`
// below; this file has no #[cfg(test)] use of these three.
#[cfg(target_arch = "wasm32")]
use crate::config::{ContestConfig, ScoringMode, TaskConfig};
// Reachable from wasm32-gated production code AND directly from the
// #[cfg(test)] unit tests below (`score_submission_subtask_details`, which
// they call directly, and `SubtaskDef` literals they construct); gate the
// same way so a native (test/clippy) build doesn't see these as unused.
#[cfg(any(target_arch = "wasm32", test))]
use crate::config::{SubtaskDef, resolve_tc_label, round_score};
#[cfg(target_arch = "wasm32")]
use crate::judge::{JudgeContext, judge_with_context_detached};
// Only used by the wasm32-gated `compute_official_task_score` below.
#[cfg(target_arch = "wasm32")]
use crate::scoring::score_best_tokened_or_last;
// Used by `sum_best_subtask_score` below, which is reachable from the
// #[cfg(test)] unit tests as well as from wasm32 - so this import has to be
// visible under `test` too, hence the split from the wasm32-only import above.
#[cfg(any(target_arch = "wasm32", test))]
use crate::scoring::score_sum_best_subtask;
// `build_default_subtasks` is reachable from the wasm32-gated `run_judge`
// below AND directly from the #[cfg(test)] unit tests; `score_all_subtasks`
// is reachable from both `score_submission_subtask_details` and
// `sum_best_subtask_score`, which are themselves wasm32+test. Gate both the
// same way.
#[cfg(any(target_arch = "wasm32", test))]
use crate::subtasks::{build_default_subtasks, score_all_subtasks};
#[cfg(target_arch = "wasm32")]
use crate::{load_effective_subtasks, load_task_config, load_token_state};

// Only constructed by the wasm32-gated `compute_official_task_score` below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct MaxScore {
    max_score: Option<f64>,
}

// Constructed by the wasm32-gated `load_current_submission_test_case_results`/
// `recompute_sum_best_subtask` below AND directly by the #[cfg(test)] unit
// tests, which build rows by hand instead of querying a mock DB.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Deserialize)]
pub(crate) struct TcResultRow {
    #[allow(dead_code)]
    submission_id: i32,
    test_case_id: i32,
    score: f64,
    verdict: Verdict,
}

/// Normalize a persisted per-test result back to a 0..1 raw score for subtask
/// RECOMPUTE. The stored `score` is the point-WEIGHTED value `round(raw * tc_max)`,
/// which is LOSSY: a passing zero-point member (`tc_max == 0`) stores 0, and a
/// non-2-decimal weight rounds a full pass slightly below 1.0. Re-deriving raw as
/// `score / tc_max` would therefore wrongly FAIL a GroupMin/GroupMul subtask that
/// actually passed (those methods gate on `raw >= 1.0`), diverging from the live
/// judging path (which scores from the true `outcome.score`). Use the persisted
/// verdict for the full-pass signal so recompute matches live scoring.
// Called from the wasm32-gated `recompute_sum_best_subtask` below AND from
// `score_submission_subtask_details` (wasm32+test); gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn normalized_raw_score(verdict: &Verdict, score: f64, tc_max: f64) -> f64 {
    if verdict.is_accepted() {
        1.0
    } else if tc_max > 0.0 {
        (score / tc_max).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

// Only constructed inside `score_submission_subtask_details` below
// (wasm32+test); gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
#[derive(Debug, Clone, Serialize, PartialEq)]
pub(crate) struct SubtaskScoreDetail {
    name: String,
    scoring_method: crate::config::SubtaskScoringMethod,
    score: f64,
    max_score: f64,
}

// Only constructed by the wasm32-gated `recompute_sum_best_subtask` below,
// and used as a type from `api.rs`'s wasm32-gated handlers.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
pub(crate) struct TcMaxScore {
    #[allow(dead_code)]
    test_case_id: i32,
    pub(crate) max_score: f64,
}

// Only constructed by the wasm32-gated `compute_official_task_score` below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct SubmissionScore {
    #[allow(dead_code)]
    id: i32,
    score: f64,
}

// Called by the wasm32-gated `api.rs` subtask-scores handler AND directly by
// the #[cfg(test)] unit tests below; gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn score_submission_subtask_details(
    test_cases: &[TestCaseRow],
    subtask_defs: &[SubtaskDef],
    tc_results: &[TcResultRow],
) -> Vec<SubtaskScoreDetail> {
    let max_map: HashMap<i32, f64> = test_cases.iter().map(|tc| (tc.id, tc.score)).collect();
    let id_to_label: HashMap<i32, String> = test_cases
        .iter()
        .map(|tc| (tc.id, resolve_tc_label(tc)))
        .collect();

    let mut tc_scores = HashMap::new();
    for row in tc_results {
        let Some(label) = id_to_label.get(&row.test_case_id) else {
            continue;
        };
        let tc_max = max_map.get(&row.test_case_id).copied().unwrap_or(0.0);
        let raw_score = normalized_raw_score(&row.verdict, row.score, tc_max);
        tc_scores.insert(label.clone(), raw_score);
    }

    score_all_subtasks(subtask_defs, test_cases, &tc_scores)
        .into_iter()
        .zip(subtask_defs.iter())
        .map(|(score, def)| SubtaskScoreDetail {
            name: score.name,
            scoring_method: def.scoring_method,
            score: round_score(score.score),
            max_score: score.max_score,
        })
        .collect()
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_current_submission_test_case_results(
    host: &Host,
    contest_id: i32,
    submission_id: i32,
) -> Result<Vec<TcResultRow>, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT tcr.submission_id, tcr.test_case_id, tcr.score, tcr.verdict \
         FROM test_case_result tcr \
         JOIN submission s ON s.id = tcr.submission_id \
         JOIN submission_judgement sj \
           ON sj.id = tcr.judgement_id \
          AND sj.submission_id = tcr.submission_id \
          AND sj.judge_epoch = tcr.judge_epoch \
         WHERE tcr.submission_id = {} \
           AND s.contest_id = {} \
           AND tcr.test_case_id IS NOT NULL \
           AND sj.is_current = TRUE AND sj.is_finalized = TRUE",
        p.bind(submission_id),
        p.bind(contest_id)
    );
    host.db.query_with_args(&sql, &p.into_args())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn run_judge(
    host: &Host,
    req: &OnSubmissionInput,
    contest_id: i32,
) -> Result<OnSubmissionOutput, SdkError> {
    let contest_config: ContestConfig = contest::load_config(host, contest_id)?;

    let task_config: TaskConfig = load_task_config(host, contest_id, req.problem_id)?;

    let test_cases = req.test_cases.clone();

    let subtask_defs = if task_config.subtasks.is_empty() {
        build_default_subtasks(&test_cases)
    } else {
        task_config.subtasks.clone()
    };

    let ctx = JudgeContext {
        contest_config: contest_config.clone(),
        task_config: task_config.clone(),
        submission_id: req.submission_id,
        problem_id: req.problem_id,
        contest_id,
        test_cases,
        subtask_defs,
    };

    let result = judge_with_context_detached(host, req, &ctx)?;

    Ok(result.output)
}

#[cfg(target_arch = "wasm32")]
fn recompute_sum_best_subtask(
    host: &Host,
    contest_id: i32,
    problem_id: i32,
    user_id: i32,
    test_cases: &[TestCaseRow],
    subtask_defs: &[SubtaskDef],
) -> Result<f64, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT tcr.submission_id, tcr.test_case_id, tcr.score, tcr.verdict \
         FROM test_case_result tcr \
         JOIN submission s ON s.id = tcr.submission_id \
         JOIN submission_judgement sj \
           ON sj.id = tcr.judgement_id \
          AND sj.submission_id = tcr.submission_id \
          AND sj.judge_epoch = tcr.judge_epoch \
         WHERE s.user_id = {} AND s.problem_id = {} AND s.contest_id = {} \
         AND tcr.test_case_id IS NOT NULL \
         AND sj.is_current = TRUE AND sj.is_finalized = TRUE",
        p.bind(user_id),
        p.bind(problem_id),
        p.bind(contest_id)
    );
    let tc_results: Vec<TcResultRow> = host.db.query_with_args(&sql, &p.into_args())?;

    let mut p = Params::new();
    let sql = format!(
        "SELECT id as test_case_id, score as max_score \
         FROM test_case WHERE problem_id = {}",
        p.bind(problem_id)
    );
    let tc_maxes: Vec<TcMaxScore> = host.db.query_with_args(&sql, &p.into_args())?;
    let max_map: HashMap<i32, f64> = tc_maxes
        .iter()
        .map(|t| (t.test_case_id, t.max_score))
        .collect();

    let id_to_label: HashMap<i32, String> = test_cases
        .iter()
        .map(|tc| (tc.id, resolve_tc_label(tc)))
        .collect();

    let mut by_submission: HashMap<i32, HashMap<String, f64>> = HashMap::new();
    for row in &tc_results {
        let tc_max = max_map.get(&row.test_case_id).copied().unwrap_or(0.0);
        let raw_score = normalized_raw_score(&row.verdict, row.score, tc_max);
        let label = id_to_label
            .get(&row.test_case_id)
            .cloned()
            .unwrap_or_else(|| row.test_case_id.to_string());
        by_submission
            .entry(row.submission_id)
            .or_default()
            .insert(label, raw_score);
    }

    Ok(sum_best_subtask_score(
        subtask_defs,
        test_cases,
        by_submission.values(),
    ))
}

/// The SumBestSubtask score: for each of the user's submissions, score every
/// subtask from its normalized per-test-case scores, then for each subtask take
/// the best across submissions and sum. Single source of truth for both the
/// official task score ([`recompute_sum_best_subtask`]) and the scoreboard cell
/// (`crate::scoreboard`), which differ only in how they fetch + normalize the
/// rows - so the two cannot diverge on the value.
// Called from the wasm32-gated `recompute_sum_best_subtask` above, from
// `scoreboard.rs`'s wasm32-gated sum-best-subtask cell loader, AND directly by
// the #[cfg(test)] unit tests below; gate the same way. The doc comment above
// calls this the single source of truth that keeps the official task score and
// the scoreboard cell from diverging - an invariant worth a host test, which a
// wasm32-only gate would make impossible to write.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn sum_best_subtask_score<'a>(
    subtask_defs: &[SubtaskDef],
    test_cases: &[TestCaseRow],
    per_submission_scores: impl IntoIterator<Item = &'a HashMap<String, f64>>,
) -> f64 {
    let all_subtask_scores: Vec<Vec<f64>> = per_submission_scores
        .into_iter()
        .map(|tc_scores| {
            score_all_subtasks(subtask_defs, test_cases, tc_scores)
                .iter()
                .map(|r| r.score)
                .collect()
        })
        .collect();
    score_sum_best_subtask(&all_subtask_scores)
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn compute_official_task_score(
    host: &Host,
    config: &ContestConfig,
    contest_id: i32,
    problem_id: i32,
    user_id: i32,
    test_cases: Option<&[TestCaseRow]>,
    subtask_defs: Option<&[SubtaskDef]>,
) -> Result<f64, SdkError> {
    match config.scoring_mode {
        ScoringMode::MaxSubmission => {
            let mut p = Params::new();
            let sql = format!(
                "SELECT MAX(sj.score) as max_score \
                 FROM submission s \
                 JOIN submission_judgement sj \
                   ON sj.submission_id = s.id \
                  AND sj.is_current = TRUE \
                  AND sj.judge_epoch = s.judge_epoch \
                 WHERE s.user_id = {} AND s.problem_id = {} AND s.contest_id = {}",
                p.bind(user_id),
                p.bind(problem_id),
                p.bind(contest_id)
            );
            Ok(host
                .db
                .query_one_with_args::<MaxScore>(&sql, &p.into_args())?
                .and_then(|r| r.max_score)
                .unwrap_or(0.0))
        }
        ScoringMode::SumBestSubtask => {
            let owned;
            let (test_cases, subtask_defs) = match (test_cases, subtask_defs) {
                (Some(test_cases), Some(subtask_defs)) => (test_cases, subtask_defs),
                _ => {
                    let task_config = load_task_config(host, contest_id, problem_id)?;
                    owned = load_effective_subtasks(host, problem_id, &task_config)?;
                    (&owned.0[..], &owned.1[..])
                }
            };

            recompute_sum_best_subtask(
                host,
                contest_id,
                problem_id,
                user_id,
                test_cases,
                subtask_defs,
            )
        }
        ScoringMode::BestTokenedOrLast => {
            let token_state = load_token_state(host, contest_id, user_id)?;
            let tokened_best = if token_state.tokened_submission_ids.is_empty() {
                0.0
            } else {
                let mut p = Params::new();
                let ids_sql: Vec<String> = token_state
                    .tokened_submission_ids
                    .iter()
                    .map(|id| p.bind(*id))
                    .collect();
                // `is_finalized = TRUE`: MAX() already skips a fresh in-flight
                // judgement's NULL score, but a rejudging tokened submission can
                // carry a stale non-NULL score on its not-yet-finalized current
                // judgement; the gate keeps only truly-finalized scores in the max.
                let sql = format!(
                    "SELECT MAX(sj.score) as max_score \
                     FROM submission s \
                     JOIN submission_judgement sj \
                       ON sj.submission_id = s.id \
                      AND sj.is_current = TRUE \
                      AND sj.is_finalized = TRUE \
                      AND sj.judge_epoch = s.judge_epoch \
                     WHERE s.id IN ({}) AND s.problem_id = {}",
                    ids_sql.join(","),
                    p.bind(problem_id)
                );
                host.db
                    .query_one_with_args::<MaxScore>(&sql, &p.into_args())?
                    .and_then(|r| r.max_score)
                    .unwrap_or(0.0)
            };

            let mut p = Params::new();
            // `is_finalized = TRUE` so an in-flight/rejudging last submission does
            // not read a COALESCE-0 or stale score for the "last" component of
            // BestTokenedOrLast; the task score then holds its finalized value
            // until the newer submission actually completes. `s.id DESC` breaks a
            // same-created_at tie deterministically. Mirrors the scoreboard query.
            let sql = format!(
                "SELECT s.id, COALESCE(sj.score, 0.0) as score \
                 FROM submission s \
                 JOIN submission_judgement sj \
                   ON sj.submission_id = s.id \
                  AND sj.is_current = TRUE \
                  AND sj.is_finalized = TRUE \
                  AND sj.judge_epoch = s.judge_epoch \
                 WHERE s.user_id = {} AND s.problem_id = {} AND s.contest_id = {} \
                 ORDER BY s.created_at DESC, s.id DESC LIMIT 1",
                p.bind(user_id),
                p.bind(problem_id),
                p.bind(contest_id)
            );
            let last_score = host
                .db
                .query_one_with_args::<SubmissionScore>(&sql, &p.into_args())?
                .map(|r| r.score)
                .unwrap_or(0.0);

            Ok(score_best_tokened_or_last(tokened_best, last_score))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_best_subtask_takes_the_best_per_subtask_across_submissions_not_the_best_submission() {
        // Two subtasks, two submissions, each submission winning a different
        // subtask. The whole point of sum-best-subtask scoring is that a
        // contestant banks their best result on EACH subtask independently:
        // 100 + 100 = 200. Taking the best single submission's total would
        // give 100, and summing everything would give 200 only by accident,
        // so the asymmetric split below distinguishes all three.
        let test_cases = vec![
            TestCaseRow {
                id: 11,
                score: 100.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("a".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
            TestCaseRow {
                id: 12,
                score: 100.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("b".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
        ];
        let subtasks = vec![
            SubtaskDef {
                name: "X".into(),
                scoring_method: crate::config::SubtaskScoringMethod::Sum,
                max_score: 100.0,
                test_cases: vec!["a".into()],
            },
            SubtaskDef {
                name: "Y".into(),
                scoring_method: crate::config::SubtaskScoringMethod::Sum,
                max_score: 100.0,
                test_cases: vec!["b".into()],
            },
        ];

        // Submission 1 aces subtask X and fails Y; submission 2 is the mirror.
        // Values here are per-test-case ratios in [0.0, 1.0], scaled by each
        // test case's own weight - not absolute points.
        let first: HashMap<String, f64> = [("a".to_string(), 1.0), ("b".to_string(), 0.0)]
            .into_iter()
            .collect();
        let second: HashMap<String, f64> = [("a".to_string(), 0.0), ("b".to_string(), 1.0)]
            .into_iter()
            .collect();

        let total = sum_best_subtask_score(&subtasks, &test_cases, [&first, &second]);

        assert_eq!(
            total, 200.0,
            "each subtask's best result across submissions is banked independently"
        );

        // A single submission alone can only earn its own subtask.
        assert_eq!(
            sum_best_subtask_score(&subtasks, &test_cases, [&first]),
            100.0
        );
    }

    #[test]
    fn subtask_detail_scores_are_derived_from_current_test_case_results() {
        let test_cases = vec![
            TestCaseRow {
                id: 11,
                score: 50.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("a".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
            TestCaseRow {
                id: 12,
                score: 50.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("b".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
        ];
        let subtasks = vec![SubtaskDef {
            name: "Current".into(),
            scoring_method: crate::config::SubtaskScoringMethod::Sum,
            max_score: 100.0,
            test_cases: vec!["a".into(), "b".into()],
        }];
        let current_rows = vec![
            TcResultRow {
                submission_id: 1,
                test_case_id: 11,
                score: 50.0,
                verdict: Verdict::Accepted,
            },
            TcResultRow {
                submission_id: 1,
                test_case_id: 12,
                score: 0.0,
                verdict: Verdict::WrongAnswer,
            },
        ];

        let scores = score_submission_subtask_details(&test_cases, &subtasks, &current_rows);

        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].name, "Current");
        assert_eq!(scores[0].score, 50.0);
        assert_eq!(scores[0].max_score, 100.0);
    }

    #[test]
    fn default_subtask_detail_scores_use_test_case_weights() {
        let test_cases = vec![
            TestCaseRow {
                id: 11,
                score: 10.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("small".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
            TestCaseRow {
                id: 12,
                score: 90.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("large".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
        ];
        let subtasks = build_default_subtasks(&test_cases);
        let current_rows = vec![
            TcResultRow {
                submission_id: 1,
                test_case_id: 11,
                score: 10.0,
                verdict: Verdict::Accepted,
            },
            TcResultRow {
                submission_id: 1,
                test_case_id: 12,
                score: 0.0,
                verdict: Verdict::WrongAnswer,
            },
        ];

        let scores = score_submission_subtask_details(&test_cases, &subtasks, &current_rows);

        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].name, "All Tests");
        assert_eq!(scores[0].score, 10.0);
        assert_eq!(scores[0].max_score, 100.0);
    }

    /// Regression: a fully-passing GroupMin subtask whose member test cases carry
    /// ZERO points must score its full max on RECOMPUTE. The persisted per-test
    /// score is `round(raw * tc.score) = round(1.0 * 0) = 0`, so re-deriving raw as
    /// `score / tc_max` (tc_max=0 -> 0.0) wrongly failed the subtask; the verdict
    /// (Accepted) is the authoritative full-pass signal.
    #[test]
    fn zero_point_group_min_member_full_pass_scores_full_on_recompute() {
        let test_cases = vec![
            TestCaseRow {
                id: 11,
                score: 0.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("m1".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
            TestCaseRow {
                id: 12,
                score: 0.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("m2".into()),
                input: TestCaseBodyRef::inline(""),
                expected_output: TestCaseBodyRef::inline(""),
                is_custom: false,
            },
        ];
        let subtasks = vec![SubtaskDef {
            name: "S".into(),
            scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
            max_score: 30.0,
            test_cases: vec!["m1".into(), "m2".into()],
        }];
        // Both members passed (Accepted), persisted score 0 because point value 0.
        let current_rows = vec![
            TcResultRow {
                submission_id: 1,
                test_case_id: 11,
                score: 0.0,
                verdict: Verdict::Accepted,
            },
            TcResultRow {
                submission_id: 1,
                test_case_id: 12,
                score: 0.0,
                verdict: Verdict::Accepted,
            },
        ];

        let scores = score_submission_subtask_details(&test_cases, &subtasks, &current_rows);
        assert_eq!(scores.len(), 1);
        assert_eq!(
            scores[0].score, 30.0,
            "fully-passing zero-point GroupMin subtask must score its full max, not 0"
        );

        // And a FAILED member (not Accepted) must still zero the GroupMin subtask.
        let failed_rows = vec![
            TcResultRow {
                submission_id: 1,
                test_case_id: 11,
                score: 0.0,
                verdict: Verdict::Accepted,
            },
            TcResultRow {
                submission_id: 1,
                test_case_id: 12,
                score: 0.0,
                verdict: Verdict::WrongAnswer,
            },
        ];
        let failed = score_submission_subtask_details(&test_cases, &subtasks, &failed_rows);
        assert_eq!(failed[0].score, 0.0, "a failed member must zero GroupMin");
    }
}
