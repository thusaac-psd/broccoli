// Only used by the #[cfg(test)] short-circuit test harness below (production
// evaluation never needs these); gate the import the same way to keep normal
// (non-test) builds from flagging it as unused.
#[cfg(test)]
use std::collections::{HashMap, HashSet};

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::evaluator::{
    CaseOutcome, ContestJudge, DetachedEval, JudgeProgress, JudgeStep,
};
use broccoli_server_sdk::types::*;
use serde::{Deserialize, Serialize};

/// Auto-flush threshold for buffered `TestCaseResultRow`s. With typical
/// ICPC contests in the 15-25 testcase range this yields 2-3 bulk
/// INSERTs per submission instead of one INSERT per testcase (UP#34).
#[cfg(test)]
const RESULT_BATCH_FLUSH_THRESHOLD: usize = 8;

/// Per-test-case outcome from evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalOutcome {
    pub test_case_id: i32,
    pub verdict: Verdict,
    pub time_used: Option<i32>,
    pub memory_used: Option<i32>,
    pub message: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

/// Result of the evaluation phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResult {
    pub outcomes: Vec<EvalOutcome>,
    pub is_compile_error: bool,
    /// True only if ALL test cases were evaluated and ALL returned Accepted.
    pub is_accepted: bool,
}

/// ICPC judging policy for the shared detached-evaluate driver.
///
/// ICPC is all-or-nothing: every case must be Accepted, and the batch
/// short-circuits on the first non-AC verdict. The policy is stateless - the
/// driver owns persistence, the epoch guards, the compile-error short-circuit,
/// and the refill hint - so this carries no fields.
#[derive(Serialize, Deserialize)]
struct IcpcJudge;

impl ContestJudge for IcpcJudge {
    fn score(&self, result: &TestCaseVerdict) -> CaseOutcome {
        // Binary scoring: 1.0 for AC, 0.0 otherwise. The driver's default
        // `db_score` persists exactly this, matching the legacy row score.
        let score = if result.verdict == Verdict::Accepted {
            1.0
        } else {
            0.0
        };
        CaseOutcome::from_verdict(result, score)
    }

    fn next_step(&mut self, progress: &JudgeProgress<'_>) -> JudgeStep {
        // First non-AC verdict fails the submission: skip the rest and finish.
        // (A compile error is handled by the driver before we are consulted.)
        match progress.last {
            Some(outcome) if outcome.verdict != Verdict::Accepted => JudgeStep::short_circuit(),
            _ => JudgeStep::Continue,
        }
    }

    fn finalize(&self, host: &Host, progress: &JudgeProgress<'_>) -> Result<(), SdkError> {
        let is_compile_error = progress
            .outcomes
            .iter()
            .any(|o| o.verdict == Verdict::CompileError);
        let is_accepted = !is_compile_error
            && progress
                .outcomes
                .iter()
                .all(|o| o.verdict == Verdict::Accepted);
        let eval = EvalResult {
            outcomes: progress
                .outcomes
                .iter()
                .map(eval_outcome_from_case)
                .collect(),
            is_compile_error,
            is_accepted,
        };
        crate::persist::persist_and_track(
            host,
            progress.request.submission_id,
            progress.request.judgement_id,
            progress.request.judge_epoch,
            &eval,
        )?;
        Ok(())
    }
}

/// Adapt the driver's shared `CaseOutcome` into ICPC's persistence-layer
/// `EvalOutcome` (which `persist_and_track` consumes). Output was already
/// stripped by the driver.
fn eval_outcome_from_case(outcome: &CaseOutcome) -> EvalOutcome {
    EvalOutcome {
        test_case_id: outcome.test_case_id,
        verdict: outcome.verdict.clone(),
        time_used: outcome.time_used,
        memory_used: outcome.memory_used,
        message: outcome.message.clone(),
        stdout: None,
        stderr: None,
    }
}

