use broccoli_server_sdk::prelude::*;
use codelink::judge::{handle_eval_callback, judge};
use serde_json::Value;

fn request() -> OnSubmissionInput {
    serde_json::from_value(serde_json::json!({
        "submission_id": 42, "judgement_id": 84, "judge_epoch": 3,
        "user_id": 1, "problem_id": 2, "contest_id": 3,
        "files": [{"filename": "main.cpp", "content": "int main() {}"}],
        "language": "cpp", "time_limit_ms": 1000, "memory_limit_kb": 65536,
        "problem_type": "batch", "target_worker_id": "worker-a",
        "test_cases": [
            {"id": 1, "score": 100, "position": 1, "is_sample": false},
            {"id": 2, "score": 100, "position": 2, "is_sample": false}
        ]
    }))
    .unwrap()
}

fn start(host: &Host) -> Value {
    judge(host, &request()).unwrap();
    host.eval.detached_windowed_requests()[0].state.clone()
}

fn callback(
    host: &Host,
    state: Value,
    event: DetachedEvaluateCallbackEvent,
) -> DetachedEvaluateCallbackOutput {
    handle_eval_callback(
        host,
        DetachedEvaluateCallbackInput {
            session_id: "codelink-test".into(),
            state,
            event,
            completed: host.submission.results().len(),
            total: request().test_cases.len(),
        },
    )
}

fn result(host: &Host, state: Value, result: TestCaseVerdict) -> DetachedEvaluateCallbackOutput {
    callback(
        host,
        state,
        DetachedEvaluateCallbackEvent::Result { result },
    )
}

#[test]
fn ac_persists_only_after_callbacks_and_forwards_worker_and_epoch() {
    let host = Host::mock();
    let initial = start(&host);
    assert_eq!(
        host.submission.last_update().status,
        Some(SubmissionStatus::Compiling)
    );
    assert!(host.submission.results().is_empty());
    let first = result(&host, initial, TestCaseVerdict::accepted(2));
    assert_eq!(first.action, DetachedEvaluateCallbackAction::Continue);
    assert!(first.refill);
    let last = result(&host, first.state, TestCaseVerdict::accepted(1));
    assert_eq!(last.action, DetachedEvaluateCallbackAction::Finish);
    let update = host.submission.last_update();
    assert_eq!(update.verdict, Some(Some(Verdict::Accepted)));
    assert_eq!(update.score, Some(1.0));
    assert_eq!(update.judgement_id, 84);
    assert_eq!(update.judge_epoch, 3);
    assert_eq!(host.submission.results().len(), 2);
    assert!(
        host.submission
            .results()
            .iter()
            .all(|r| r.judgement_id == 84 && r.judge_epoch == 3)
    );
    let dispatched = &host.eval.detached_windowed_requests()[0];
    assert_eq!(dispatched.callback_fn, "on_codelink_eval_result");
    assert!(
        dispatched
            .batch
            .test_cases
            .iter()
            .all(|tc| tc.target_worker_id.as_deref() == Some("worker-a"))
    );
    assert!(
        dispatched
            .submission_completion
            .as_ref()
            .unwrap()
            .fire_after_judging
    );
}

#[test]
fn unapplied_rejudge_keeps_after_judging_hook_disabled() {
    let host = Host::mock();
    let mut req = request();
    req.fire_after_judging = false;
    judge(&host, &req).unwrap();
    let completion = host.eval.detached_windowed_requests()[0]
        .submission_completion
        .clone()
        .unwrap();
    assert!(!completion.fire_after_judging);
    assert_eq!(
        (
            completion.submission_id,
            completion.judgement_id,
            completion.judge_epoch
        ),
        (42, 84, 3)
    );
}

#[test]
fn failure_finishes_batch_and_awards_no_score() {
    let host = Host::mock();
    let output = result(&host, start(&host), TestCaseVerdict::wrong_answer(1));
    assert_eq!(output.action, DetachedEvaluateCallbackAction::Finish);
    assert!(!output.refill);
    assert_eq!(host.submission.last_update().score, Some(0.0));
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::WrongAnswer))
    );
    assert_eq!(host.submission.results()[1].verdict, Verdict::Skipped);
}

