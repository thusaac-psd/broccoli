use std::collections::{HashMap, HashSet};

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::evaluator::{
    CaseOutcome, ContestJudge, DetachedEval, JudgeProgress, JudgeStep, ScoringCase, without_bodies,
};
use broccoli_server_sdk::types::*;
use serde::{Deserialize, Serialize};

use crate::config::round_score;
use crate::config::{SubtaskDef, SubtaskScoringMethod};
use crate::persist::persist_results;
use crate::subtasks::score_all_subtasks;
use crate::subtasks::test_case_reference_keys;

/// Per-test-case outcome from evaluation, with raw (0..1) scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalOutcome {
    pub test_case_id: i32,
    pub verdict: Verdict,
    pub raw_score: f64,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub message: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

/// IOI judging policy for the shared detached-evaluate driver.
///
/// IOI scores each case on a normalized 0..1 raw score, weights it by the
/// case's point value when persisting, and short-circuits per subtask via
/// [`SubtaskShortCircuit`]. The final submission score is the sum of subtask
/// scores. The short-circuit tracker and definitions ride in the policy so they
/// survive across callbacks; the driver owns persistence, the epoch guards, the
/// compile-error short-circuit, and the refill hint.
#[derive(Serialize, Deserialize)]
struct IoiJudge {
    all_test_cases: Vec<TestCaseRow>,
    subtask_defs: Vec<SubtaskDef>,
    short_circuit: SubtaskShortCircuit,
}

impl ContestJudge for IoiJudge {
    fn score(&self, result: &TestCaseVerdict) -> CaseOutcome {
        CaseOutcome::from_verdict(result, normalize_raw_score(result.score))
    }

    fn db_score(&self, outcome: &CaseOutcome, tc: &ScoringCase) -> f64 {
        // Weight the case's raw 0..1 score by its point value.
        round_score(outcome.score * tc.score)
    }

    fn next_step(&mut self, progress: &JudgeProgress<'_>) -> JudgeStep {
        let Some(last) = progress.last else {
            return JudgeStep::Continue;
        };
        // Cases already recorded (including the just-recorded trigger) must not
        // be re-cancelled.
        let already_recorded: HashSet<i32> =
            progress.outcomes.iter().map(|o| o.test_case_id).collect();
        let cancel_ids =
            self.short_circuit
                .cancellable_after(last.test_case_id, last.score, &already_recorded);
        if cancel_ids.is_empty() {
            // The driver keeps refilling while any case is unjudged, so an
            // unrelated pending subtask is never stranded.
            JudgeStep::Continue
        } else {
            self.short_circuit.mark_cancelled(&cancel_ids);
            JudgeStep::Cancel(cancel_ids)
        }
    }

    fn finalize(&self, host: &Host, progress: &JudgeProgress<'_>) -> Result<(), SdkError> {
        let id_to_keys: HashMap<i32, Vec<String>> = self
            .all_test_cases
            .iter()
            .map(|tc| (tc.id, test_case_reference_keys(tc)))
            .collect();
        let tc_scores: HashMap<String, f64> = progress
            .outcomes
            .iter()
            .filter(|outcome| !outcome.verdict.is_skipped_or_cancelled())
            .flat_map(|outcome| {
                id_to_keys
                    .get(&outcome.test_case_id)
                    .into_iter()
                    .flat_map(|keys| keys.iter().cloned().map(|key| (key, outcome.score)))
            })
            .collect();
        let subtask_results =
            score_all_subtasks(&self.subtask_defs, &self.all_test_cases, &tc_scores);
        let submission_score = round_score(subtask_results.iter().map(|result| result.score).sum());
        let persist_outcomes: Vec<EvalOutcome> = progress
            .outcomes
            .iter()
            .map(eval_outcome_from_case)
            .collect();
        persist_results(
            host,
            progress.submission.submission_id,
            progress.submission.judgement_id,
            progress.submission.judge_epoch,
            &persist_outcomes,
            submission_score,
        )?;
        Ok(())
    }
}