pub fn evaluate_short_circuit_detached(
    host: &Host,
    req: &OnSubmissionInput,
    test_cases: &[TestCaseRow],
    _submission_id: i32,
) -> Result<OnSubmissionOutput, SdkError> {
    DetachedEval::start(host, req, test_cases, IcpcJudge, "on_icpc_eval_result", 1)?;
    Ok(OnSubmissionOutput {
        success: true,
        error_message: None,
    })
}

pub fn handle_detached_eval_callback(
    host: &Host,
    input: DetachedEvaluateCallbackInput,
) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
    DetachedEval::<IcpcJudge>::handle_callback(host, input)
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

/// Best-effort recovery when a detached result callback fails mid-stream.
///
/// The detached callback (`on_icpc_eval_result`) persists results one window at
/// a time. If a single host call fails for a non-stale reason (transient DB
/// error, deadlock, a row the DB rejects), the callback's `?` would otherwise
/// abort the whole WASM call and strand the submission on "Judging" with no
/// terminal verdict. The driver funnels it to a terminal `SystemError` instead:
/// fill any unrecorded test cases and write the terminal submission update.
///
/// All steps are best-effort - if the DB is genuinely unreachable these also
/// fail, and the worker's submission-level retry re-drives judging later. A
/// `StaleEpoch` here is benign: a newer judgement already owns the rows, so the
/// epoch guards turn these writes into no-ops.
pub fn recover_detached_callback_error(host: &Host, state_value: &serde_json::Value) {
    DetachedEval::<IcpcJudge>::recover(host, state_value);
}

/// Evaluate test cases with short-circuit: cancel remaining on first non-AC verdict.
/// Unevaluated test cases are filled with `Skipped` (on failure short-circuit)
/// or `SystemError` (on timeout/error).
#[cfg(test)]
pub fn evaluate_short_circuit(
    host: &Host,
    req: &OnSubmissionInput,
    test_cases: &[TestCaseRow],
    submission_id: i32,
) -> Result<EvalResult, SdkError> {
    let batch_input = build_batch_input(req, test_cases);

    let tc_map: HashMap<i32, &TestCaseRow> = test_cases.iter().map(|tc| (tc.id, tc)).collect();

    let _ = host
        .submission
        .delete_results(submission_id, req.judgement_id, req.judge_epoch);

    let mut outcomes: Vec<EvalOutcome> = Vec::new();
    let mut recorded_ids: HashSet<i32> = HashSet::new();
    // Per-submission row buffer (UP#34). Filled by `record_outcome`,
    // periodically drained by the threshold flush inside that helper,
    // and force-flushed before any early return so the caller still
    // observes terminal verdicts in `test_case_result`.
    let mut row_buf: Vec<TestCaseResultRow> = Vec::new();

    // Try to start batch
    let mut session = match host.eval.windowed(&batch_input).concurrency(1).start() {
        Ok(session) => session,
        Err(e) => {
            for tc in test_cases {
                let outcome = EvalOutcome {
                    test_case_id: tc.id,
                    verdict: Verdict::SystemError,
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
                )?;
                outcomes.push(outcome);
            }
            flush_results(host, &mut row_buf)?;
            return Ok(EvalResult {
                outcomes,
                is_compile_error: false,
                is_accepted: false,
            });
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
        "ICPC: Started evaluate batch for {} test cases",
        test_cases.len()
    ));

    let mut collected = 0;
    let mut is_compile_error = false;
    let mut short_circuited = false;
    let mut marked_running = false;
    let result_timeout_ms = default_evaluation_result_timeout_ms(req.time_limit_ms);

    while collected < test_cases.len() {
        match session.next_result_with_refill_filter(result_timeout_ms, |verdict| {
            verdict.verdict == Verdict::Accepted
        }) {
            Ok(Some(verdict)) => {
                let outcome = EvalOutcome {
                    test_case_id: verdict.test_case_id,
                    verdict: verdict.verdict,
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

                if outcome.verdict.is_skipped_or_cancelled()
                    && recorded_ids.contains(&outcome.test_case_id)
                {
                    continue;
                }

                if outcome.verdict == Verdict::CompileError {
                    record_outcome(
                        host,
                        &mut row_buf,
                        submission_id,
                        req.judgement_id,
                        req.judge_epoch,
                        &outcome,
                        &tc_map,
                    )?;
                    recorded_ids.insert(outcome.test_case_id);
                    outcomes.push(outcome);
                    is_compile_error = true;
                    let _ = session.cancel_all();
                    short_circuited = true;
                    break;
                }

                let is_fail = outcome.verdict != Verdict::Accepted;

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
                )?;
                recorded_ids.insert(outcome.test_case_id);
                outcomes.push(outcome);
                collected += 1;

                // Short-circuit on first failure
                if is_fail {
                    let _ = session.cancel_all();
                    short_circuited = true;
                    break;
                }
            }
            Ok(None) => {
                let _ = host.log.info(&format!(
                    "Timeout waiting for result {}/{}",
                    collected + 1,
                    test_cases.len()
                ));
                short_circuited = true;
                break;
            }
            Err(e) => {
                let _ = host.log.info(&format!("Error polling result: {e:?}"));
                short_circuited = true;
                break;
            }
        }
    }

    // Fill remaining TCs
    if short_circuited {
        // If we short-circuited due to a test failure or CE, fill with Skipped.
        // If due to timeout/error, fill with SystemError.
        let is_known_failure = is_compile_error
            || outcomes
                .last()
                .is_some_and(|o| o.verdict != Verdict::Accepted);
        let (fill_verdict, fill_message) = if is_known_failure {
            (Verdict::Skipped, "SKIPPED_SHORT_CIRCUIT")
        } else {
            (Verdict::SystemError, "EVALUATION_TIMEOUT")
        };

        for tc in test_cases {
            if !recorded_ids.contains(&tc.id) {
                let outcome = EvalOutcome {
                    test_case_id: tc.id,
                    verdict: fill_verdict.clone(),
                    time_used: None,
                    memory_used: None,
                    message: Some(fill_message.into()),
                    stdout: None,
                    stderr: None,
                };
                // `record_outcome` may flush mid-fill if buf crosses the
                // threshold; that's fine - fill rows are independent.
                record_outcome(
                    host,
                    &mut row_buf,
                    submission_id,
                    req.judgement_id,
                    req.judge_epoch,
                    &outcome,
                    &tc_map,
                )?;
                recorded_ids.insert(tc.id);
                outcomes.push(outcome);
            }
        }

        if fill_verdict == Verdict::SystemError {
            let _ = session.cancel_all();
        }
    }

    // Final flush: drain any rows still in the buffer (typical case
    // when total testcases < threshold or the last partial chunk).
    flush_results(host, &mut row_buf)?;

    let is_accepted = !is_compile_error
        && !short_circuited
        && outcomes.iter().all(|o| o.verdict == Verdict::Accepted);

    Ok(EvalResult {
        outcomes,
        is_compile_error,
        is_accepted,
    })
}

