// Every item below is reachable only from wasm32-gated production code
// and/or the #[cfg(test)] unit tests at the bottom of this file; this file
// has no top-level (unconditional) use of the SDK prelude, HashMap, or serde
// derive macros, so gate these imports the same way a native (test/clippy)
// build doesn't see them as unused.
#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use serde::Deserialize;
#[cfg(any(target_arch = "wasm32", test))]
use serde::Serialize;

// Only reachable from the wasm32-gated `run_judge`/`compute_official_task_score`
// below; this file has no #[cfg(test)] use of these three.
#[cfg(target_arch = "wasm32")]
use crate::config::{ContestConfig, ScoringMode, TaskConfig};
// Reachable from wasm32-gated production code AND directly from the
// #[cfg(test)] unit tests below (`score_submission_subtask_details`, which
// they call directly, and `SubtaskDef` literals they construct); gate the
// same way so a native (test/clippy) build doesn't see these as unused.
#[cfg(any(target_arch = "wasm32", test))]
use crate::case_ratios::CaseRatios;
#[cfg(target_arch = "wasm32")]
use crate::case_ratios::JudgedRatios;
#[cfg(any(target_arch = "wasm32", test))]
use crate::config::{SubtaskDef, round_score};
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
// `effective_subtasks` is reachable from the wasm32-gated `run_judge`
// below AND directly from the #[cfg(test)] unit tests; `score_all_subtasks`
// is reachable from both `score_submission_subtask_details` and
// `sum_best_subtask_score`, which are themselves wasm32+test. Gate both the
// same way.
#[cfg(any(target_arch = "wasm32", test))]
use crate::subtasks::{effective_subtasks, score_all_subtasks};
#[cfg(target_arch = "wasm32")]
use crate::{load_effective_subtasks, load_task_config, load_token_state};

// Only constructed by the wasm32-gated `compute_official_task_score` below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct MaxScore {
    max_score: Option<f64>,
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

// Only constructed by the wasm32-gated `compute_official_task_score` below.
#[cfg(target_arch = "wasm32")]
#[derive(Deserialize)]
struct SubmissionScore {
    score: f64,
}

// Called by the wasm32-gated `api.rs` subtask-scores handler AND directly by
// the #[cfg(test)] unit tests below; gate the same way.
#[cfg(any(target_arch = "wasm32", test))]
pub(crate) fn score_submission_subtask_details(
    test_cases: &[TestCaseRow],
    subtask_defs: &[SubtaskDef],
    ratios: &CaseRatios,
) -> Vec<SubtaskScoreDetail> {
    score_all_subtasks(subtask_defs, test_cases, ratios)
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

/// The ratios of a submission's current judgement, once it is finalized.
#[cfg(target_arch = "wasm32")]
pub(crate) fn load_current_submission_ratios(
    host: &Host,
    contest_id: i32,
    submission_id: i32,
) -> Result<Option<CaseRatios>, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT cr.ratios \
         FROM submission s \
         JOIN submission_judgement sj ON sj.submission_id = s.id \
         JOIN case_ratio cr ON cr.judgement_id = sj.id \
         WHERE s.id = {} AND s.contest_id = {} \
           AND sj.is_current = TRUE AND sj.is_finalized = TRUE",
        p.bind(submission_id),
        p.bind(contest_id)
    );
    Ok(host
        .db
        .query_one_with_args::<JudgedRatios>(&sql, &p.into_args())?
        .map(|row| row.ratios))
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

    let subtask_defs = effective_subtasks(&task_config.subtasks, &test_cases);

    let ctx = JudgeContext {
        contest_config: contest_config.clone(),
        task_config: task_config.clone(),
        submission_id: req.submission_id,
        problem_id: req.problem_id,
        contest_id,
        test_cases,
        subtask_defs,
    };

    judge_with_context_detached(host, req, &ctx)
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
        "SELECT cr.ratios \
         FROM submission s \
         JOIN submission_judgement sj ON sj.submission_id = s.id \
         JOIN case_ratio cr ON cr.judgement_id = sj.id \
         WHERE s.user_id = {} AND s.problem_id = {} AND s.contest_id = {} \
           AND sj.is_current = TRUE AND sj.is_finalized = TRUE",
        p.bind(user_id),
        p.bind(problem_id),
        p.bind(contest_id)
    );
    let judged: Vec<JudgedRatios> = host.db.query_with_args(&sql, &p.into_args())?;

    Ok(sum_best_subtask_score(
        subtask_defs,
        test_cases,
        judged.iter().map(|row| &row.ratios),
    ))
}

/// The SumBestSubtask score: for each of the user's submissions, score every
/// subtask from its judged case ratios, then for each subtask take
/// the best across submissions and sum. Single source of truth for both the
/// official task score ([`recompute_sum_best_subtask`]) and the scoreboard cell
/// (`crate::scoreboard`), which differ only in which submissions they fetch,
/// so the two cannot diverge on the value.
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
    per_submission_scores: impl IntoIterator<Item = &'a HashMap<i32, f64>>,
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
                "SELECT COALESCE(sj.score, 0.0) as score \
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
        let first: HashMap<i32, f64> = [(11, 1.0), (12, 0.0)].into_iter().collect();
        let second: HashMap<i32, f64> = [(11, 0.0), (12, 1.0)].into_iter().collect();

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
    fn subtask_detail_scores_are_derived_from_the_judged_ratios() {
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
        let ratios: CaseRatios = [(11, 1.0), (12, 0.0)].into_iter().collect();

        let scores = score_submission_subtask_details(&test_cases, &subtasks, &ratios);

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
        let subtasks = effective_subtasks(&[], &test_cases);
        let ratios: CaseRatios = [(11, 1.0), (12, 0.0)].into_iter().collect();

        let scores = score_submission_subtask_details(&test_cases, &subtasks, &ratios);

        assert_eq!(scores.len(), 1);
        assert_eq!(scores[0].name, "All Tests");
        assert_eq!(scores[0].score, 10.0);
        assert_eq!(scores[0].max_score, 100.0);
    }
}
