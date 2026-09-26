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

/// Auto-flush threshold for buffered `TestCaseResultRow`s. IOI problems
/// commonly have 20-50 testcases across many subtasks; an 8-row chunk
/// keeps the per-submission INSERT count in the 2-6 range instead of
/// one INSERT per testcase (UP#34).
#[cfg(test)]
const RESULT_BATCH_FLUSH_THRESHOLD: usize = 8;

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
    Ok(OnSubmissionOutput {
        success: true,
        error_message: None,
    })
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

#[cfg(test)]
fn build_batch_input(
    req: &OnSubmissionInput,
    test_cases: &[TestCaseRow],
) -> StartEvaluateBatchInput {
    StartEvaluateBatchInput {
        problem_type: req.problem_type.clone(),
        test_cases: test_cases
            .iter()
            .map(|tc| StartEvaluateCaseInput {
                problem_id: req.problem_id,
                test_case_id: tc.id,
                solution_source: req
                    .files
                    .iter()
                    .map(|f| SourceFile {
                        filename: f.filename.clone(),
                        content: f.content.clone(),
                    })
                    .collect(),
                solution_language: req.language.clone(),
                time_limit_ms: req.time_limit_ms,
                memory_limit_kb: req.memory_limit_kb,
                contest_id: req.contest_id,
                input: tc.input.clone(),
                expected_output: tc.expected_output.clone(),
                is_custom: tc.is_custom,
                target_worker_id: req.target_worker_id.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
pub fn evaluate_all(
    host: &Host,
    req: &OnSubmissionInput,
    test_cases: &[TestCaseRow],
    submission_id: i32,
    scale_score: impl Fn(f64, &TestCaseRow) -> f64,
    subtask_defs: &[SubtaskDef],
) -> Result<Vec<EvalOutcome>, SdkError> {
    let batch_input = build_batch_input(req, test_cases);

    let tc_map: HashMap<i32, &TestCaseRow> = test_cases.iter().map(|tc| (tc.id, tc)).collect();
    let mut short_circuit = SubtaskShortCircuit::new(subtask_defs, test_cases);

    let _ = host
        .submission
        .delete_results(submission_id, req.judgement_id, req.judge_epoch);

    let mut outcomes: Vec<EvalOutcome> = Vec::new();
    // Per-submission row buffer (UP#34). Drained by the threshold flush
    // inside `record_outcome` and force-flushed before every early
    // return so callers see terminal verdicts in `test_case_result`.
    let mut row_buf: Vec<TestCaseResultRow> = Vec::new();

    let mut session = match host.eval.windowed(&batch_input).concurrency(4).start() {
        Ok(session) => session,
        Err(e) => {
            for tc in test_cases {
                let outcome = EvalOutcome {
                    test_case_id: tc.id,
                    verdict: Verdict::SystemError,
                    raw_score: 0.0,
                    time_used: None,
                    memory_used: None,
                    message: Some(format!("BATCH_START_FAILED: {e:?}")),
                    stdout: None,
                    stderr: None,
                };
                record_outcome(
                    host,
                    &mut row_buf,
                    submission_id,
                    req.judgement_id,
                    req.judge_epoch,
                    &outcome,
                    &tc_map,
                    &scale_score,
                )?;
                outcomes.push(outcome);
            }
            flush_results(host, &mut row_buf)?;
            return Ok(outcomes);
        }
    };

    let affected =
        host.submission
            .set_compiling(submission_id, req.judgement_id, req.judge_epoch)?;

    if affected == 0 {
        let _ = session.cancel_all();
        return Err(SdkError::StaleEpoch);
    }

    let _ = host.log.info(&format!(
        "Started evaluate batch for {} test cases",
        test_cases.len()
    ));

    let mut collected = 0;
    let mut timed_out = false;
    let mut marked_running = false;
    let result_timeout_ms = default_evaluation_result_timeout_ms(req.time_limit_ms);

    while collected < test_cases.len() {
        match session.next_result_with_refill_filter(result_timeout_ms, |verdict| {
            verdict.verdict != Verdict::CompileError
                && short_circuit
                    .should_refill_after(verdict.test_case_id, normalize_raw_score(verdict.score))
        }) {
            Ok(Some(verdict)) => {
                let normalized = normalize_raw_score(verdict.score);

                let outcome = EvalOutcome {
                    test_case_id: verdict.test_case_id,
                    verdict: verdict.verdict,
                    raw_score: normalized,
                    time_used: verdict
                        .time_used_ms
                        .map(|t| t.clamp(0, i32::MAX as i64) as i32),
                    memory_used: verdict
                        .memory_used_kb
                        .map(|m| m.clamp(0, i32::MAX as i64) as i32),
                    message: verdict.message,
                    stdout: verdict.stdout,
                    stderr: verdict.stderr,
                };

                if outcome.verdict == Verdict::CompileError {
                    record_outcome(
                        host,
                        &mut row_buf,
                        submission_id,
                        req.judgement_id,
                        req.judge_epoch,
                        &outcome,
                        &tc_map,
                        &scale_score,
                    )?;
                    outcomes.push(outcome);
                    let collected_ids: HashSet<i32> =
                        outcomes.iter().map(|r| r.test_case_id).collect();
                    for tc in test_cases {
                        if !collected_ids.contains(&tc.id) {
                            let outcome = EvalOutcome {
                                test_case_id: tc.id,
                                verdict: Verdict::Skipped,
                                raw_score: 0.0,
                                time_used: None,
                                memory_used: None,
                                message: Some("SKIPPED_SHORT_CIRCUIT".into()),
                                stdout: None,
                                stderr: None,
                            };
                            record_outcome(
                                host,
                                &mut row_buf,
                                submission_id,
                                req.judgement_id,
                                req.judge_epoch,
                                &outcome,
                                &tc_map,
                                &scale_score,
                            )?;
                            outcomes.push(outcome);
                        }
                    }
                    let _ = session.cancel_all();
                    break;
                }

                if !marked_running {
                    let affected = host.submission.set_running(
                        submission_id,
                        req.judgement_id,
                        req.judge_epoch,
                    )?;
                    if affected == 0 {
                        let _ = session.cancel_all();
                        return Err(SdkError::StaleEpoch);
                    }
                    marked_running = true;
                }

                record_outcome(
                    host,
                    &mut row_buf,
                    submission_id,
                    req.judgement_id,
                    req.judge_epoch,
                    &outcome,
                    &tc_map,
                    &scale_score,
                )?;
                outcomes.push(outcome);
                collected += 1;

                let already_recorded: HashSet<i32> =
                    outcomes.iter().map(|o| o.test_case_id).collect();
                let cancel_ids = short_circuit.cancellable_after(
                    verdict.test_case_id,
                    normalized,
                    &already_recorded,
                );
                if !cancel_ids.is_empty() {
                    match session.cancel_test_cases(&cancel_ids) {
                        Ok(cancelled_count) if cancelled_count == cancel_ids.len() => {
                            short_circuit.mark_cancelled(&cancel_ids);
                            for test_case_id in cancel_ids {
                                let skipped = EvalOutcome {
                                    test_case_id,
                                    verdict: Verdict::Skipped,
                                    raw_score: 0.0,
                                    time_used: None,
                                    memory_used: None,
                                    message: Some("SKIPPED_SHORT_CIRCUIT".into()),
                                    stdout: None,
                                    stderr: None,
                                };
                                record_outcome(
                                    host,
                                    &mut row_buf,
                                    submission_id,
                                    req.judgement_id,
                                    req.judge_epoch,
                                    &skipped,
                                    &tc_map,
                                    &scale_score,
                                )?;
                                outcomes.push(skipped);
                                collected += 1;
                            }
                        }
                        Ok(cancelled_count) => {
                            let _ = host.log.info(&format!(
                                "Short-circuit cancel skipped: requested {}, cancelled {}",
                                cancel_ids.len(),
                                cancelled_count
                            ));
                        }
                        Err(e) => {
                            let _ = host
                                .log
                                .info(&format!("Short-circuit cancel failed: {e:?}"));
                        }
                    }
                }
            }
            Ok(None) => {
                let _ = host.log.info(&format!(
                    "Timeout waiting for result {}/{}",
                    collected + 1,
                    test_cases.len()
                ));
                timed_out = true;
                break;
            }
            Err(e) => {
                let _ = host.log.info(&format!("Error polling result: {e:?}"));
                timed_out = true;
                break;
            }
        }
    }

    if timed_out {
        let collected_ids: HashSet<i32> = outcomes.iter().map(|r| r.test_case_id).collect();
        for tc in test_cases {
            if !collected_ids.contains(&tc.id) {
                let outcome = EvalOutcome {
                    test_case_id: tc.id,
                    verdict: Verdict::SystemError,
                    raw_score: 0.0,
                    time_used: None,
                    memory_used: None,
                    message: Some("EVALUATION_TIMEOUT".into()),
                    stdout: None,
                    stderr: None,
                };
                record_outcome(
                    host,
                    &mut row_buf,
                    submission_id,
                    req.judgement_id,
                    req.judge_epoch,
                    &outcome,
                    &tc_map,
                    &scale_score,
                )?;
                outcomes.push(outcome);
            }
        }
        let _ = session.cancel_all();
    }

    // Final flush before returning so the caller's terminal verdicts
    // are visible in `test_case_result` no matter which exit path we
    // took.
    flush_results(host, &mut row_buf)?;

    Ok(outcomes)
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

    /// Refill hint for the synchronous session only: there the refill
    /// happens BEFORE the cancel set is applied, so holding a refill back
    /// while a cancellation is imminent avoids dispatching a doomed case.
    /// The session self-heals afterwards (it refills whenever its active
    /// window is empty), so `false` never strands pending cases. Do NOT
    /// reuse this for the detached callback: the detached host treats
    /// `refill=false` as "stop dispatching entirely" and would strand
    /// pending cases of unrelated subtasks.
    #[cfg(test)]
    fn should_refill_after(&self, test_case_id: i32, raw_score: f64) -> bool {
        let failing_subtasks = self.failing_subtasks_for(test_case_id, raw_score);
        if failing_subtasks.is_empty() {
            return true;
        }
        let mut failed_subtasks = self.failed_subtasks.clone();
        failed_subtasks.extend(failing_subtasks);
        self.cancellable_cases_for_failure(test_case_id, &failed_subtasks)
            .is_empty()
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
fn build_tc_row(
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
    outcome: &EvalOutcome,
    tc_map: &HashMap<i32, &TestCaseRow>,
    scale_score: &impl Fn(f64, &TestCaseRow) -> f64,
) -> TestCaseResultRow {
    let tc = tc_map.get(&outcome.test_case_id);
    let score = match tc {
        Some(tc) => scale_score(outcome.raw_score, tc),
        None => 0.0,
    };
    let is_custom = tc.is_some_and(|t| t.is_custom);
    let (tc_id, run_index) = if is_custom {
        (None, Some(outcome.test_case_id))
    } else {
        (Some(outcome.test_case_id), None)
    };
    TestCaseResultRow {
        submission_id,
        judgement_id,
        judge_epoch,
        test_case_id: tc_id,
        run_index,
        verdict: outcome.verdict.clone(),
        score,
        time_used: outcome.time_used,
        memory_used: outcome.memory_used,
        message: sanitize_optional_text(outcome.message.as_deref()),
        stdout: sanitize_optional_text(outcome.stdout.as_deref()),
        stderr: sanitize_optional_text(outcome.stderr.as_deref()),
    }
}

/// Drain `buf` into one bulk `insert_results` call. No-op if `buf` is
/// empty.
#[cfg(test)]
fn flush_results(host: &Host, buf: &mut Vec<TestCaseResultRow>) -> Result<(), SdkError> {
    if buf.is_empty() {
        return Ok(());
    }
    host.submission.insert_results(buf)?;
    buf.clear();
    Ok(())
}

/// Append a row for `outcome` to `buf`; flush when the buffer reaches
/// `RESULT_BATCH_FLUSH_THRESHOLD`.
#[cfg(test)]
// Test-only fixture: each parameter mirrors one argument of the production
// batch-recording call site (host handle, output buffer, and the row
// identity/outcome/lookup values `build_tc_row` needs) 1:1, which keeps every
// call site self-documenting. Bundling them into a params struct would only
// relocate the same list one level of indirection without reducing the real
// complexity here.
#[allow(clippy::too_many_arguments)]
fn record_outcome(
    host: &Host,
    buf: &mut Vec<TestCaseResultRow>,
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
    outcome: &EvalOutcome,
    tc_map: &HashMap<i32, &TestCaseRow>,
    scale_score: &impl Fn(f64, &TestCaseRow) -> f64,
) -> Result<(), SdkError> {
    buf.push(build_tc_row(
        submission_id,
        judgement_id,
        judge_epoch,
        outcome,
        tc_map,
        scale_score,
    ));
    if buf.len() >= RESULT_BATCH_FLUSH_THRESHOLD {
        flush_results(host, buf)?;
    }
    Ok(())
}

#[cfg(test)]
fn sanitize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(|s| sanitize_result_text_field(s).into_owned())
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
fn batch_test_case_ids(host: &Host) -> Vec<Vec<i32>> {
    host.eval
        .batch_inputs()
        .into_iter()
        .map(|batch| {
            batch
                .test_cases
                .into_iter()
                .map(|case| case.test_case_id)
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_four_case_initial_window_for_ten_test_cases() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        for id in 1..=10 {
            host.eval.queue_result(TestCaseVerdict::accepted(id));
        }

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &[]).unwrap();

        assert_eq!(outcomes.len(), 10);
        assert_eq!(
            batch_test_case_ids(&host),
            vec![
                vec![1, 2, 3, 4],
                vec![5],
                vec![6],
                vec![7],
                vec![8],
                vec![9],
                vec![10],
            ]
        );
        let updates = host.submission.updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::Running));
    }

    #[test]
    fn stale_epoch_cancels_active_windowed_evaluation() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        host.submission.queue_update_result(Ok(0));

        let err = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &[]).unwrap_err();

        assert!(matches!(err, SdkError::StaleEpoch));
        assert_eq!(batch_test_case_ids(&host), vec![vec![1, 2, 3, 4]]);
        assert!(host.eval.was_cancelled());
    }

    #[test]
    fn compile_error_does_not_refill_but_fills_remaining_cases() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::compile_error(1));

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &[]).unwrap();

        assert_eq!(outcomes.len(), 10);
        assert_eq!(outcomes[0].verdict, Verdict::CompileError);
        assert!(outcomes[1..].iter().all(|o| o.verdict == Verdict::Skipped));
        assert_eq!(batch_test_case_ids(&host), vec![vec![1, 2, 3, 4]]);
        assert_eq!(host.submission.results().len(), 10);
        assert!(host.eval.was_cancelled());
        let updates = host.submission.updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
    }

    #[test]
    fn group_min_short_circuits_remaining_exclusive_cases() {
        let host = Host::mock();
        let tcs = (1..=10).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        let subtasks = vec![crate::config::SubtaskDef {
            name: "All".into(),
            scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
            max_score: 100.0,
            test_cases: (1..=10).map(|id| id.to_string()).collect(),
        }];
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 3,
            verdict: Verdict::Accepted,
            score: 0.5,
            time_used_ms: None,
            memory_used_kb: None,
            message: None,
            stdout: None,
            stderr: None,
        });

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        let skipped_ids = outcomes
            .iter()
            .filter(|outcome| outcome.verdict == Verdict::Skipped)
            .map(|outcome| outcome.test_case_id)
            .collect::<Vec<_>>();
        assert_eq!(skipped_ids, vec![4, 5, 6, 7, 8, 9, 10]);
        assert_eq!(
            host.eval.testcase_cancels(),
            vec![
                ("mock-batch-1".to_string(), vec![4]),
                ("mock-batch-2".to_string(), vec![5]),
                ("mock-batch-3".to_string(), vec![6]),
            ]
        );
        assert_eq!(
            batch_test_case_ids(&host),
            vec![vec![1, 2, 3, 4], vec![5], vec![6]]
        );
    }

    #[test]
    fn sum_subtask_does_not_short_circuit_on_partial_score() {
        let host = Host::mock();
        let tcs = (1..=5).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        let subtasks = vec![crate::config::SubtaskDef {
            name: "Sum".into(),
            scoring_method: crate::config::SubtaskScoringMethod::Sum,
            max_score: 100.0,
            test_cases: (1..=5).map(|id| id.to_string()).collect(),
        }];
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 2,
            verdict: Verdict::Accepted,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: None,
            stdout: None,
            stderr: None,
        });
        for id in 3..=5 {
            host.eval.queue_result(TestCaseVerdict::accepted(id));
        }

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        assert_eq!(outcomes.len(), 5);
        assert!(
            outcomes
                .iter()
                .all(|outcome| outcome.verdict != Verdict::Skipped)
        );
        assert!(host.eval.testcase_cancels().is_empty());
    }

    #[test]
    fn overlapping_subtasks_cancel_only_cases_not_needed_by_active_subtasks() {
        let host = Host::mock();
        let tcs = (1..=5).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        let subtasks = vec![
            crate::config::SubtaskDef {
                name: "Small".into(),
                scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
                max_score: 40.0,
                test_cases: vec!["1".into(), "2".into()],
            },
            crate::config::SubtaskDef {
                name: "Large".into(),
                scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
                max_score: 60.0,
                test_cases: (1..=5).map(|id| id.to_string()).collect(),
            },
        ];
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(3));

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        let skipped_ids = outcomes
            .iter()
            .filter(|outcome| outcome.verdict == Verdict::Skipped)
            .map(|outcome| outcome.test_case_id)
            .collect::<Vec<_>>();
        assert_eq!(skipped_ids, vec![4, 5]);
        assert_eq!(
            host.eval.testcase_cancels(),
            vec![
                ("mock-batch-1".to_string(), vec![4]),
                ("mock-batch-2".to_string(), vec![5]),
            ]
        );
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
        host.eval.queue_result(TestCaseVerdict::wrong_answer(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));
        host.eval.queue_result(TestCaseVerdict::accepted(3));

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        let skipped_ids = outcomes
            .iter()
            .filter(|outcome| outcome.verdict == Verdict::Skipped)
            .map(|outcome| outcome.test_case_id)
            .collect::<Vec<_>>();
        assert!(
            skipped_ids.is_empty(),
            "nested child cases must not be skipped while NestedChild remains active"
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome.test_case_id == 2 && outcome.verdict == Verdict::Accepted),
            "zero-score nested sibling must still be judged"
        );
        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome.test_case_id == 3 && outcome.verdict == Verdict::Accepted),
            "scored nested sibling must still be judged"
        );
        assert!(
            host.eval.testcase_cancels().is_empty(),
            "parent failure must not cancel cases still needed by the nested child subtask"
        );
    }

    #[test]
    fn detached_nested_subtask_zero_score_sibling_stays_active_after_parent_failure() {
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
    fn detached_disjoint_subtask_failure_keeps_refilling_other_subtask() {
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

    #[test]
    fn group_mul_short_circuits_on_zero_score() {
        let host = Host::mock();
        let tcs = (1..=6).map(test_case).collect::<Vec<_>>();
        let req = test_submission(tcs.clone());
        let subtasks = vec![crate::config::SubtaskDef {
            name: "Mul".into(),
            scoring_method: crate::config::SubtaskScoringMethod::GroupMul,
            max_score: 100.0,
            test_cases: (1..=6).map(|id| id.to_string()).collect(),
        }];
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 2,
            verdict: Verdict::Accepted,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: None,
            stdout: None,
            stderr: None,
        });

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        let skipped_ids = outcomes
            .iter()
            .filter(|outcome| outcome.verdict == Verdict::Skipped)
            .map(|outcome| outcome.test_case_id)
            .collect::<Vec<_>>();
        assert_eq!(skipped_ids, vec![3, 4, 5, 6]);
        assert_eq!(batch_test_case_ids(&host), vec![vec![1, 2, 3, 4], vec![5]]);
    }

    #[test]
    fn sample_outside_failed_subtask_continues_judging() {
        let host = Host::mock();
        let mut tcs = (1..=6).map(test_case).collect::<Vec<_>>();
        tcs[5].is_sample = true;
        tcs[5].score = 0.0;
        let req = test_submission(tcs.clone());
        let subtasks = vec![crate::config::SubtaskDef {
            name: "Scoring".into(),
            scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
            max_score: 100.0,
            test_cases: (1..=5).map(|id| id.to_string()).collect(),
        }];
        host.eval.queue_result(TestCaseVerdict::wrong_answer(1));
        host.eval.queue_result(TestCaseVerdict::accepted(6));

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        let skipped_ids = outcomes
            .iter()
            .filter(|outcome| outcome.verdict == Verdict::Skipped)
            .map(|outcome| outcome.test_case_id)
            .collect::<Vec<_>>();
        assert_eq!(skipped_ids, vec![2, 3, 4, 5]);
        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome.test_case_id == 6 && outcome.verdict == Verdict::Accepted)
        );
        assert_eq!(batch_test_case_ids(&host), vec![vec![1, 2, 3, 4], vec![6]]);
    }

    #[test]
    fn sample_inside_failed_subtask_continues_judging() {
        let host = Host::mock();
        let mut tcs = (1..=5).map(test_case).collect::<Vec<_>>();
        tcs[4].is_sample = true;
        tcs[4].score = 0.0;
        let req = test_submission(tcs.clone());
        let subtasks = vec![crate::config::SubtaskDef {
            name: "ScoringWithSample".into(),
            scoring_method: crate::config::SubtaskScoringMethod::GroupMin,
            max_score: 100.0,
            test_cases: (1..=5).map(|id| id.to_string()).collect(),
        }];
        host.eval.queue_result(TestCaseVerdict::wrong_answer(1));
        host.eval.queue_result(TestCaseVerdict::accepted(5));

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &subtasks).unwrap();

        let skipped_ids = outcomes
            .iter()
            .filter(|outcome| outcome.verdict == Verdict::Skipped)
            .map(|outcome| outcome.test_case_id)
            .collect::<Vec<_>>();
        assert_eq!(skipped_ids, vec![2, 3, 4]);
        assert!(
            outcomes
                .iter()
                .any(|outcome| outcome.test_case_id == 5 && outcome.verdict == Verdict::Accepted)
        );
        assert_eq!(
            host.eval.testcase_cancels(),
            vec![("mock-batch-1".to_string(), vec![2, 3, 4])]
        );
    }

    /// UP#34 regression: 20 accepted testcases must produce strictly
    /// fewer than 20 `insert_results` calls. With
    /// `RESULT_BATCH_FLUSH_THRESHOLD = 8` we expect 3 calls (two full
    /// chunks of 8 + one tail flush of 4).
    #[test]
    fn bulk_inserts_batch_accepted_results() {
        let host = Host::mock();
        let tcs: Vec<TestCaseRow> = (1..=20).map(test_case).collect();
        let req = test_submission(tcs.clone());
        for i in 1..=20 {
            host.eval.queue_result(TestCaseVerdict::accepted(i));
        }

        let outcomes = evaluate_all(&host, &req, &tcs, 1, |raw, _| raw, &[]).unwrap();

        assert_eq!(outcomes.len(), 20);
        assert_eq!(host.submission.results().len(), 20);
        let calls = host.submission.insert_call_count();
        assert!(
            calls <= 3,
            "expected ≤3 bulk INSERTs for 20 testcases, got {calls}"
        );
        assert!(
            calls >= 2,
            "expected ≥2 bulk INSERTs (threshold = 8), got {calls}"
        );
    }
}