/// Helper to build a minimal `OnSubmissionInput` for testing.
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
        problem_type: "standard".into(),
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
fn build_tc_row(
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
    outcome: &EvalOutcome,
    tc_map: &HashMap<i32, &TestCaseRow>,
) -> TestCaseResultRow {
    let tc = tc_map.get(&outcome.test_case_id);
    let is_custom = tc.is_some_and(|t| t.is_custom);
    let (tc_id, run_index) = if is_custom {
        (None, Some(outcome.test_case_id))
    } else {
        (Some(outcome.test_case_id), None)
    };
    // ICPC: binary scoring - 1.0 for AC, 0.0 otherwise
    let score = if outcome.verdict == Verdict::Accepted {
        1.0
    } else {
        0.0
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
/// `RESULT_BATCH_FLUSH_THRESHOLD`. Returns the error from the flush call
/// if any, so the caller's existing `?` continues to short-circuit on
/// `StaleEpoch` exactly as before.
#[cfg(test)]
fn record_outcome(
    host: &Host,
    buf: &mut Vec<TestCaseResultRow>,
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
    outcome: &EvalOutcome,
    tc_map: &HashMap<i32, &TestCaseRow>,
) -> Result<(), SdkError> {
    buf.push(build_tc_row(
        submission_id,
        judgement_id,
        judge_epoch,
        outcome,
        tc_map,
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
mod tests {
    use super::*;

    #[test]
    fn all_accepted() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(result.is_accepted);
        assert!(!result.is_compile_error);
        assert_eq!(result.outcomes.len(), 2);
        assert!(
            result
                .outcomes
                .iter()
                .all(|o| o.verdict == Verdict::Accepted)
        );
        assert!(!host.eval.was_cancelled());
        let updates = host.submission.updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::Running));
    }

    #[test]
    fn starts_single_case_initial_window_and_refills_after_result() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(result.is_accepted);
        let batch_test_case_ids = host
            .eval
            .batch_inputs()
            .into_iter()
            .map(|batch| {
                batch
                    .test_cases
                    .into_iter()
                    .map(|case| case.test_case_id)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_test_case_ids, vec![vec![1], vec![2]]);
    }

    #[test]
    fn stale_epoch_cancels_active_windowed_evaluation() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());
        host.submission.queue_update_result(Ok(0));

        let err = match evaluate_short_circuit(&host, &req, &tcs, 1) {
            Ok(_) => panic!("expected stale epoch"),
            Err(err) => err,
        };

        assert!(matches!(err, SdkError::StaleEpoch));
        let batch_test_case_ids = host
            .eval
            .batch_inputs()
            .into_iter()
            .map(|batch| {
                batch
                    .test_cases
                    .into_iter()
                    .map(|case| case.test_case_id)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_test_case_ids, vec![vec![1]]);
        assert!(host.eval.was_cancelled());
    }

    #[test]
    fn short_circuits_on_wrong_answer_and_fills_skipped() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2), test_case(3)];
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(2));
        // TC 3 never evaluated - batch cancelled after WA

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(!result.is_accepted);
        assert!(!result.is_compile_error);
        assert_eq!(result.outcomes.len(), 3);
        assert_eq!(result.outcomes[0].verdict, Verdict::Accepted);
        assert_eq!(result.outcomes[1].verdict, Verdict::WrongAnswer);
        assert_eq!(result.outcomes[2].verdict, Verdict::Skipped);
        let batch_test_case_ids = host
            .eval
            .batch_inputs()
            .into_iter()
            .map(|batch| {
                batch
                    .test_cases
                    .into_iter()
                    .map(|case| case.test_case_id)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_test_case_ids, vec![vec![1], vec![2]]);
    }

    #[test]
    fn compile_error_fills_skipped_not_system_error() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::compile_error(1));
        // TC 2 never evaluated

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(!result.is_accepted);
        assert!(result.is_compile_error);
        assert_eq!(result.outcomes.len(), 2);
        assert_eq!(result.outcomes[0].verdict, Verdict::CompileError);
        // CE fills remaining with Skipped (not SystemError)
        assert_eq!(result.outcomes[1].verdict, Verdict::Skipped);
        let batch_test_case_ids = host
            .eval
            .batch_inputs()
            .into_iter()
            .map(|batch| {
                batch
                    .test_cases
                    .into_iter()
                    .map(|case| case.test_case_id)
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(batch_test_case_ids, vec![vec![1]]);
        let updates = host.submission.updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
    }

    #[test]
    fn test_case_result_text_replaces_nul_bytes() {
        let host = Host::mock();
        let tcs = vec![test_case(1)];
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            time_used_ms: Some(10),
            memory_used_kb: Some(256),
            message: Some("bad\0message".into()),
            stdout: Some("out\0put".into()),
            stderr: Some("err\0or".into()),
        });

        evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        let rows = host.submission.results();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("bad\u{FFFD}message"));
        assert_eq!(rows[0].stdout.as_deref(), Some("out\u{FFFD}put"));
        assert_eq!(rows[0].stderr.as_deref(), Some("err\u{FFFD}or"));
    }

    #[test]
    fn polling_error_fills_system_error() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());
        host.eval
            .queue_result_error(SdkError::Other("poll failed".into()));

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(!result.is_accepted);
        assert!(!result.is_compile_error);
        assert_eq!(result.outcomes.len(), 2);
        assert_eq!(result.outcomes[0].verdict, Verdict::SystemError);
        assert_eq!(result.outcomes[1].verdict, Verdict::SystemError);
    }

    #[test]
    fn ignores_late_cancelled_for_already_recorded_test_case() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::Cancelled,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: Some("Cancelled by host".into()),
            stdout: None,
            stderr: None,
        });
        host.eval.queue_result(TestCaseVerdict::accepted(2));

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(result.is_accepted);
        assert_eq!(result.outcomes.len(), 2);

        let rows = host.submission.results();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].test_case_id, Some(1));
        assert_eq!(rows[0].verdict, Verdict::Accepted);
        assert_eq!(rows[1].test_case_id, Some(2));
        assert_eq!(rows[1].verdict, Verdict::Accepted);
    }

    #[test]
    fn polls_ready_window_without_waiting() {
        let host = Host::mock();
        let tcs = vec![test_case(1)];
        let mut req = test_submission(tcs.clone());
        req.time_limit_ms = 300_000;
        host.eval.queue_result(TestCaseVerdict::accepted(1));

        evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert_eq!(host.eval.result_timeouts(), vec![0]);
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

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(result.is_accepted);
        assert_eq!(host.submission.results().len(), 20);
        // 20 / 8 = 2 full chunks + 1 tail = 3 INSERTs total.
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

    /// UP#34 regression: a short-circuited failure should still take at
    /// most 2 INSERTs (one for the partial run, one tail flush for the
    /// fill rows).
    #[test]
    fn bulk_inserts_short_circuit_uses_at_most_two_calls() {
        let host = Host::mock();
        let tcs: Vec<TestCaseRow> = (1..=15).map(test_case).collect();
        let req = test_submission(tcs.clone());
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(2));

        let result = evaluate_short_circuit(&host, &req, &tcs, 1).unwrap();

        assert!(!result.is_accepted);
        assert_eq!(host.submission.results().len(), 15);
        let calls = host.submission.insert_call_count();
        assert!(
            calls <= 2,
            "expected ≤2 bulk INSERTs for short-circuit + fill, got {calls}"
        );
    }
}

