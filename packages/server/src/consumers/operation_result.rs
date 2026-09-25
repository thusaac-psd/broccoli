use crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry;
use crate::registry::OperationWaiters;
use common::metrics::Metrics;
use common::worker::TaskReply;
use mq::MqQueue;
use opentelemetry::KeyValue;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

pub async fn consume_operation_results(
    mq: Arc<MqQueue>,
    waiters: OperationWaiters,
    evaluate_ops_registry: EvaluateBatchOpsRegistry,
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
                let evaluate_ops_registry = evaluate_ops_registry.clone();
                let metrics = metrics.clone();
                async move {
                    crate::consumers::guard_handler("operation_result", async move {
                        let started = std::time::Instant::now();
                    let (reply_type, outcome, task_success) = match message.payload {
                        TaskReply::Started { notification } => {
                            let task_id = notification.task_id;
                            let outcome = if evaluate_ops_registry
                                .mark_operation_started(&task_id, notification.started_at_unix_ms)
                            {
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
                                let tx = waiter
                                    .result_tx
                                    .lock()
                                    .ok()
                                    .and_then(|mut guard| guard.take());
                                if let Some(tx) = tx {
                                    if tx.send(result).is_err() {
                                        error!(%task_id, "Failed to send operation result to waiter (receiver dropped)");
                                        "receiver_dropped"
                                    } else {
                                        debug!(%task_id, "Operation result delivered to plugin");
                                        "delivered"
                                    }
                                } else {
                                    error!(%task_id, "Operation result waiter sender missing");
                                    "receiver_dropped"
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