/// Adapt the driver's shared `CaseOutcome` into IOI's persistence-layer
/// `EvalOutcome` (which `persist_results` consumes). The per-case `score` is
/// IOI's normalized raw score; output was already stripped by the driver.
fn eval_outcome_from_case(outcome: &CaseOutcome) -> EvalOutcome {
    EvalOutcome {
        test_case_id: outcome.test_case_id,
        verdict: outcome.verdict.clone(),
        raw_score: outcome.score,
        time_used: outcome.time_used,
        memory_used: outcome.memory_used,
        message: outcome.message.clone(),
        stdout: None,
        stderr: None,
    }
}

pub fn evaluate_all_detached(
    host: &Host,
    req: &OnSubmissionInput,
    all_test_cases: &[TestCaseRow],
    scoring_test_cases: &[TestCaseRow],
    _submission_id: i32,
    subtask_defs: &[SubtaskDef],
) -> Result<OnSubmissionOutput, SdkError> {
    let policy = IoiJudge {
        // Scoring reads only id / score / is_sample. The policy rides the
        // session state on every callback, so it must not carry bodies.
        all_test_cases: all_test_cases.iter().map(without_bodies).collect(),
        subtask_defs: subtask_defs.to_vec(),
        short_circuit: SubtaskShortCircuit::new(subtask_defs, scoring_test_cases),
    };
    DetachedEval::start(
        host,
        req,
        scoring_test_cases,
        policy,
        "on_ioi_eval_result",
        4,
    )?;
    Ok(OnSubmissionOutput::ok())
}

pub fn handle_detached_eval_callback(
    host: &Host,
    input: DetachedEvaluateCallbackInput,
) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
    DetachedEval::<IoiJudge>::handle_callback(host, input)
}

/// Best-effort recovery when a detached result callback fails mid-stream (a
/// transient DB error, a rejected row). Without it, the callback's `?` would
/// abort the WASM call and strand the submission on "Judging" with no terminal
/// verdict. Fills any unrecorded case with `SystemError` and writes the terminal
/// update. All steps are best-effort: a stale epoch no-ops the writes and the
/// worker's submission-level retry re-drives judging later.
pub fn recover_detached_callback_error(host: &Host, state_value: &serde_json::Value) {
    DetachedEval::<IoiJudge>::recover(host, state_value);
}