/// Direct contract tests for the ICPC path over the shared detached-evaluate
/// driver: they drive `handle_detached_eval_callback` with synthesized result
/// events and assert the persisted rows, the terminal submission update, and the
/// driver's action/idempotency/output-strip guarantees end to end.
#[cfg(test)]
mod detached_tests {
    use super::*;

    fn start_state(host: &Host, req: &OnSubmissionInput, tcs: &[TestCaseRow]) -> serde_json::Value {
        evaluate_short_circuit_detached(host, req, tcs, req.submission_id).unwrap();
        host.eval.detached_windowed_requests()[0].state.clone()
    }

    fn drive(
        host: &Host,
        state: serde_json::Value,
        result: TestCaseVerdict,
    ) -> DetachedEvaluateCallbackOutput {
        handle_detached_eval_callback(
            host,
            DetachedEvaluateCallbackInput {
                session_id: "icpc-detached".into(),
                state,
                event: DetachedEvaluateCallbackEvent::Result { result },
                completed: 0,
                total: 0,
            },
        )
        .unwrap()
    }

    fn row_verdict(host: &Host, id: i32) -> Verdict {
        host.submission
            .results()
            .into_iter()
            .find(|r| r.test_case_id == Some(id))
            .unwrap_or_else(|| panic!("no row for test case {id}"))
            .verdict
    }

