use std::time::{Duration, Instant};

use crate::registry::{EvaluateBatches, OperationWaiters};
use common::metrics::Metrics;
use common::worker::{TaskReply, TaskResult};
use mq::{Mq, ReplyInbox};
use opentelemetry::KeyValue;
use tracing::{debug, error, info, warn};

/// How often the result consumer checks this server's outstanding operations
/// against its ledger in the queue (see [`OutstandingCheck`]).
pub const OUTSTANDING_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// The error a lost operation fails with.
pub const OPERATION_LOST: &str =
    "the operation was lost from the queue (Redis restarted without it); the result cannot arrive";

/// Proof that every operation this server waits for is still alive.
///
/// The queue keeps a ledger per server of the tasks it published that have no
/// reply yet: a task enters it in the script that publishes it and leaves it
/// in the script that publishes its completed reply (see
/// `mq::Mq::check_outstanding`). A task the queue holds anywhere (unread, in a
/// worker's pending list, requeued or moved off a dead worker) is in the
/// ledger, so a published operation missing from it either has its reply in
/// the reply stream or was lost with a Redis that came back from an older
/// snapshot. Every `interval` the consumer checks all published waiters in one
/// round trip per [`mq::CHECK_CHUNK`] ids and notes the reply stream's last id;
/// once its reads have passed that id, any of them still waiting has no reply
/// coming and fails at once, into the plugin's normal failure handling.
pub struct OutstandingCheck {
    pub mq: Mq,
    pub server_id: String,
    pub interval: Duration,
}

/// Operations found missing from the ledger, waiting for the inbox to read
/// past every reply that could still answer them.
struct Suspects {
    ids: Vec<String>,
    replies_through: Option<String>,
}

/// Deliver this server's replies to their waiters, for the life of the
/// process, and fail the waiters whose operation the queue lost. One reader is
/// enough: handling a reply is a map lookup and a channel send, and each read
/// returns up to [`mq::REPLY_BATCH`] replies.
pub async fn consume_operation_results(
    mut inbox: ReplyInbox,
    check: OutstandingCheck,
    waiters: OperationWaiters,
    evaluate_batches: EvaluateBatches,
    metrics: Metrics,
) {
    info!(stream = %inbox.key(), "Starting operation result consumer");
    let mut suspects: Option<Suspects> = None;
    let mut next_check = Instant::now() + check.interval;
    loop {
        let batch = match inbox.next_batch().await {
            Ok(batch) => batch,
            Err(e) => {
                warn!(stream = %inbox.key(), error = %e, "Reading replies failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
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
        if let Some(found) =
            suspects.take_if(|s| inbox.has_read_through(s.replies_through.as_deref()))
        {
            let failed = found
                .ids
                .iter()
                .filter(|id| fail_waiter(&waiters, id, OPERATION_LOST))
                .count();
            if failed > 0 {
                error!(failed, "Failed operations the queue lost");
            }
        }
        if suspects.is_none() && Instant::now() >= next_check {
            next_check = Instant::now() + check.interval;
            suspects = find_missing(&check, &waiters).await;
        }
    }
}

/// The published operations the ledger no longer holds, if any.
async fn find_missing(check: &OutstandingCheck, waiters: &OperationWaiters) -> Option<Suspects> {
    let published: Vec<String> = waiters
        .iter()
        .filter(|w| w.published)
        .map(|w| w.key().clone())
        .collect();
    match check
        .mq
        .check_outstanding(&check.server_id, &published)
        .await
    {
        Ok(found) if found.missing.is_empty() => None,
        Ok(found) => Some(Suspects {
            ids: found.missing,
            replies_through: found.replies_through,
        }),
        Err(e) => {
            warn!(error = %e, "Checking outstanding operations failed; retrying next interval");
            None
        }
    }
}

/// Fail one waiter with `reason`; whether it was still waiting.
fn fail_waiter(waiters: &OperationWaiters, task_id: &str, reason: &str) -> bool {
    let Some((task_id, waiter)) = waiters.remove(task_id) else {
        return false;
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
    true
}

/// Fail every outstanding operation now: their replies cannot arrive any
/// more (this server's reply stream was lost with whatever it held). The
/// plugins see failed operations at once instead of waiting out their
/// timeouts. Returns how many were failed.
pub fn fail_all_waiters(waiters: &OperationWaiters, reason: &str) -> usize {
    let ids: Vec<String> = waiters.iter().map(|w| w.key().clone()).collect();
    let failed = ids
        .iter()
        .filter(|id| fail_waiter(waiters, id, reason))
        .count();
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
