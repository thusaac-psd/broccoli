#[cfg(any(target_arch = "wasm32", test))]
use std::collections::HashMap;

use broccoli_server_sdk::prelude::*;
use serde::{Deserialize, Serialize};

use crate::case_ratios::CaseRatios;
#[cfg(target_arch = "wasm32")]
use crate::case_ratios::{JOIN_JUDGED_RATIOS, JudgedSubtasks};
#[cfg(target_arch = "wasm32")]
use crate::config::{ContestConfig, TaskConfig};
use crate::config::{SubtaskDef, round_score};
#[cfg(target_arch = "wasm32")]
use crate::judge::{JudgeContext, judge_with_context_detached};
#[cfg(target_arch = "wasm32")]
use crate::load_task_config;
#[cfg(any(target_arch = "wasm32", test))]
use crate::scoring::score_sum_best_subtask;
#[cfg(any(target_arch = "wasm32", test))]
use crate::subtasks::effective_subtasks;
use crate::subtasks::score_all_subtasks;

/// One subtask's result as judging scored it, in the order of the subtasks
/// the judgement was scored by.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SubtaskScoreDetail {
    pub(crate) name: String,
    pub(crate) scoring_method: crate::config::SubtaskScoringMethod,
    pub(crate) score: f64,
    pub(crate) max_score: f64,
}

/// The result of every subtask in `subtask_defs` for a judgement whose cases
/// scored `ratios`.
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

/// The subtask results a submission's current judgement was scored with,
/// once the submission holds it as its standing result.
#[cfg(target_arch = "wasm32")]
pub(crate) fn load_current_judged_subtasks(
    host: &Host,
    contest_id: i32,
    submission_id: i32,
) -> Result<Option<Vec<SubtaskScoreDetail>>, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT cr.subtasks \
         FROM submission s \
         JOIN submission_judgement sj ON sj.submission_id = s.id \
         {JOIN_JUDGED_RATIOS} \
         WHERE s.id = {} AND s.contest_id = {} \
           AND sj.is_current = TRUE AND {standing}",
        p.bind(submission_id),
        p.bind(contest_id),
        standing = broccoli_server_sdk::contest::standing_result("s"),
    );
    Ok(host
        .db
        .query_one_with_args::<JudgedSubtasks>(&sql, &p.into_args())?
        .map(|row| row.subtasks))
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

/// The SumBestSubtask score: for each of the user's submissions, score every
/// subtask from its judged case ratios, then for each subtask take
/// the best across submissions and sum. The scoreboard's sum-best-subtask
/// cell (`crate::scoreboard`) reads its score through this.
// Called from `scoreboard.rs`'s wasm32-gated sum-best-subtask cell loader AND
// directly by the #[cfg(test)] unit tests below; gate the same way.
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