    #[test]
    fn all_accepted_finishes_and_scores_one() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());

        let s0 = start_state(&host, &req, &tcs);
        let out1 = drive(&host, s0, TestCaseVerdict::accepted(1));
        assert_eq!(out1.action, DetachedEvaluateCallbackAction::Continue);
        let out2 = drive(&host, out1.state, TestCaseVerdict::accepted(2));
        assert_eq!(out2.action, DetachedEvaluateCallbackAction::Finish);

        assert_eq!(host.submission.results().len(), 2);
        let update = host.submission.last_update();
        assert_eq!(update.verdict, Some(Some(Verdict::Accepted)));
        assert_eq!(update.score, Some(1.0));
    }

    #[test]
    fn wrong_answer_short_circuits_and_fills_skipped() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2), test_case(3)];
        let req = test_submission(tcs.clone());

        let s0 = start_state(&host, &req, &tcs);
        let out1 = drive(&host, s0, TestCaseVerdict::accepted(1));
        let out2 = drive(&host, out1.state, TestCaseVerdict::wrong_answer(2));
        assert_eq!(out2.action, DetachedEvaluateCallbackAction::Finish);

        assert_eq!(host.submission.results().len(), 3);
        assert_eq!(row_verdict(&host, 1), Verdict::Accepted);
        assert_eq!(row_verdict(&host, 2), Verdict::WrongAnswer);
        assert_eq!(row_verdict(&host, 3), Verdict::Skipped);
        let update = host.submission.last_update();
        assert_eq!(update.verdict, Some(Some(Verdict::WrongAnswer)));
        assert_eq!(update.score, Some(0.0));
    }

    #[test]
    fn compile_error_fills_skipped_without_marking_running() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());

        let s0 = start_state(&host, &req, &tcs);
        let out = drive(&host, s0, TestCaseVerdict::compile_error(1));
        assert_eq!(out.action, DetachedEvaluateCallbackAction::Finish);

        assert_eq!(row_verdict(&host, 1), Verdict::CompileError);
        assert_eq!(row_verdict(&host, 2), Verdict::Skipped);
        let update = host.submission.last_update();
        assert_eq!(update.status, Some(SubmissionStatus::CompilationError));
        assert!(
            host.submission
                .updates()
                .iter()
                .all(|u| u.status != Some(SubmissionStatus::Running)),
            "a compile error must never flip the submission to Running"
        );
    }

    #[test]
    fn late_cancelled_for_recorded_case_is_ignored_not_double_inserted() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());

        let s0 = start_state(&host, &req, &tcs);
        let out1 = drive(&host, s0, TestCaseVerdict::accepted(1));
        let cancelled = TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::Cancelled,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: Some("late cancel".into()),
            stdout: None,
            stderr: None,
        };
        let out2 = drive(&host, out1.state, cancelled);
        assert_eq!(out2.action, DetachedEvaluateCallbackAction::Continue);
        let out3 = drive(&host, out2.state, TestCaseVerdict::accepted(2));
        assert_eq!(out3.action, DetachedEvaluateCallbackAction::Finish);

        let rows = host.submission.results();
        assert_eq!(
            rows.len(),
            2,
            "late cancelled must not double-insert case 1"
        );
        assert!(rows.iter().all(|r| r.verdict == Verdict::Accepted));
    }

    #[test]
    fn output_is_persisted_to_row_but_stripped_from_round_tripped_state() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2)];
        let req = test_submission(tcs.clone());

        let s0 = start_state(&host, &req, &tcs);
        let verdict = TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::Accepted,
            score: 1.0,
            time_used_ms: Some(5),
            memory_used_kb: Some(256),
            message: Some("msg".into()),
            stdout: Some("STDOUT-BYTES".into()),
            stderr: Some("STDERR-BYTES".into()),
        };
        let out = drive(&host, s0, verdict);

        // The DB row keeps the output the frontend renders...
        let row = host
            .submission
            .results()
            .into_iter()
            .find(|r| r.test_case_id == Some(1))
            .unwrap();
        assert_eq!(row.stdout.as_deref(), Some("STDOUT-BYTES"));
        assert_eq!(row.stderr.as_deref(), Some("STDERR-BYTES"));

        // ...but the state re-serialized across the host boundary on every
        // callback does NOT (the O(n^2) round-trip blow-up guard).
        let outcome0 = &out.state["outcomes"][0];
        assert_eq!(outcome0["test_case_id"].as_i64(), Some(1));
        assert!(
            outcome0["stdout"].is_null(),
            "stdout must be stripped from retained state, got {:?}",
            outcome0["stdout"]
        );
        assert!(
            outcome0["stderr"].is_null(),
            "stderr must be stripped from retained state, got {:?}",
            outcome0["stderr"]
        );
    }
}