#[test]
fn compile_errors_do_not_become_accepted() {
    let host = Host::mock();
    result(&host, start(&host), TestCaseVerdict::compile_error(1));
    assert_eq!(
        host.submission.last_update().status,
        Some(SubmissionStatus::CompilationError)
    );
    assert_eq!(host.submission.last_update().verdict, Some(None));
    assert_eq!(host.submission.last_update().score, Some(0.0));
}

#[test]
fn empty_tests_and_incomplete_evaluation_cannot_grant_an_ac() {
    let host = Host::mock();
    let mut empty = request();
    empty.test_cases.clear();
    judge(&host, &empty).unwrap();
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::SystemError))
    );
    assert!(host.eval.detached_windowed_requests().is_empty());

    let first = result(&host, start(&host), TestCaseVerdict::accepted(1));
    let end = callback(&host, first.state, DetachedEvaluateCallbackEvent::Exhausted);
    assert_eq!(end.action, DetachedEvaluateCallbackAction::Cancel);
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::SystemError))
    );
    assert_eq!(host.submission.last_update().score, Some(0.0));
}

#[test]
fn duplicate_delivery_is_idempotent_and_does_not_replace_a_missing_test() {
    let host = Host::mock();
    let first = result(&host, start(&host), TestCaseVerdict::accepted(1));
    let repeated = result(&host, first.state, TestCaseVerdict::accepted(1));
    assert_eq!(repeated.action, DetachedEvaluateCallbackAction::Continue);
    assert_eq!(host.submission.results().len(), 1);
    assert_eq!(
        host.submission.last_update().status,
        Some(SubmissionStatus::Running)
    );
    result(&host, repeated.state, TestCaseVerdict::accepted(2));
    assert_eq!(host.submission.results().len(), 2);
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::Accepted))
    );
}

#[test]
fn stale_epochs_stop_initial_dispatch_and_callbacks() {
    let host = Host::mock();
    host.submission.queue_update_result(Ok(0));
    assert!(matches!(
        judge(&host, &request()),
        Err(SdkError::StaleEpoch)
    ));
    assert!(host.eval.detached_windowed_requests().is_empty());

    let initial = start(&host);
    host.submission.queue_update_result(Ok(0));
    let stopped = result(&host, initial, TestCaseVerdict::accepted(1));
    assert_eq!(stopped.action, DetachedEvaluateCallbackAction::Cancel);
    assert!(host.submission.results().is_empty());
}

#[test]
fn timeout_reaches_a_terminal_error() {
    let host = Host::mock();
    callback(
        &host,
        start(&host),
        DetachedEvaluateCallbackEvent::Timeout {
            message: "timeout".into(),
        },
    );
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::SystemError))
    );
    assert_eq!(host.submission.results().len(), 2);
}

#[test]
fn callback_persistence_failure_is_recovered_instead_of_stranding_submission() {
    let host = Host::mock();
    let initial = start(&host);
    host.submission
        .queue_insert_error(SdkError::Other("persist failed".into()));
    let output = result(&host, initial, TestCaseVerdict::accepted(1));
    assert_eq!(output.action, DetachedEvaluateCallbackAction::Cancel);
    assert_eq!(
        host.submission.last_update().status,
        Some(SubmissionStatus::Judged)
    );
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::SystemError))
    );
}

#[test]
fn partial_acceptance_with_a_cancelled_case_never_receives_a_slot() {
    let host = Host::mock();
    let first = result(&host, start(&host), TestCaseVerdict::accepted(1));
    let mut cancelled = TestCaseVerdict::accepted(2);
    cancelled.verdict = Verdict::Cancelled;
    result(&host, first.state, cancelled);
    assert_eq!(
        host.submission.last_update().verdict,
        Some(Some(Verdict::SystemError))
    );
    assert_eq!(host.submission.last_update().score, Some(0.0));
}
