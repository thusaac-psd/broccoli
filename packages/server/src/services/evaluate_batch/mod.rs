use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use broccoli_server_sdk::types::{
    BuildEvalOpsInput, DetachedEvaluateCallbackAction, DetachedEvaluateCallbackEvent,
    DetachedEvaluateCallbackInput, DetachedEvaluateCallbackOutput, DetachedEvaluateSession,
    EvaluateOperationResultInput, EvaluateOperationResultsInput, FileRef, JudgeFile,
    OperationResult, OperationTask, PreparedEvaluateCase, StartDetachedWindowedEvaluateInput,
    StartEvaluateBatchInput, StartEvaluateCaseInput, TestCaseBodyRef, TestCaseVerdict,
    Verdict as SdkVerdict,
};
use common::storage::{BlobStore, ContentHash};
use opentelemetry::KeyValue;
use plugin_core::retry::{PoolRetryPolicy, call_raw_with_pool_retry};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

use crate::entity::{additional_file, plugin_config, problem, test_case};
use crate::host_funcs::context::EvaluateHostDeps;
use crate::registry::{EvaluateBatches, EvaluatorFunctions};
use crate::services::batch_table::{BatchSender, Removal};
use crate::services::batch_wait::{BatchWait, WaitCtx, WaitPolicy};
use crate::services::submission_dispatch::fire_after_judging_hooks_for_detached_completion;
use crate::services::windowed_session::{
    SessionAction, SessionDecision, SlotEvent, SlotOutcome, WindowedSession, run_windowed_session,
};

const INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES: usize = 1_048_576;
const EVALUATE_RESULT_WAIT_TICK: Duration = Duration::from_millis(50);
/// How many finished cases a staged evaluator interprets in one call.
const INTERPRET_COALESCE: usize = 4;
/// How many times a timed-out detached evaluate op is re-dispatched (on a fresh
/// op batch, which any worker may pick up) before the timeout is surfaced to the
/// plugin as a terminal event. The queued-case extend in [`EvaluateCases`]
/// already prevents queue-wait timeouts, so a timeout reaching the driver means a
/// genuine execution/result failure (dead worker, lost result, transient stall)
/// that is worth retrying on another worker rather than letting the plugin
/// blanket-fail the whole submission with SystemError.
const MAX_DETACHED_OP_RETRIES: u32 = 2;

mod cases;
mod detached;
mod dispatch;
mod result_wait;
#[cfg(test)]
mod tests;

pub use cases::EvaluateCases;
pub use detached::*;
pub use dispatch::*;
pub use result_wait::*;

fn record_evaluator_semaphore_wait(
    metrics: Option<&common::metrics::Metrics>,
    evaluator_plugin_id: &str,
    evaluator_function: &str,
    problem_type: &str,
    outcome: &'static str,
    elapsed: Duration,
) {
    let Some(metrics) = metrics else {
        return;
    };

    metrics.plugin_evaluator_semaphore_wait_duration.record(
        elapsed.as_secs_f64(),
        &[
            KeyValue::new("batch.kind", "evaluate"),
            KeyValue::new("evaluator.plugin.id", evaluator_plugin_id.to_string()),
            KeyValue::new("evaluator.function", evaluator_function.to_string()),
            KeyValue::new("problem.type", problem_type.to_string()),
            KeyValue::new("outcome", outcome),
        ],
    );
}

/// Whether a test-case body is cheap enough to keep a clone of for the whole
/// submission (so a timed-out op can be re-dispatched). Blob/Missing carry only
/// a hash; small inline bodies are fine. A large inline body (the
/// database-backend path) is NOT retained - holding ~tens of MB per test case
/// for the submission's lifetime would re-introduce the result-set memory
/// blow-up the windowed driver otherwise avoids by freeing each case after
/// dispatch. Such cases simply fall through to the existing timeout behavior.
fn body_cheap_to_retain(body: &TestCaseBodyRef) -> bool {
    match body {
        TestCaseBodyRef::Blob { .. } | TestCaseBodyRef::Missing => true,
        TestCaseBodyRef::Inline { text } => text.len() <= INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES,
    }
}

fn test_case_cheap_to_retain(test_case: &StartEvaluateCaseInput) -> bool {
    body_cheap_to_retain(&test_case.input) && body_cheap_to_retain(&test_case.expected_output)
}

fn detached_evaluate_completion(
    completion: Option<&broccoli_server_sdk::types::DetachedSubmissionCompletion>,
    action: DetachedEvaluateCallbackAction,
) -> Option<broccoli_server_sdk::types::DetachedSubmissionCompletion> {
    match action {
        DetachedEvaluateCallbackAction::Finish | DetachedEvaluateCallbackAction::Cancel => {
            completion
                .filter(|completion| completion.fire_after_judging)
                .cloned()
        }
        DetachedEvaluateCallbackAction::Continue => None,
    }
}

fn push_ready_case_for_callback<T>(pending: &mut Vec<T>, item: T) -> Option<Vec<T>> {
    pending.push(item);
    if pending.len() >= INTERPRET_COALESCE {
        Some(std::mem::take(pending))
    } else {
        None
    }
}

fn system_error_verdict(test_case_id: i32, message: impl Into<String>) -> TestCaseVerdict {
    TestCaseVerdict {
        test_case_id,
        verdict: SdkVerdict::SystemError,
        score: 0.0,
        time_used_ms: None,
        memory_used_kb: None,
        message: Some(message.into()),
        stdout: None,
        stderr: None,
    }
}

