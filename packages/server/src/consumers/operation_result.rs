use crate::registry::{EvaluateBatches, OperationWaiters};
use common::metrics::Metrics;
use common::worker::{TaskReply, TaskResult};
use mq::ReplyInbox;
use opentelemetry::KeyValue;
use tracing::{debug, error, info, warn};

/// Deliver this server's replies to their waiters, for the life of the
/// process. One reader is enough: handling a reply is a map lookup and a
/// channel send, and each read returns up to [`mq::REPLY_BATCH`] replies.
pub async fn consume_operation_results(
    mut inbox: ReplyInbox,
    waiters: OperationWaiters,
    evaluate_batches: EvaluateBatches,
    metrics: Metrics,
) {
    info!(stream = %inbox.key(), "Starting operation result consumer");
    loop {
        let batch = match inbox.next_batch().await {
            Ok(batch) => batch,
            Err(e) => {
                warn!(stream = %inbox.key(), error = %e, "Reading replies failed; retrying");
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                continue;
            }
        };
        for reply in batch {
            match reply {
                Ok(reply) => {
                    crate::consumers::guard_handler("operation_result", async {
                        handle_reply(reply, &waiters, &evaluate_batches, &metrics)
                    })
                    .await;
                }
                Err(e) => error!(error = %e, "Dropping an undecodable reply"),
            }
        }
    }
}

/// Fail every outstanding operation now: their replies cannot arrive any
/// more (this server's reply stream was lost with whatever it held). The
/// plugins see failed operations at once instead of waiting out their
/// timeouts. Returns how many were failed.
pub fn fail_all_waiters(waiters: &OperationWaiters, reason: &str) -> usize {
    let ids: Vec<String> = waiters.iter().map(|w| w.key().clone()).collect();
    let mut failed = 0;
    for task_id in ids {
        let Some((_, waiter)) = waiters.remove(&task_id) else {
            continue;
        };
        let _ = waiter.result_tx.send(TaskResult {
            task_id,
            success: false,
            output: serde_json::json!({}),
            error: Some(reason.to_string()),
            task_type: None,
            operation: None,
            worker_id: None,
        });
        failed += 1;
    }
    if failed > 0 {
        error!(failed, reason, "Failed every outstanding operation");
    }
    failed
}

fn handle_reply(
    reply: TaskReply,
    waiters: &OperationWaiters,
    evaluate_batches: &EvaluateBatches,
    metrics: &Metrics,
) {
    let started = std::time::Instant::now();
    let (reply_type, outcome, task_success) = match reply {
        TaskReply::Started { notification } => {
            let task_id = notification.task_id;
            // The start is timed by this server's monotonic clock on
            // receipt, never by the worker's clock.
            let case = waiters
                .get(&task_id)
                .and_then(|waiter| waiter.evaluate_case.clone());
            let outcome = if case.is_some_and(|case| {
                evaluate_batches.mark_case_started(
                    &case.batch_id,
                    case.test_case_id,
                    std::time::Instant::now(),
                )
            }) {
                debug!(%task_id, "Operation start delivered to evaluate batch registry");
                "started"
            } else {
                debug!(%task_id, "Operation start had no evaluate batch registry mapping");
                "started_untracked"
            };
            ("started", outcome, "unknown".to_string())
        }
        TaskReply::Completed { result } => {
            let task_id = result.task_id.clone();
            let task_success = result.success;
            let outcome = if let Some((_, waiter)) = waiters.remove(&task_id) {
                let elapsed = waiter.published_at.elapsed();
                let outcome = if waiter.result_tx.send(result.clone()).is_err() {
                    error!(%task_id, "Failed to send operation result to waiter (receiver dropped)");
                    "receiver_dropped"
                } else {
                    debug!(%task_id, "Operation result delivered to plugin");
                    "delivered"
                };
                crate::services::operation_batch::record_operation_result_e2e(
                    metrics, &result, outcome, elapsed,
                );
                outcome
            } else {
                warn!(%task_id, "Operation result received but no waiter found (batch may have been cancelled)");
                "no_waiter"
            };
            ("completed", outcome, task_success.to_string())
        }
    };

    let attrs = [
        KeyValue::new("reply_type", reply_type),
        KeyValue::new("outcome", outcome),
        KeyValue::new("task_success", task_success),
    ];
    metrics.operation_result_messages_total.add(1, &attrs);
    metrics
        .operation_result_consume_duration
        .record(started.elapsed().as_secs_f64(), &attrs);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dashmap::DashMap;

    use super::fail_all_waiters;
    use crate::registry::{OperationWaiter, OperationWaiters};

    #[tokio::test]
    async fn every_outstanding_waiter_fails_at_once() {
        let waiters: OperationWaiters = Arc::new(DashMap::new());
        let (tx_a, rx_a) = tokio::sync::oneshot::channel();
        let (tx_b, rx_b) = tokio::sync::oneshot::channel();
        waiters.insert("a".into(), OperationWaiter::new(tx_a, None));
        waiters.insert("b".into(), OperationWaiter::new(tx_b, None));

        assert_eq!(fail_all_waiters(&waiters, "stream lost"), 2);
        assert!(waiters.is_empty());
        for rx in [rx_a, rx_b] {
            let result = rx.await.expect("the waiter is answered");
            assert!(!result.success);
            assert_eq!(result.error.as_deref(), Some("stream lost"));
        }
    }
}
