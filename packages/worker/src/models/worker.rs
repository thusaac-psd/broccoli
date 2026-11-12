use common::worker::*;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::WorkerError;

/// Runs tasks on the executor registered under each task's `executor_name`.
/// The executors are fixed at construction.
pub struct Worker {
    executors: HashMap<String, Arc<dyn Executor>>,
}

impl Worker {
    pub fn new(executors: impl IntoIterator<Item = (String, Arc<dyn Executor>)>) -> Self {
        Self {
            executors: executors.into_iter().collect(),
        }
    }

    pub async fn execute_task(&self, task: Task) -> Result<TaskResult, WorkerError> {
        let executor = self.executors.get(&task.executor_name).cloned();

        let result = if let Some(executor) = executor {
            if !executor.if_accept(&task.task_type) {
                return Err(WorkerError::External(format!(
                    "Executor '{}' does not accept task type '{}'",
                    task.executor_name, task.task_type
                )));
            }
            match executor.execute(task.clone()).await {
                Ok(result) => result,
                Err(e) => {
                    let error_msg = e.to_string();
                    if !is_permanent_execution_failure(&error_msg) {
                        // Transient infra/IO failure (cold blob fetch under load,
                        // interrupted stream, sandbox hiccup). Return an error so the
                        // consumer's RetryTracker retries with backoff; only after
                        // retries are exhausted does it become a terminal failure.
                        // Genuine, deterministic failures fall through to success=false.
                        return Err(WorkerError::External(error_msg));
                    }
                    TaskResult {
                        task_id: task.id,
                        success: false,
                        output: serde_json::json!({ "error": error_msg }),
                        error: Some(error_msg),
                        task_type: None,
                        operation: None,
                        worker_id: None,
                        enqueued_at_unix_ms: None,
                    }
                }
            }
        } else {
            let error_msg = format!("No executor found for '{}'", task.executor_name);
            return Ok(TaskResult {
                task_id: task.id,
                success: false,
                output: serde_json::json!({ "error": &error_msg }),
                error: Some(error_msg),
                task_type: None,
                operation: None,
                worker_id: None,
                enqueued_at_unix_ms: None,
            });
        };

        Ok(result)
    }
}

/// Classify an operation execution error as permanent (deterministic - retrying
/// cannot help) vs transient (an infra/IO hiccup worth retrying).
///
/// Conservative by design: only clearly-genuine failures are permanent;
/// everything else is treated as transient so that a cold-blob-fetch thrash under
/// load, an interrupted stream, or a sandbox hiccup retries (bounded by the
/// RetryTracker) instead of surfacing as a spurious SystemError. A
/// genuinely-permanent error that isn't matched here still terminates - it just
/// burns the (bounded) retry budget first.
fn is_permanent_execution_failure(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    // StorageError::NotFound / InvalidHash / SizeLimitExceeded, and the
    // unavailable-blob-store config error - none recover on retry.
    m.contains("blob not found")
        || m.contains("invalid content hash")
        || m.contains("exceeds size limit")
        || m.contains("blob storage is unavailable")
}

#[cfg(test)]
mod execution_failure_classification_tests {
    use super::is_permanent_execution_failure;

    #[test]
    fn transient_blob_io_errors_are_retryable() {
        // These are wrapped as: "Failed to fetch blob <hash>: <inner>".
        assert!(!is_permanent_execution_failure(
            "Failed to fetch blob abc: Failed to stream blob to cache: connection reset"
        ));
        assert!(!is_permanent_execution_failure(
            "Failed to fetch blob abc: Failed to finalize cache file: No space left"
        ));
        assert!(!is_permanent_execution_failure(
            "Failed to fetch blob abc: storage IO error: broken pipe"
        ));
        assert!(!is_permanent_execution_failure(
            "storage backend error: temporary failure"
        ));
    }

    #[test]
    fn genuinely_permanent_errors_are_terminal() {
        assert!(is_permanent_execution_failure(
            "Failed to fetch blob abc: blob not found: abc"
        ));
        assert!(is_permanent_execution_failure(
            "Failed to fetch blob abc: invalid content hash: bad length"
        ));
        assert!(is_permanent_execution_failure(
            "Failed to fetch blob abc: blob exceeds size limit (5 > 4 bytes)"
        ));
        assert!(is_permanent_execution_failure(
            "blob storage is unavailable; cannot fetch blob abc: init failed"
        ));
    }
}
