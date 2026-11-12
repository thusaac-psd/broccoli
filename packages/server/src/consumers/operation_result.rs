use crate::registry::{EvaluateBatches, OperationWaiters};
use common::metrics::Metrics;
use common::worker::TaskReply;
use mq::MqQueue;
use opentelemetry::KeyValue;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub async fn consume_operation_results(
    mq: Arc<MqQueue>,
    waiters: OperationWaiters,
    evaluate_batches: EvaluateBatches,
    queue_name: String,
    metrics: Metrics,
    // How long each task naps after an empty poll (`mq.result_poll_interval_ms`).
    poll_interval: std::time::Duration,
) {
    info!(
        queue = %queue_name,
        "Starting operation result consumer"
    );

    // Concurrency=8: each handler invocation is short (DashMap lookup +
    // channel send), but under burst load (1000-submission window) the queue
    // can briefly buffer thousands of result messages. Single-threaded
    // consumption serializes the waker fan-out and adds head-of-line blocking
    // on whichever evaluator is currently slow. 8 concurrent handlers give
    // enough parallelism that the result queue never becomes the bottleneck,
    // without flooding the runtime - each handler holds no locks across .await.
    if let Err(e) = mq
        .process_messages(
            &queue_name,
            Some(8),
            Some(mq::ConsumeConfig {
                consume_wait: Some(poll_interval),
                ..Default::default()
            }),
            move |message: mq::BrokerMessage<TaskReply>| {
                let waiters = waiters.clone();
                let evaluate_batches = evaluate_batches.clone();
                let metrics = metrics.clone();
                async move {
                    crate::consumers::guard_handler("operation_result", async move {
                        let started = std::time::Instant::now();
                    let (reply_type, outcome, task_success) = match message.payload {
                        TaskReply::Started { notification } => {
                            let task_id = notification.task_id;
                            // The start is timed by this server's monotonic
                            // clock on receipt, never by the worker's clock.
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
                                if waiter.result_tx.send(result).is_err() {
                                    error!(%task_id, "Failed to send operation result to waiter (receiver dropped)");
                                    "receiver_dropped"
                                } else {
                                    debug!(%task_id, "Operation result delivered to plugin");
                                    "delivered"
                                }
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

                        Ok(())
                    })
                    .await
                }
            },
        )
        .await
    {
        error!(error = %e, "Operation result consumer exited with error");
    }
}
