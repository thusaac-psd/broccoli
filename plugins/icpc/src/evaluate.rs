use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::evaluator::{
    CaseOutcome, ContestJudge, DetachedEval, JudgeProgress, JudgeStep,
};
use broccoli_server_sdk::types::*;
use serde::{Deserialize, Serialize};

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
        // `db_score` persists exactly this.
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
            progress.submission.submission_id,
            progress.submission.judgement_id,
            progress.submission.judge_epoch,
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
    Ok(OnSubmissionOutput::ok())
}

pub fn handle_detached_eval_callback(
    host: &Host,
    input: DetachedEvaluateCallbackInput,
) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
    DetachedEval::<IcpcJudge>::handle_callback(host, input)
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

/// Contract tests for the ICPC policy over the shared detached-evaluate driver:
/// they drive `handle_detached_eval_callback` with synthesized result events and
/// assert the persisted rows, the terminal submission update, and the driver's
/// action/idempotency/output-strip guarantees end to end.
#[cfg(test)]
mod tests {
    use super::*;

    /// Helper to build a minimal `OnSubmissionInput` for testing.
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

    fn start_state(host: &Host, req: &OnSubmissionInput, tcs: &[TestCaseRow]) -> serde_json::Value {
        evaluate_short_circuit_detached(host, req, tcs, req.submission_id).unwrap();
        host.eval.detached_windowed_requests()[0].state.clone()
    }

    /// The session state is copied into the host and back on every result
    /// callback, so it must not carry test data or the source: those belong
    /// to the evaluate batch alone. Before this, each callback re-parsed and
    /// re-serialized the whole inline test data (measured ~0.33 s per case at
    /// 50 x 150 KB), and it drove most of the guest's memory amplification.
    #[test]
    fn session_state_carries_no_test_bodies_or_source() {
        let host = Host::mock();
        let body = "X".repeat(100_000);
        let tcs: Vec<TestCaseRow> = (1..=5)
            .map(|id| TestCaseRow {
                input: TestCaseBodyRef::Inline {
                    text: format!("in{id}{body}"),
                },
                expected_output: TestCaseBodyRef::Inline {
                    text: format!("out{id}{body}"),
                },
                ..test_case(id)
            })
            .collect();
        let req = test_submission(tcs.clone());

        let state = start_state(&host, &req, &tcs);
        let state_json = serde_json::to_string(&state).unwrap();
        assert!(
            state_json.len() < 8_000,
            "state is {} bytes; test bodies leaked into it",
            state_json.len()
        );
        assert!(!state_json.contains("XXXX"), "a test body is in the state");
        assert!(
            !state_json.contains("int main"),
            "the source is in the state"
        );

        // The batch the host judges from still has every body.
        let batch = &host.eval.detached_windowed_requests()[0].batch;
        assert_eq!(batch.test_cases.len(), 5);
        assert!(serde_json::to_string(batch).unwrap().len() > 5 * 200_000);

        // And the driver still works from the stripped state.
        let out = drive(&host, state, TestCaseVerdict::accepted(1));
        assert!(!out.state.is_null());
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

    #[test]
    fn judges_one_case_at_a_time() {
        let host = Host::mock();
        let tcs = vec![test_case(1), test_case(2), test_case(3)];
        let req = test_submission(tcs.clone());
        start_state(&host, &req, &tcs);
        assert_eq!(host.eval.detached_windowed_requests()[0].concurrency, 1);
    }

    #[test]
    fn result_text_nul_bytes_are_replaced_in_the_persisted_row() {
        let host = Host::mock();
        let tcs = vec![test_case(1)];
        let req = test_submission(tcs.clone());
        let s0 = start_state(&host, &req, &tcs);
        drive(
            &host,
            s0,
            TestCaseVerdict {
                test_case_id: 1,
                verdict: Verdict::WrongAnswer,
                score: 0.0,
                time_used_ms: Some(10),
                memory_used_kb: Some(256),
                message: Some("bad\0message".into()),
                stdout: Some("out\0put".into()),
                stderr: Some("err\0or".into()),
            },
        );
        let rows = host.submission.results();
        assert_eq!(rows[0].message.as_deref(), Some("bad\u{FFFD}message"));
        assert_eq!(rows[0].stdout.as_deref(), Some("out\u{FFFD}put"));
        assert_eq!(rows[0].stderr.as_deref(), Some("err\u{FFFD}or"));
    }
}
