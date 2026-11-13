use std::sync::Arc;

use broccoli_server_sdk::types::{
    OperationResult, OperationTask, SourceFile, StartEvaluateCaseInput,
};
use common::storage::BlobStore;
use common::storage::filesystem::FilesystemBlobStore;
use common::worker::TaskResult;

use super::*;

fn open_batch(
    batches: &EvaluateBatches,
    batch_id: &str,
    case_ids: &[i32],
) -> BatchSender<TestCaseVerdict> {
    batches.open(
        batch_id,
        case_ids.iter().map(|id| id.to_string()),
        EvaluateCases::new(case_ids.iter().copied()),
    )
}

#[test]
fn giveup_drains_verdict_buffered_before_the_pending_decrement() {
    // Each verdict is buffered BEFORE the pending count drops, so a give-up
    // branch can observe the batch as settled with the verdict already in
    // the channel. The drain must deliver it, never drop it into a spurious
    // timeout -> SystemError.
    let batches = EvaluateBatches::default();
    let tx = open_batch(&batches, "eval-drain-buffered", &[7]);
    tx.deliver(system_error_verdict(7, "buffered"));

    let delivered = evaluate_wait("plugin", &batches, "eval-drain-buffered", Duration::ZERO)
        .unwrap()
        .give_up();
    let verdict = delivered.expect("a buffered verdict must be drained, not dropped");
    assert_eq!(verdict.test_case_id, 7);
}

#[test]
fn giveup_drain_is_none_when_channel_genuinely_empty() {
    let batches = EvaluateBatches::default();
    let _tx = open_batch(&batches, "eval-drain-empty", &[7]);

    assert!(
        evaluate_wait("plugin", &batches, "eval-drain-empty", Duration::ZERO)
            .unwrap()
            .give_up()
            .is_none()
    );
}

#[test]
fn detached_evaluate_completion_hooks_only_follow_terminal_callback_actions() {
    let input = StartDetachedWindowedEvaluateInput {
        batch: StartEvaluateBatchInput {
            problem_type: "icpc".into(),
            test_cases: Vec::new(),
        },
        concurrency: 1,
        result_timeout_ms: 1000,
        callback_fn: "on_result".into(),
        state: serde_json::json!({}),
        submission_completion: Some(broccoli_server_sdk::types::DetachedSubmissionCompletion {
            submission_id: 42,
            judgement_id: 7,
            judge_epoch: 9,
            fire_after_judging: true,
        }),
    };
    let completion = input.submission_completion.as_ref();

    assert_eq!(
        detached_evaluate_completion(completion, DetachedEvaluateCallbackAction::Finish)
            .map(|completion| completion.submission_id),
        Some(42)
    );
    assert_eq!(
        detached_evaluate_completion(completion, DetachedEvaluateCallbackAction::Cancel)
            .map(|completion| completion.judgement_id),
        Some(7)
    );
    assert_eq!(
        detached_evaluate_completion(completion, DetachedEvaluateCallbackAction::Continue),
        None
    );

    let muted = broccoli_server_sdk::types::DetachedSubmissionCompletion {
        submission_id: 42,
        judgement_id: 7,
        judge_epoch: 9,
        fire_after_judging: false,
    };
    assert_eq!(
        detached_evaluate_completion(Some(&muted), DetachedEvaluateCallbackAction::Finish),
        None
    );
}

#[test]
fn delivered_verdict_completes_its_case_and_drains_the_batch() {
    let batches = EvaluateBatches::default();
    let tx = open_batch(&batches, "eval-1", &[10, 11]);
    tx.deliver(system_error_verdict(10, "done"));

    let got = next_evaluate_result("plugin", &batches, "eval-1", Duration::ZERO).unwrap();
    assert_eq!(got.map(|v| v.test_case_id), Some(10));
    assert!(batches.contains("eval-1"), "case 11 is still pending");
    assert_eq!(
        batches.cancel_cases("eval-1", &[10, 11]),
        1,
        "case 10 has its verdict"
    );

    tx.deliver(system_error_verdict(11, "done"));
    next_evaluate_result("plugin", &batches, "eval-1", Duration::ZERO).unwrap();
    assert!(!batches.contains("eval-1"));
}

#[test]
fn next_result_zero_timeout_is_nonblocking_even_when_a_case_is_waiting() {
    let batches = EvaluateBatches::default();
    let _tx = open_batch(&batches, "eval-timeout-zero", &[10]);
    assert!(
        next_evaluate_result("plugin", &batches, "eval-timeout-zero", Duration::ZERO)
            .unwrap()
            .is_none(),
        "timeout=0 must preserve the SDK's nonblocking poll contract"
    );
}

