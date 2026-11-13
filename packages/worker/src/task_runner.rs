use std::sync::Arc;
use std::time::Duration;

use common::metrics::Metrics;
use common::retry::calculate_backoff;
use common::worker::{Task, TaskReply, TaskResult, TaskStartedNotification};
use mq::{Mq, ReplyOutcome};
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
    mq: &Mq,
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
            &task.reply_to,
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

/// Publish an already-computed `result` to the server waiting for it,
/// retrying only the publish (never the execution) with backoff per `retry`.
/// A server that is gone is not waited for: its reply is dropped.
pub async fn publish_result(
    task: &Task,
    result: &TaskResult,
    mq: &Mq,
    metrics: &Metrics,
    retry: PublishRetry,
) -> Result<(), WorkerError> {
    let reply = TaskReply::Completed {
        result: result.clone(),
    };
    let started = std::time::Instant::now();
    let mut attempt: u8 = 0;
    loop {
        let publish_start = std::time::Instant::now();
        let error = match mq.publish_reply(&task.reply_to, &reply).await {
            Ok(ReplyOutcome::Delivered) => {
                record_mq_publish(metrics, "task_result", "success", publish_start);
                info!(
                    job_id = %task.id,
                    success = result.success,
                    reply_to = %task.reply_to,
                    publish_attempts = attempt + 1,
                    "Task finished"
                );
                return Ok(());
            }
            Ok(ReplyOutcome::ServerGone) => {
                record_mq_publish(metrics, "task_result", "server_gone", publish_start);
                warn!(
                    job_id = %task.id,
                    reply_to = %task.reply_to,
                    "The server waiting for this result is gone; dropping it"
                );
                return Ok(());
            }
            Err(e) => e.to_string(),
        };
        record_mq_publish(metrics, "task_result", "error", publish_start);
        attempt = attempt.saturating_add(1);
        if started.elapsed() >= retry.deadline {
            error!(
                job_id = %task.id,
                reply_to = %task.reply_to,
                attempts = attempt,
                error = %error,
                "Giving up publishing the computed task result"
            );
            return Err(WorkerError::Mq(error));
        }
        let delay = calculate_backoff(attempt, retry.base_delay_ms, retry.max_delay_ms);
        warn!(
            job_id = %task.id,
            reply_to = %task.reply_to,
            attempt,
            delay_ms = delay.as_millis() as u64,
            error = %error,
            "Failed to publish task result; retrying the publish only"
        );
        tokio::time::sleep(delay).await;
    }
}

/// Tell the server the task started. Best effort: a lost notification only
/// delays the server's start bookkeeping until the result arrives.
fn publish_started(
    notification: TaskStartedNotification,
    reply_to: &str,
    mq: &Mq,
    metrics: &Metrics,
) {
    let task_id = notification.task_id.clone();
    let reply_to = reply_to.to_string();
    let mq = mq.clone();
    let metrics = metrics.clone();

    tokio::spawn(async move {
        let publish_start = std::time::Instant::now();
        let reply = TaskReply::Started { notification };
        match mq.publish_reply(&reply_to, &reply).await {
            Ok(ReplyOutcome::Delivered) => {
                record_mq_publish(&metrics, "task_started", "success", publish_start)
            }
            Ok(_) => record_mq_publish(&metrics, "task_started", "dropped", publish_start),
            Err(e) => {
                record_mq_publish(&metrics, "task_started", "error", publish_start);
                warn!(
                    job_id = %task_id,
                    reply_to = %reply_to,
                    error = %e,
                    "Failed to publish task started notification"
                );
            }
        }
    });
}
