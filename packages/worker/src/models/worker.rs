use common::hook::{Hook, HookRegistry};
use common::worker::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::config::WorkerAppConfig;
use crate::error::WorkerError;
use crate::models::operation::OperationTaskExecutor;

pub struct Worker {
    executors: Arc<Mutex<HashMap<String, Arc<dyn Executor>>>>,
    hook_registry: Arc<Mutex<HookRegistry>>,
}

impl Worker {
    pub fn with_no_executors() -> Self {
        Self {
            executors: Arc::new(Mutex::new(HashMap::new())),
            hook_registry: Arc::new(Mutex::new(HookRegistry::new(()))),
        }
    }

    pub async fn from_config(
        config: &WorkerAppConfig,
        metrics: common::metrics::Metrics,
    ) -> anyhow::Result<Self> {
        let worker = Self::with_no_executors();

        worker.register_executor(
            "operation",
            Arc::new(OperationTaskExecutor::from_app_config(config, metrics).await?),
        );
        Ok(worker)
    }

    pub fn register_executor(&self, name: &str, executor: Arc<dyn Executor>) {
        self.executors
            .lock()
            .unwrap()
            .insert(name.to_string(), executor);
    }

    #[allow(dead_code)]
    pub fn add_hook<H: Hook<TaskEvent, Context = ()> + 'static>(
        &self,
        hook: H,
    ) -> Result<(), WorkerError> {
        self.hook_registry
            .lock()
            .unwrap()
            .add_hook(hook)
            .map_err(|e| WorkerError::Internal(e.to_string()))
    }

    pub async fn execute_task(&self, task: Task) -> Result<TaskResult, WorkerError> {
        let hook_manager = { self.hook_registry.lock().unwrap().clone() };

        let _ = hook_manager
            .trigger(&TaskEvent::Started { task: task.clone() })
            .await;

        let executor = self
            .executors
            .lock()
            .unwrap()
            .get(&task.executor_name)
            .cloned();

        let result = if let Some(executor) = executor {
            if !executor.if_accept(&task.task_type) {
                return Err(WorkerError::External(format!(
                    "Executor '{}' does not accept task type '{}'",
                    task.executor_name, task.task_type
                )));
            }
            match executor.execute(task.clone()).await {
                Ok(result) => {
                    let _ = hook_manager
                        .trigger(&TaskEvent::Completed {
                            result: result.clone(),
                        })
                        .await;
                    result
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    let _ = hook_manager
                        .trigger(&TaskEvent::Failed {
                            task: task.clone(),
                            error: error_msg.clone(),
                        })
                        .await;
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