#[test]
fn dispatch_guard_delivers_for_unclaimed_cases_on_drop() {
    let batches = EvaluateBatches::default();
    let tx = open_batch(&batches, "eval-guard", &[10, 20, 30]);
    {
        let mut guard = DispatchGuard {
            pending: vec![10, 20, 30],
            batch_tx: tx.clone(),
        };
        // Worker spawned for tc_id=10 -> guard no longer owns it.
        guard.claim(10);
        // Simulate dispatcher unwind: drop guard with [20, 30] still pending.
    }
    let mut seen = Vec::new();
    while let Some(verdict) =
        next_evaluate_result("plugin", &batches, "eval-guard", Duration::ZERO).unwrap()
    {
        seen.push(verdict.test_case_id);
    }
    seen.sort();
    assert_eq!(seen, vec![20, 30]);
    assert!(batches.contains("eval-guard"), "case 10 is still owed");
}

#[test]
fn dispatch_guard_with_nothing_pending_delivers_nothing() {
    let batches = EvaluateBatches::default();
    let tx = open_batch(&batches, "eval-guard", &[10]);
    drop(DispatchGuard {
        pending: Vec::new(),
        batch_tx: tx.clone(),
    });
    assert!(
        next_evaluate_result("plugin", &batches, "eval-guard", Duration::ZERO)
            .unwrap()
            .is_none()
    );
}

#[test]
fn evaluator_semaphore_wait_metric_records_success_and_closed_outcomes() {
    let _guard = crate::metrics_test_lock();
    let (metrics, registry) =
        common::observability::init_metrics("broccoli-evaluator-semaphore-test");

    record_evaluator_semaphore_wait(
        Some(&metrics),
        "evaluator",
        "evaluate",
        "ioi",
        "success",
        Duration::from_millis(3),
    );
    record_evaluator_semaphore_wait(
        Some(&metrics),
        "evaluator",
        "evaluate",
        "ioi",
        "closed",
        Duration::from_millis(4),
    );

    let families = registry.gather();
    let wait_duration = families
        .iter()
        .find(|family| family.name() == "broccoli_plugin_evaluator_semaphore_wait_duration_seconds")
        .expect("evaluator semaphore wait duration should be exported");
    for outcome in ["success", "closed"] {
        assert!(
            wait_duration.get_metric().iter().any(|metric| metric
                .get_label()
                .iter()
                .any(|label| label.name() == "outcome" && label.value() == outcome)),
            "evaluator semaphore wait should include outcome={outcome}"
        );
    }
}

#[test]
fn callback_path_tags_operation_tasks_with_evaluate_context() {
    let mut ops = vec![OperationTask {
        environments: vec![],
        tasks: vec![],
        channels: vec![],
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    }];

    tag_operation_tasks_for_evaluate_batch(&mut ops, "eval-batch-1", 42);

    assert_eq!(ops[0].evaluate_batch_id.as_deref(), Some("eval-batch-1"));
    assert_eq!(ops[0].test_case_id, Some(42));
}

#[test]
fn callback_path_decodes_operation_results_from_task_result_output() {
    let operation_result = OperationResult {
        success: true,
        task_results: Default::default(),
        error: None,
    };
    let task_result = TaskResult {
        task_id: "op-1".to_string(),
        success: true,
        output: serde_json::to_value(&operation_result).unwrap(),
        error: None,
        task_type: Some("operation".to_string()),
        operation: Some("operation".to_string()),
        worker_id: Some("worker-1".to_string()),
    };

    let decoded = operation_results_from_task_results(vec![task_result]).unwrap();

    assert_eq!(decoded.len(), 1);
    assert!(decoded[0].success);
}

#[test]
fn callback_path_rejects_task_result_without_operation_output() {
    let task_result = TaskResult {
        task_id: "op-1".to_string(),
        success: false,
        output: serde_json::Value::Null,
        error: Some("worker failed".to_string()),
        task_type: Some("operation".to_string()),
        operation: Some("operation".to_string()),
        worker_id: Some("worker-1".to_string()),
    };

    let err = operation_results_from_task_results(vec![task_result]).unwrap_err();

    assert!(
        err.to_string().contains("worker failed"),
        "worker error should be preserved in callback decode error: {err}"
    );
}

#[test]
fn callback_coalescing_uses_chunks_of_four() {
    let mut pending = Vec::new();
    let mut chunks = Vec::new();
    for id in 1..=10 {
        if let Some(chunk) = push_ready_case_for_callback(&mut pending, id) {
            chunks.push(chunk);
        }
    }
    if !pending.is_empty() {
        chunks.push(pending);
    }

    assert_eq!(
        chunks,
        vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10]]
    );
}

