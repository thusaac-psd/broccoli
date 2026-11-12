use std::sync::Arc;
use std::time::Duration;

use common::metrics::Metrics;
use common::retry::calculate_backoff;
use common::worker::{Task, TaskReply, TaskResult, TaskStartedNotification};
use tracing::{error, info, warn};

use crate::error::WorkerError;
use crate::metrics::record_mq_publish;
use crate::models::worker::Worker;

/// Execute `task` and return its result, WITHOUT publishing it.
///
/// Execution and publishing are separate steps so the caller can retry them
/// separately: a failed execution may be retried, but a result that was
/// computed must never be recomputed just because its publish failed (that
/// would re-run the whole sandbox job). See [`publish_result`].
pub async fn execute_task(
    task: &Task,
    worker: &Arc<Worker>,
    mq: &Arc<mq::Mq>,
    metrics: &Metrics,
    worker_id: &str,
) -> Result<TaskResult, WorkerError> {
    info!(
        job_id = %task.id,
        task_type = %task.task_type,
        "Processing task"
    );

    if task.task_type == "operation" {
        publish_started(
            TaskStartedNotification {
                task_id: task.id.clone(),
            },
            task.reply_queue_name(),
            mq,
            metrics,
        );
    }

    let worker = Arc::clone(worker);
    let task_clone = task.clone();
    let mut result = tokio::spawn(async move { worker.execute_task(task_clone).await })
        .await
        .map_err(|e| {
            if e.is_panic() {
                WorkerError::TaskPanic(format!("{e}"))
            } else {
                WorkerError::Internal(format!("Task join error: {e}"))
            }
        })??;
    result.attach_worker_metadata(task, worker_id);
    Ok(result)
}

/// How a computed result's publish is retried: exponential backoff from
/// `base_delay_ms` up to `max_delay_ms`, giving up once `deadline` has passed.
#[derive(Debug, Clone, Copy)]
pub struct PublishRetry {
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub deadline: Duration,
}

impl Default for PublishRetry {
    /// Long enough to ride out a Redis restart with AOF reload (minutes),
    /// short enough that a permanently broken MQ eventually frees the slot.
    fn default() -> Self {
        Self {
            base_delay_ms: 250,
            max_delay_ms: 5_000,
            deadline: Duration::from_secs(600),
        }
    }
}

/// Publish an already-computed `result` to the task's reply queue, retrying
/// only the publish (never the execution) with backoff per `retry`.
pub async fn publish_result(
    task: &Task,
    result: &TaskResult,
    mq: &Arc<mq::Mq>,
    metrics: &Metrics,
    retry: PublishRetry,
) -> Result<(), WorkerError> {
    let reply = TaskReply::Completed {
        result: result.clone(),
    };
    let reply_queue = task.reply_queue_name();
    let started = std::time::Instant::now();
    let mut attempt: u8 = 0;
    loop {
        let publish_start = std::time::Instant::now();
        match mq.publish(reply_queue, None, &reply, None).await {
            Ok(_) => {
                record_mq_publish(
                    metrics,
                    reply_queue,
                    "task_result",
                    "success",
                    publish_start,
                );
                info!(
                    job_id = %task.id,
                    task_result_id = %result.task_id,
                    success = result.success,
                    result_queue = %reply_queue,
                    publish_attempts = attempt + 1,
                    "Task finished"
                );
                return Ok(());
            }
            Err(e) => {
                record_mq_publish(metrics, reply_queue, "task_result", "error", publish_start);
                attempt = attempt.saturating_add(1);
                if started.elapsed() >= retry.deadline {
                    error!(
                        job_id = %task.id,
                        result_queue = %reply_queue,
                        attempts = attempt,
                        error = %e,
                        "Giving up publishing the computed task result"
                    );
                    return Err(WorkerError::Mq(e.to_string()));
                }
                let delay = calculate_backoff(attempt, retry.base_delay_ms, retry.max_delay_ms);
                warn!(
                    job_id = %task.id,
                    result_queue = %reply_queue,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    error = %e,
                    "Failed to publish task result; retrying the publish only"
                );
                tokio::time::sleep(delay).await;
            }
        }
    }
}

fn publish_started(
    notification: TaskStartedNotification,
    reply_queue: &str,
    mq: &Arc<mq::Mq>,
    metrics: &Metrics,
) {
    let task_id = notification.task_id.clone();
    let reply_queue = reply_queue.to_string();
    let mq = Arc::clone(mq);
    let metrics = metrics.clone();

    tokio::spawn(async move {
        let publish_start = std::time::Instant::now();
        let reply = TaskReply::Started { notification };
        match mq.publish(&reply_queue, None, &reply, None).await {
            Ok(_) => record_mq_publish(
                &metrics,
                &reply_queue,
                "task_started",
                "success",
                publish_start,
            ),
            Err(e) => {
                record_mq_publish(
                    &metrics,
                    &reply_queue,
                    "task_started",
                    "error",
                    publish_start,
                );
                warn!(
                    job_id = %task_id,
                    result_queue = %reply_queue,
                    error = %e,
                    "Failed to publish task started notification"
                );
            }
        }
    });
}