fn validate_batch_shape(
    test_cases: &[StartEvaluateCaseInput],
) -> anyhow::Result<Option<(i32, String)>> {
    let Some(first) = test_cases.first() else {
        return Ok(None);
    };

    let problem_id = first.problem_id;
    if test_cases.iter().any(|tc| tc.problem_id != problem_id) {
        return Err(anyhow!(
            "All test cases in a batch must belong to the same problem"
        ));
    }

    let solution_language = first.solution_language.clone();
    if test_cases
        .iter()
        .any(|tc| tc.solution_language != solution_language)
    {
        return Err(anyhow!(
            "All test cases in a batch must use the same solution_language"
        ));
    }

    Ok(Some((problem_id, solution_language)))
}

struct ResolvedEvaluateBody {
    file: JudgeFile,
    present: bool,
}

async fn resolve_evaluate_body(
    body: TestCaseBodyRef,
    db_inline: Option<String>,
    db_blob_hash: Option<String>,
    filename: &str,
    log_label: &str,
    blob_store: Arc<dyn BlobStore>,
) -> anyhow::Result<ResolvedEvaluateBody> {
    let content = match body {
        TestCaseBodyRef::Blob { hash } => {
            return Ok(ResolvedEvaluateBody {
                file: JudgeFile::blob(file_ref(filename, hash)),
                present: true,
            });
        }
        TestCaseBodyRef::Inline { text } => text,
        TestCaseBodyRef::Missing => {
            if let Some(hash) = db_blob_hash {
                return Ok(ResolvedEvaluateBody {
                    file: JudgeFile::blob(file_ref(filename, hash)),
                    present: true,
                });
            }
            let Some(content) = db_inline else {
                return Ok(ResolvedEvaluateBody {
                    file: JudgeFile::Missing,
                    present: false,
                });
            };
            content
        }
    };

    let (inline, reference) =
        maybe_externalize_text_file(content, filename, log_label, blob_store).await?;
    Ok(ResolvedEvaluateBody {
        file: reference
            .map(JudgeFile::blob)
            .unwrap_or_else(|| JudgeFile::inline(inline)),
        present: true,
    })
}

async fn maybe_externalize_text_file(
    content: String,
    filename: &str,
    log_label: &str,
    blob_store: Arc<dyn BlobStore>,
) -> anyhow::Result<(String, Option<FileRef>)> {
    if content.len() < INLINE_TEST_INPUT_BLOB_THRESHOLD_BYTES {
        return Ok((content, None));
    }

    let content_len = content.len();
    let hash = ContentHash::compute(content.as_bytes());
    let exists = blob_store
        .exists(&hash)
        .await
        .with_context(|| "Failed to check evaluate blob")?;
    if !exists {
        blob_store
            .put(content.as_bytes())
            .await
            .with_context(|| "Failed to store evaluate blob")?;
    }

    tracing::info!(
        content_bytes = content_len,
        blob_hash = %hash.to_hex(),
        label = log_label,
        "Externalized large evaluate file to blob storage"
    );

    Ok((String::new(), Some(file_ref(filename, hash.to_hex()))))
}

fn file_ref(filename: &str, blob_hash: String) -> FileRef {
    FileRef {
        filename: filename.to_string(),
        content_type: Some("text/plain".to_string()),
        blob_hash,
        read_token: None,
    }
}

fn tag_operation_tasks_for_evaluate_batch(
    operations: &mut [OperationTask],
    evaluate_batch_id: &str,
    test_case_id: i32,
) {
    for operation in operations {
        operation.evaluate_batch_id = Some(evaluate_batch_id.to_string());
        operation.test_case_id = Some(test_case_id);
    }
}

fn operation_results_from_task_results(
    task_results: Vec<common::worker::TaskResult>,
) -> anyhow::Result<Vec<OperationResult>> {
    task_results
        .into_iter()
        .map(|task_result| {
            serde_json::from_value::<OperationResult>(task_result.output.clone()).map_err(|_| {
                let error_msg = task_result
                    .error
                    .as_deref()
                    .or_else(|| task_result.output.get("error").and_then(|e| e.as_str()))
                    .unwrap_or("Unknown operation error");
                anyhow!(
                    "Operation task {} did not contain a valid operation output: {}",
                    task_result.task_id,
                    error_msg
                )
            })
        })
        .collect()
}

/// Defensive cleanup for the fan-out dispatcher task. The dispatcher loop has
/// no panic sites today, but if a future change introduces one, the guard's
/// Drop delivers a SystemError for every test case not yet handed off to a
/// worker. Without this floor, a dispatcher panic would leave the batch
/// pending forever and block every `next_evaluate_result` waiter.
struct DispatchGuard {
    pending: Vec<i32>,
    batch_tx: BatchSender<TestCaseVerdict>,
}

impl DispatchGuard {
    /// Mark a tc_id as successfully handed off to a worker (or cleaned up
    /// inline). The worker now owns the delivery obligation for that tc_id, so
    /// the guard must not deliver it again.
    fn claim(&mut self, tc_id: i32) {
        if let Some(pos) = self.pending.iter().position(|x| *x == tc_id) {
            self.pending.swap_remove(pos);
        }
    }
}

impl Drop for DispatchGuard {
    fn drop(&mut self) {
        for tc_id in self.pending.drain(..) {
            self.batch_tx.deliver(system_error_verdict(
                tc_id,
                "Evaluator dispatcher terminated unexpectedly",
            ));
        }
    }
}