fn case(problem_id: i32, language: &str) -> StartEvaluateCaseInput {
    StartEvaluateCaseInput {
        problem_id,
        test_case_id: 1,
        solution_source: vec![SourceFile {
            filename: "main.cpp".to_string(),
            content: "int main() {}".to_string(),
        }],
        solution_language: language.to_string(),
        time_limit_ms: 1000,
        memory_limit_kb: 262_144,
        contest_id: None,
        input: TestCaseBodyRef::Missing,
        expected_output: TestCaseBodyRef::Missing,
        is_custom: false,
        target_worker_id: None,
    }
}

#[test]
fn cheap_bodies_are_retained_for_retry() {
    // Blob/Missing carry only a hash - always cheap to keep for the whole
    // submission so the op can be re-dispatched on timeout.
    assert!(body_cheap_to_retain(&TestCaseBodyRef::Missing));
    assert!(body_cheap_to_retain(&TestCaseBodyRef::blob("deadbeef")));
    // A small inline body is fine too.
    assert!(body_cheap_to_retain(&TestCaseBodyRef::inline(
        "a".repeat(1024)
    )));
    // At exactly the threshold it is still retained.
    assert!(body_cheap_to_retain(&TestCaseBodyRef::inline(
        "x".repeat(INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES)
    )));
}

#[test]
fn large_inline_bodies_are_not_retained_for_retry() {
    // A large inline body (database-backend path) must NOT be held for the
    // submission's lifetime - that would re-introduce the result-set memory
    // blow-up. Such a case falls through to the existing timeout behavior.
    let huge = TestCaseBodyRef::inline("x".repeat(INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES + 1));
    assert!(!body_cheap_to_retain(&huge));

    let mut tc = case(1, "cpp");
    tc.input = huge;
    assert!(!test_case_cheap_to_retain(&tc));
}

#[test]
fn test_case_retainable_only_when_both_bodies_cheap() {
    let mut tc = case(1, "cpp");
    // Both Missing by default - retainable.
    assert!(test_case_cheap_to_retain(&tc));

    // Cheap input + cheap expected output - retainable.
    tc.input = TestCaseBodyRef::blob("in");
    tc.expected_output = TestCaseBodyRef::blob("out");
    assert!(test_case_cheap_to_retain(&tc));

    // A large expected_output alone disqualifies retention.
    tc.expected_output =
        TestCaseBodyRef::inline("y".repeat(INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES + 1));
    assert!(!test_case_cheap_to_retain(&tc));
}

#[test]
fn validate_batch_shape_accepts_empty_batch() {
    assert!(validate_batch_shape(&[]).unwrap().is_none());
}

#[test]
fn validate_batch_shape_rejects_mixed_problem_ids() {
    let cases = vec![case(1, "cpp"), case(2, "cpp")];
    let err = validate_batch_shape(&cases).unwrap_err();
    assert!(
        err.to_string()
            .contains("All test cases in a batch must belong to the same problem")
    );
}

#[test]
fn validate_batch_shape_rejects_mixed_languages() {
    let cases = vec![case(1, "cpp"), case(1, "python")];
    let err = validate_batch_shape(&cases).unwrap_err();
    assert!(
        err.to_string()
            .contains("All test cases in a batch must use the same solution_language")
    );
}

#[tokio::test]
async fn resolve_evaluate_body_keeps_small_inline_body_inline() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        FilesystemBlobStore::new(temp.path().to_path_buf(), 2_000_000)
            .await
            .unwrap(),
    );

    let resolved = resolve_evaluate_body(
        TestCaseBodyRef::inline("hello"),
        None,
        None,
        "input.txt",
        "test",
        store,
    )
    .await
    .unwrap();

    assert!(resolved.present);
    assert_eq!(resolved.file.inline_text(), "hello");
}

#[tokio::test]
async fn resolve_evaluate_body_externalizes_large_inline_body() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(
        FilesystemBlobStore::new(temp.path().to_path_buf(), 2_000_000)
            .await
            .unwrap(),
    );
    let content = "x".repeat(INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES);

    let resolved = resolve_evaluate_body(
        TestCaseBodyRef::inline(content.clone()),
        None,
        None,
        "input.txt",
        "test",
        store.clone(),
    )
    .await
    .unwrap();

    let JudgeFile::Blob { file } = resolved.file else {
        panic!("large inline body should be externalized");
    };
    assert!(resolved.present);
    assert!(
        store
            .exists(&ContentHash::compute(content.as_bytes()))
            .await
            .unwrap()
    );
    assert_eq!(file.filename, "input.txt");
}