fn normalize_raw_score(score: f64) -> f64 {
    if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubtaskShortCircuit {
    methods: Vec<SubtaskScoringMethod>,
    subtask_cases: Vec<HashSet<i32>>,
    case_subtasks: HashMap<i32, Vec<usize>>,
    failed_subtasks: HashSet<usize>,
    active_cases: HashSet<i32>,
    sample_cases: HashSet<i32>,
    test_case_order: Vec<i32>,
}

impl SubtaskShortCircuit {
    fn new(subtask_defs: &[SubtaskDef], test_cases: &[TestCaseRow]) -> Self {
        let mut key_to_ids: HashMap<String, Vec<i32>> = HashMap::new();
        for tc in test_cases {
            for key in test_case_reference_keys(tc) {
                key_to_ids.entry(key).or_default().push(tc.id);
            }
        }

        let mut subtask_cases = vec![HashSet::new(); subtask_defs.len()];
        let mut case_subtasks: HashMap<i32, Vec<usize>> = HashMap::new();

        for (subtask_idx, def) in subtask_defs.iter().enumerate() {
            for key in &def.test_cases {
                let Some(ids) = key_to_ids.get(key) else {
                    continue;
                };
                for id in ids {
                    if subtask_cases[subtask_idx].insert(*id) {
                        case_subtasks.entry(*id).or_default().push(subtask_idx);
                    }
                }
            }
        }

        Self {
            methods: subtask_defs.iter().map(|def| def.scoring_method).collect(),
            subtask_cases,
            case_subtasks,
            failed_subtasks: HashSet::new(),
            active_cases: test_cases.iter().map(|tc| tc.id).collect(),
            sample_cases: test_cases
                .iter()
                .filter(|tc| tc.is_sample)
                .map(|tc| tc.id)
                .collect(),
            test_case_order: test_cases.iter().map(|tc| tc.id).collect(),
        }
    }

    fn cancellable_after(
        &mut self,
        test_case_id: i32,
        raw_score: f64,
        already_recorded: &HashSet<i32>,
    ) -> Vec<i32> {
        self.active_cases.remove(&test_case_id);
        let failing_subtasks = self.failing_subtasks_for(test_case_id, raw_score);
        if failing_subtasks.is_empty() {
            return Vec::new();
        }

        self.failed_subtasks.extend(failing_subtasks);

        self.cancellable_cases_for_failure(test_case_id, &self.failed_subtasks)
            .into_iter()
            .filter(|id| !already_recorded.contains(id))
            .collect::<Vec<_>>()
    }

    fn mark_cancelled(&mut self, test_case_ids: &[i32]) {
        for id in test_case_ids {
            self.active_cases.remove(id);
        }
    }

    fn cancellable_cases_for_failure(
        &self,
        failed_test_case_id: i32,
        failed_subtasks: &HashSet<usize>,
    ) -> Vec<i32> {
        let Some(failed_case_subtasks) = self.case_subtasks.get(&failed_test_case_id) else {
            return Vec::new();
        };
        let newly_failed_cases = failed_case_subtasks
            .iter()
            .filter(|subtask_idx| self.method_can_short_circuit(**subtask_idx))
            .flat_map(|subtask_idx| self.subtask_cases[*subtask_idx].iter().copied())
            .collect::<HashSet<_>>();

        self.test_case_order
            .iter()
            .copied()
            .filter(|id| self.active_cases.contains(id))
            .filter(|id| !self.sample_cases.contains(id))
            .filter(|id| newly_failed_cases.contains(id))
            .filter(|id| {
                self.case_is_only_needed_by_failed_short_circuit_subtasks(*id, failed_subtasks)
            })
            .collect()
    }

    fn case_is_only_needed_by_failed_short_circuit_subtasks(
        &self,
        test_case_id: i32,
        failed_subtasks: &HashSet<usize>,
    ) -> bool {
        self.case_subtasks
            .get(&test_case_id)
            .is_some_and(|subtasks| {
                subtasks.iter().all(|subtask_idx| {
                    self.method_can_short_circuit(*subtask_idx)
                        && failed_subtasks.contains(subtask_idx)
                })
            })
    }

    fn failing_subtasks_for(&self, test_case_id: i32, raw_score: f64) -> HashSet<usize> {
        self.case_subtasks
            .get(&test_case_id)
            .into_iter()
            .flat_map(|subtasks| subtasks.iter().copied())
            .filter(|subtask_idx| self.result_fails_method(*subtask_idx, raw_score))
            .collect()
    }

    fn result_fails_method(&self, subtask_idx: usize, raw_score: f64) -> bool {
        match self.methods.get(subtask_idx) {
            Some(SubtaskScoringMethod::GroupMin) => raw_score < 1.0,
            Some(SubtaskScoringMethod::GroupMul) => raw_score <= 0.0,
            Some(SubtaskScoringMethod::Sum) | None => false,
        }
    }

    fn method_can_short_circuit(&self, subtask_idx: usize) -> bool {
        matches!(
            self.methods.get(subtask_idx),
            Some(SubtaskScoringMethod::GroupMin | SubtaskScoringMethod::GroupMul)
        )
    }
}

#[cfg(test)]
fn test_submission(test_cases: Vec<TestCaseRow>) -> OnSubmissionInput {
    OnSubmissionInput {
        submission_id: 1,
        judgement_id: 1,
        fire_after_judging: true,
        user_id: 10,
        problem_id: 100,
        contest_id: Some(1000),
        files: vec![SourceFile {
            filename: "main.cpp".into(),
            content: "int main() {}".into(),
        }],
        language: "cpp".into(),
        time_limit_ms: 2000,
        memory_limit_kb: 262144,
        problem_type: "ioi".into(),
        test_cases,
        judge_epoch: 1,
        target_worker_id: None,
    }
}

#[cfg(test)]
fn test_case(id: i32) -> TestCaseRow {
    TestCaseRow {
        id,
        score: 1.0,
        is_sample: false,
        position: id,
        description: None,
        label: None,
        input: TestCaseBodyRef::Missing,
        expected_output: TestCaseBodyRef::Missing,
        is_custom: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subtask(
        name: &str,
        scoring_method: crate::config::SubtaskScoringMethod,
        test_cases: Vec<String>,
    ) -> crate::config::SubtaskDef {
        crate::config::SubtaskDef {
            name: name.into(),
            scoring_method,
            max_score: 100.0,
            test_cases,
        }
    }

    fn ids(range: std::ops::RangeInclusive<i32>) -> Vec<String> {
        range.map(|id| id.to_string()).collect()
    }

    fn scored(test_case_id: i32, score: f64) -> TestCaseVerdict {
        TestCaseVerdict {
            score,
            ..TestCaseVerdict::accepted(test_case_id)
        }
    }

    /// Start a detached evaluation and feed `results` in order through the
    /// real callback, returning every callback output.
    fn run(
        host: &Host,
        tcs: &[TestCaseRow],
        subtasks: &[crate::config::SubtaskDef],
        results: Vec<TestCaseVerdict>,
    ) -> Vec<DetachedEvaluateCallbackOutput> {
        let req = test_submission(tcs.to_vec());
        evaluate_all_detached(host, &req, tcs, tcs, req.submission_id, subtasks).unwrap();
        let mut state = host.eval.detached_windowed_requests()[0].state.clone();
        let total = tcs.len();
        results
            .into_iter()
            .enumerate()
            .map(|(i, result)| {
                let out = handle_detached_eval_callback(
                    host,
                    DetachedEvaluateCallbackInput {
                        session_id: "ioi-detached".into(),
                        state: state.clone(),
                        event: DetachedEvaluateCallbackEvent::Result { result },
                        completed: i + 1,
                        total,
                    },
                )
                .unwrap();
                state = out.state.clone();
                out
            })
            .collect()
    }

    fn skipped_rows(host: &Host) -> Vec<i32> {
        let mut ids: Vec<i32> = host
            .submission
            .results()
            .into_iter()
            .filter(|row| row.verdict == Verdict::Skipped)
            .filter_map(|row| row.test_case_id)
            .collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn judges_four_cases_at_a_time() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        evaluate_all_detached(&host, &req, &tcs, &tcs, req.submission_id, &[]).unwrap();
        assert_eq!(host.eval.detached_windowed_requests()[0].concurrency, 4);
    }

    #[test]
    fn compile_error_finishes_and_skips_every_other_case() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let outs = run(&host, &tcs, &[], vec![TestCaseVerdict::compile_error(1)]);
        assert_eq!(outs[0].action, DetachedEvaluateCallbackAction::Finish);
        assert_eq!(skipped_rows(&host), (2..=10).collect::<Vec<_>>());
    }

    #[test]
    fn group_min_short_circuits_remaining_exclusive_cases() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let subtasks = [subtask(
            "All",
            crate::config::SubtaskScoringMethod::GroupMin,
            ids(1..=10),
        )];
        let results = vec![
            TestCaseVerdict::accepted(1),
            TestCaseVerdict::accepted(2),
            scored(3, 0.5),
        ];
        let outs = run(&host, &tcs, &subtasks, results);
        assert!(outs[..2].iter().all(|o| o.cancel_test_case_ids.is_empty()));
        // Skipping 4..=10 leaves nothing unjudged, so the session finishes.
        assert_eq!(outs[2].action, DetachedEvaluateCallbackAction::Finish);
        assert_eq!(skipped_rows(&host), (4..=10).collect::<Vec<_>>());
    }

    #[test]
    fn sum_subtask_does_not_short_circuit_on_partial_score() {
        let host = Host::mock();
        let tcs = (1..=5).map(test_case).collect::<Vec<_>>();
        let subtasks = [subtask(
            "Sum",
            crate::config::SubtaskScoringMethod::Sum,
            ids(1..=5),
        )];
        let mut results = vec![TestCaseVerdict::accepted(1), scored(2, 0.0)];
        results.extend((3..=5).map(TestCaseVerdict::accepted));
        let outs = run(&host, &tcs, &subtasks, results);
        assert!(outs.iter().all(|o| o.cancel_test_case_ids.is_empty()));
        assert_eq!(outs[4].action, DetachedEvaluateCallbackAction::Finish);
        assert!(skipped_rows(&host).is_empty());
    }

    #[test]
    fn overlapping_subtasks_cancel_only_cases_not_needed_by_active_subtasks() {
        let host = Host::mock();
        let tcs = (1..=5).map(test_case).collect::<Vec<_>>();
        let subtasks = [
            subtask(
                "Small",
                crate::config::SubtaskScoringMethod::GroupMin,
                ids(1..=2),
            ),
            subtask(
                "Large",
                crate::config::SubtaskScoringMethod::GroupMin,
                ids(1..=5),
            ),
        ];
        let results = vec![
            TestCaseVerdict::accepted(1),
            TestCaseVerdict::accepted(2),
            TestCaseVerdict::wrong_answer(3),
        ];
        let outs = run(&host, &tcs, &subtasks, results);
        assert_eq!(outs[2].action, DetachedEvaluateCallbackAction::Finish);
        assert_eq!(skipped_rows(&host), vec![4, 5]);
    }

    #[test]
    fn group_mul_short_circuits_on_zero_score() {
        let host = Host::mock();
        let tcs = (1..=6).map(test_case).collect::<Vec<_>>();
        let subtasks = [subtask(
            "Mul",
            crate::config::SubtaskScoringMethod::GroupMul,
            ids(1..=6),
        )];
        let results = vec![TestCaseVerdict::accepted(1), scored(2, 0.0)];
        let outs = run(&host, &tcs, &subtasks, results);
        assert_eq!(outs[1].action, DetachedEvaluateCallbackAction::Finish);
        assert_eq!(skipped_rows(&host), vec![3, 4, 5, 6]);
    }

    #[test]
    fn sample_outside_failed_subtask_continues_judging() {
        let host = Host::mock();
        let mut tcs = (1..=6).map(test_case).collect::<Vec<_>>();
        tcs[5].is_sample = true;
        tcs[5].score = 0.0;
        let subtasks = [subtask(
            "Scoring",
            crate::config::SubtaskScoringMethod::GroupMin,
            ids(1..=5),
        )];
        let results = vec![
            TestCaseVerdict::wrong_answer(1),
            TestCaseVerdict::accepted(6),
        ];
        let outs = run(&host, &tcs, &subtasks, results);
        assert_eq!(outs[0].cancel_test_case_ids, vec![2, 3, 4, 5]);
        assert_eq!(outs[0].action, DetachedEvaluateCallbackAction::Continue);
        assert_eq!(outs[1].action, DetachedEvaluateCallbackAction::Finish);
        assert_eq!(skipped_rows(&host), vec![2, 3, 4, 5]);
    }

    #[test]
    fn sample_inside_failed_subtask_continues_judging() {
        let host = Host::mock();
        let mut tcs = (1..=5).map(test_case).collect::<Vec<_>>();
        tcs[4].is_sample = true;
        tcs[4].score = 0.0;
        let subtasks = [subtask(
            "ScoringWithSample",
            crate::config::SubtaskScoringMethod::GroupMin,
            ids(1..=5),
        )];
        let results = vec![
            TestCaseVerdict::wrong_answer(1),
            TestCaseVerdict::accepted(5),
        ];
        let outs = run(&host, &tcs, &subtasks, results);
        assert_eq!(outs[0].cancel_test_case_ids, vec![2, 3, 4]);
        assert_eq!(outs[1].action, DetachedEvaluateCallbackAction::Finish);
        assert_eq!(skipped_rows(&host), vec![2, 3, 4]);
    }

    #[test]
    fn nested_subtask_zero_score_sibling_stays_active_after_parent_failure() {
        let host = Host::mock();
        let mut tcs = vec![test_case(1), test_case(2), test_case(3)];
        tcs[0].label = Some("parent_fail".into());
        tcs[0].score = 50.0;
        tcs[1].label = Some("child_zero".into());
        tcs[1].score = 0.0;
        tcs[2].label = Some("child_score".into());
        tcs[2].score = 50.0;
        let req = test_submission(tcs.clone());
        let subtasks = vec![
            crate::config::SubtaskDef {
                name: "Parent".into(),
                scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
                max_score: 50.0,
                test_cases: vec![
                    "parent_fail".into(),
                    "child_zero".into(),
                    "child_score".into(),
                ],
            },
            crate::config::SubtaskDef {
                name: "NestedChild".into(),
                scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
                max_score: 50.0,
                test_cases: vec!["child_zero".into(), "child_score".into()],
            },
        ];

        evaluate_all_detached(&host, &req, &tcs, &tcs, req.submission_id, &subtasks).unwrap();
        let detached = host.eval.detached_windowed_requests();
        assert_eq!(detached.len(), 1);
        assert_eq!(
            detached[0]
                .batch
                .test_cases
                .iter()
                .map(|case| case.test_case_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );

        let first = handle_detached_eval_callback(
            &host,
            DetachedEvaluateCallbackInput {
                session_id: "nested-detached".into(),
                state: detached[0].state.clone(),
                event: DetachedEvaluateCallbackEvent::Result {
                    result: TestCaseVerdict::wrong_answer(1),
                },
                completed: 1,
                total: 3,
            },
        )
        .unwrap();
        assert_eq!(first.action, DetachedEvaluateCallbackAction::Continue);
        assert!(
            first.cancel_test_case_ids.is_empty(),
            "parent failure must not cancel nested child members"
        );

        let second = handle_detached_eval_callback(
            &host,
            DetachedEvaluateCallbackInput {
                session_id: "nested-detached".into(),
                state: first.state,
                event: DetachedEvaluateCallbackEvent::Result {
                    result: TestCaseVerdict::accepted(2),
                },
                completed: 2,
                total: 3,
            },
        )
        .unwrap();
        assert_eq!(second.action, DetachedEvaluateCallbackAction::Continue);
        assert!(
            second.cancel_test_case_ids.is_empty(),
            "zero-score nested sibling completion must not cancel the scored sibling"
        );

        let third = handle_detached_eval_callback(
            &host,
            DetachedEvaluateCallbackInput {
                session_id: "nested-detached".into(),
                state: second.state,
                event: DetachedEvaluateCallbackEvent::Result {
                    result: TestCaseVerdict::accepted(3),
                },
                completed: 3,
                total: 3,
            },
        )
        .unwrap();
        assert_eq!(third.action, DetachedEvaluateCallbackAction::Finish);
        assert!(third.cancel_test_case_ids.is_empty());

        let rows = host.submission.results();
        let testcase_rows = rows
            .iter()
            .filter(|row| row.test_case_id.is_some())
            .collect::<Vec<_>>();
        assert_eq!(testcase_rows.len(), 3);
        assert!(
            testcase_rows
                .iter()
                .any(|row| row.test_case_id == Some(2) && row.verdict == Verdict::Accepted),
            "zero-score nested sibling must be persisted as judged, not skipped"
        );
        assert!(
            testcase_rows
                .iter()
                .any(|row| row.test_case_id == Some(3) && row.verdict == Verdict::Accepted),
            "scored nested sibling must be persisted as judged, not skipped"
        );
        assert!(
            testcase_rows
                .iter()
                .all(|row| row.verdict != Verdict::Skipped),
            "detached callback path must not persist skipped rows for active nested siblings"
        );
    }

    /// Regression: two disjoint group_min subtasks. A WA on the first
    /// subtask's first case cancels the rest of that subtask, but the
    /// callback must keep `refill` enabled so the host still dispatches
    /// the second subtask's cases instead of breaking the session and
    /// filling them in as SystemError on Exhausted.
    #[test]
    fn disjoint_subtask_failure_keeps_refilling_other_subtask() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        let subtasks = vec![
            crate::config::SubtaskDef {
                name: "A".into(),
                scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
                max_score: 60.0,
                test_cases: (1..=8).map(|id| id.to_string()).collect(),
            },
            crate::config::SubtaskDef {
                name: "B".into(),
                scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
                max_score: 40.0,
                test_cases: (9..=10).map(|id| id.to_string()).collect(),
            },
        ];

        evaluate_all_detached(&host, &req, &tcs, &tcs, req.submission_id, &subtasks).unwrap();
        let detached = host.eval.detached_windowed_requests();
        assert_eq!(detached.len(), 1);

        let first = handle_detached_eval_callback(
            &host,
            DetachedEvaluateCallbackInput {
                session_id: "disjoint-detached".into(),
                state: detached[0].state.clone(),
                event: DetachedEvaluateCallbackEvent::Result {
                    result: TestCaseVerdict::wrong_answer(1),
                },
                completed: 1,
                total: 10,
            },
        )
        .unwrap();
        assert_eq!(first.action, DetachedEvaluateCallbackAction::Continue);
        assert_eq!(first.cancel_test_case_ids, vec![2, 3, 4, 5, 6, 7, 8]);
        assert!(
            first.refill,
            "cancelling subtask A's cases must not disable refills while subtask B is pending"
        );

        let second = handle_detached_eval_callback(
            &host,
            DetachedEvaluateCallbackInput {
                session_id: "disjoint-detached".into(),
                state: first.state,
                event: DetachedEvaluateCallbackEvent::Result {
                    result: TestCaseVerdict::accepted(9),
                },
                completed: 2,
                total: 10,
            },
        )
        .unwrap();
        assert_eq!(second.action, DetachedEvaluateCallbackAction::Continue);
        assert!(second.refill);
        assert!(second.cancel_test_case_ids.is_empty());

        let third = handle_detached_eval_callback(
            &host,
            DetachedEvaluateCallbackInput {
                session_id: "disjoint-detached".into(),
                state: second.state,
                event: DetachedEvaluateCallbackEvent::Result {
                    result: TestCaseVerdict::accepted(10),
                },
                completed: 3,
                total: 10,
            },
        )
        .unwrap();
        assert_eq!(third.action, DetachedEvaluateCallbackAction::Finish);

        let rows = host.submission.results();
        let testcase_rows = rows
            .iter()
            .filter(|row| row.test_case_id.is_some())
            .collect::<Vec<_>>();
        assert_eq!(testcase_rows.len(), 10);
        assert!(
            testcase_rows
                .iter()
                .all(|row| row.verdict != Verdict::SystemError),
            "no case may be persisted as SystemError in this flow"
        );
        for id in 9..=10 {
            assert!(
                testcase_rows
                    .iter()
                    .any(|row| row.test_case_id == Some(id) && row.verdict == Verdict::Accepted),
                "subtask B case {id} must still be judged after subtask A failed"
            );
        }
        for id in 2..=8 {
            assert!(
                testcase_rows
                    .iter()
                    .any(|row| row.test_case_id == Some(id) && row.verdict == Verdict::Skipped),
                "subtask A case {id} must be skipped via short-circuit"
            );
        }
    }
}
