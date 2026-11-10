use std::sync::Arc;

use common::metrics::Metrics;
use common::retry::{RetryCleanupGuard, RetryDecision, RetryTracker, calculate_backoff};
use common::worker::Task;
use common::{DlqConfig, DlqEnvelope, DlqErrorCode, DlqMessageType};
use mq::{BroccoliError, BrokerMessage};
use opentelemetry::KeyValue;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use common::cancel::RedisCancelChecker;

use crate::dedup::{ClaimOutcome, RedisTaskDedup, task_dedup_ttl_secs};
use crate::metrics::{
    TaskMetricGuard, WorkerPermitMetricGuard, record_mq_consume, record_mq_publish,
};
use crate::models::worker::Worker;
use crate::task_runner::{PublishRetry, execute_task, publish_result};

#[derive(Clone)]
pub struct WorkerConsumer {
    worker: Arc<Worker>,
    mq: Arc<mq::Mq>,
    dlq_queue: String,
    dlq_config: DlqConfig,
    retry_tracker: Arc<Mutex<RetryTracker>>,
    dedup: Option<Arc<RedisTaskDedup>>,
    cancel_checker: Option<Arc<RedisCancelChecker>>,
    metrics: Metrics,
    worker_id: String,
    publish_retry: PublishRetry,
}

pub struct WorkerConsumerDeps {
    pub worker: Arc<Worker>,
    pub mq: Arc<mq::Mq>,
    pub dlq_queue: String,
    pub dlq_config: DlqConfig,
    pub retry_tracker: Arc<Mutex<RetryTracker>>,
    pub dedup: Option<Arc<RedisTaskDedup>>,
    pub cancel_checker: Option<Arc<RedisCancelChecker>>,
    pub metrics: Metrics,
    pub worker_id: String,
}

impl WorkerConsumer {
    pub fn new(deps: WorkerConsumerDeps) -> Self {
        Self {
            worker: deps.worker,
            mq: deps.mq,
            dlq_queue: deps.dlq_queue,
            dlq_config: deps.dlq_config,
            retry_tracker: deps.retry_tracker,
            dedup: deps.dedup,
            cancel_checker: deps.cancel_checker,
            metrics: deps.metrics,
            worker_id: deps.worker_id,
            publish_retry: PublishRetry::default(),
        }
    }

    /// Process one already-consumed task under the per-task worker-permit metric
    /// guard, returning the ack/reject decision (`Ok` -> acknowledge, `Err` ->
    /// reject/retry).
    ///
    /// Unlike the former `handler`, this does NOT short-circuit on shutdown. The
    /// manual consume loop in `crate::runtime` stops CONSUMING the instant
    /// shutdown is signaled, so a task that has already been taken off the queue
    /// is always run to completion during the drain window instead of being
    /// rejected - the old shutdown-reject path burned the retry budget and pushed
    /// still-valid tasks into the failed queue on every graceful shutdown.
    pub async fn run_task(&self, message: BrokerMessage<Task>) -> Result<(), BroccoliError> {
        let _worker_permit_guard = WorkerPermitMetricGuard::new(&self.metrics, &self.worker_id);
        self.process_message(message).await
    }

    #[tracing::instrument(skip(self, message), fields(job_id, task_type, executor))]
    pub async fn process_message(&self, message: BrokerMessage<Task>) -> Result<(), BroccoliError> {
        let consume_start = std::time::Instant::now();
        let attempts = message.attempts;
        let task = message.payload;
        let task_id = task.id.clone();
        tracing::Span::current().record("job_id", tracing::field::display(&task_id));
        tracing::Span::current().record("task_type", tracing::field::display(&task.task_type));
        tracing::Span::current().record("executor", tracing::field::display(&task.executor_name));

        let task_attrs = vec![
            KeyValue::new("task_type", task.task_type.clone()),
            KeyValue::new("executor", task.executor_name.clone()),
        ];
        self.metrics.tasks_received_total.add(1, &task_attrs);
        let worker_attrs = vec![KeyValue::new("worker_id", self.worker_id.clone())];
        let _task_metric_guard =
            TaskMetricGuard::new(&self.metrics, task_attrs.clone(), worker_attrs);

        if let Some(enqueued_at_unix_ms) = task.enqueued_at_unix_ms {
            let now = chrono::Utc::now().timestamp_millis();
            let age_secs = now.saturating_sub(enqueued_at_unix_ms) as f64 / 1000.0;
            self.metrics.mq_message_age.record(
                age_secs,
                &[
                    KeyValue::new("queue", "operation"),
                    KeyValue::new("message.type", "operation_task"),
                ],
            );
            self.metrics
                .task_queue_wait_duration
                .record(age_secs, &task_attrs);
        }

        if let Some(dedup) = self.dedup.as_ref() {
            // Per-operation TTL (step limits + slack, floored at the fallback) so
            // the claim outlives the run without lingering long after it finishes.
            let ttl_secs = task_dedup_ttl_secs(&task, dedup.fallback_ttl_secs());
            match dedup.try_claim_with_ttl(&task_id, ttl_secs).await {
                ClaimOutcome::Claimed => {
                    let mut attrs = task_attrs.clone();
                    attrs.push(KeyValue::new("outcome", "claimed"));
                    self.metrics.task_dedup_claims_total.add(1, &attrs);
                }
                ClaimOutcome::Stolen => {
                    let mut attrs = task_attrs.clone();
                    attrs.push(KeyValue::new("outcome", "stolen"));
                    self.metrics.task_dedup_claims_total.add(1, &attrs);
                    warn!(
                        job_id = %task_id,
                        "Stole claim from a worker with no live heartbeat — re-judging"
                    );
                }
                ClaimOutcome::HeldByOther => {
                    let mut attrs = task_attrs.clone();
                    attrs.push(KeyValue::new("outcome", "held_by_other"));
                    self.metrics.task_dedup_claims_total.add(1, &attrs);
                    info!(job_id = %task_id, "Task already claimed by a live worker, skipping");
                    record_mq_consume(
                        &self.metrics,
                        "operation",
                        "operation_task",
                        "dedup_skip",
                        attempts,
                        consume_start,
                    );
                    return Ok(());
                }
            }
        }

        if let Some(ref tc) = task.trace_context
            && let Some(remote_cx) = common::observability::extract_trace_context(tc)
        {
            use tracing_opentelemetry::OpenTelemetrySpanExt;
            let _ = tracing::Span::current().set_parent(remote_cx);
        }

        if let Some(checker) = self.cancel_checker.as_ref() {
            match checker
                .is_cancelled(task.operation_batch_id.as_deref(), &task_id)
                .await
            {
                Ok(true) => {
                    info!(
                        job_id = %task_id,
                        operation_batch_id = ?task.operation_batch_id,
                        "Task pre-empted by Redis cancel key; skipping execution"
                    );
                    let mut attrs = task_attrs.clone();
                    attrs.push(KeyValue::new("outcome", "cancelled_by_host"));
                    self.metrics.tasks_completed_total.add(1, &attrs);
                    record_mq_consume(
                        &self.metrics,
                        "operation",
                        "operation_task",
                        "cancelled_by_host",
                        attempts,
                        consume_start,
                    );
                    return Ok(());
                }
                Ok(false) => {}
                Err(e) => {
                    warn!(error = %e, job_id = %task_id, "Cancel check failed; proceeding with task execution");
                }
            }
        }

        info!(
            job_id = %task_id,
            task_type = %task.task_type,
            executor_name = %task.executor_name,
            worker_id = %self.worker_id,
            trace_id = common::observability::current_trace_id().as_deref().unwrap_or(""),
            "Received task"
        );

        let task_start = std::time::Instant::now();
        let mut cleanup_guard = RetryCleanupGuard::new(&self.retry_tracker, &task_id);

        // Only EXECUTION is retried by this loop. Once a result exists it is
        // published with its own publish-only retry, so a transient MQ failure
        // never re-runs the sandbox job or discards the computed verdict.
        loop {
            match execute_task(
                &task,
                &self.worker,
                &self.mq,
                &self.metrics,
                &self.worker_id,
            )
            .await
            {
                Ok(result) => {
                    self.retry_tracker.lock().await.clear(&task_id);
                    cleanup_guard.defuse();
                    let published =
                        publish_result(&task, &result, &self.mq, &self.metrics, self.publish_retry)
                            .await;

                    let outcome = match (&published, result.success) {
                        (Err(_), _) => "publish_failed",
                        (Ok(()), true) => "success",
                        (Ok(()), false) => "failure",
                    };
                    let mut completion_attrs = task_attrs.clone();
                    completion_attrs.push(KeyValue::new("outcome", outcome));
                    self.metrics
                        .task_process_duration
                        .record(task_start.elapsed().as_secs_f64(), &completion_attrs);
                    self.metrics.tasks_completed_total.add(1, &completion_attrs);
                    record_mq_consume(
                        &self.metrics,
                        "operation",
                        "operation_task",
                        outcome,
                        attempts,
                        consume_start,
                    );
                    // A result that could not be delivered within the publish
                    // window is handed back to the broker (reject -> redelivery):
                    // nothing else can reach the waiting server.
                    return published.map_err(|e| BroccoliError::Publish(e.to_string()));
                }
                Err(e) => {
                    let error_str = e.to_string();
                    let decision = self
                        .retry_tracker
                        .lock()
                        .await
                        .record_failure(&task_id, &error_str);

                    match decision {
                        RetryDecision::Retry { attempt, .. } => {
                            let mut retry_attrs = task_attrs.clone();
                            retry_attrs.push(KeyValue::new("outcome", "retry"));
                            self.metrics.task_retries_total.add(1, &retry_attrs);

                            let delay = calculate_backoff(
                                attempt,
                                self.dlq_config.base_delay_ms,
                                self.dlq_config.max_delay_ms,
                            );
                            warn!(
                                job_id = %task_id,
                                attempt,
                                delay_ms = delay.as_millis() as u64,
                                error = %e,
                                "Task failed, retrying"
                            );
                            tokio::time::sleep(delay).await;
                        }
                        RetryDecision::Exhausted { history } => {
                            self.finish_exhausted(
                                &task,
                                &task_id,
                                &task_attrs,
                                attempts,
                                consume_start,
                                task_start,
                                error_str,
                                history,
                                &mut cleanup_guard,
                            )
                            .await;
                            return Ok(());
                        }
                    }
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn finish_exhausted(
        &self,
        task: &Task,
        task_id: &str,
        task_attrs: &[KeyValue],
        attempts: u8,
        consume_start: std::time::Instant,
        task_start: std::time::Instant,
        error_str: String,
        history: Vec<common::retry::RetryAttempt>,
        cleanup_guard: &mut RetryCleanupGuard<'_>,
    ) {
        let mut exhausted_attrs = task_attrs.to_vec();
        exhausted_attrs.push(KeyValue::new("outcome", "exhausted"));
        self.metrics.task_retries_total.add(1, &exhausted_attrs);
        self.metrics.dlq_messages_total.add(1, &exhausted_attrs);
        self.metrics
            .task_process_duration
            .record(task_start.elapsed().as_secs_f64(), &exhausted_attrs);
        self.metrics.tasks_completed_total.add(1, &exhausted_attrs);

        error!(
            job_id = %task_id,
            retry_count = history.len(),
            error = %error_str,
            "Max retries exhausted, sending to DLQ"
        );

        let error_result = common::worker::TaskResult {
            task_id: task_id.to_string(),
            success: false,
            output: serde_json::json!({}),
            error: Some(format!(
                "Operation failed after {} retries: {}",
                history.len(),
                error_str
            )),
            task_type: Some(task.task_type.clone()),
            operation: Some(task.executor_name.clone()),
            worker_id: Some(self.worker_id.clone()),
            enqueued_at_unix_ms: task.enqueued_at_unix_ms,
        };
        let publish_start = std::time::Instant::now();
        if let Err(e) = self
            .mq
            .publish(task.reply_queue_name(), None, &error_result, None)
            .await
        {
            record_mq_publish(
                &self.metrics,
                task.reply_queue_name(),
                "task_result",
                "error",
                publish_start,
            );
            error!(job_id = %task_id, error = %e, "Failed to publish error result for operation task");
        } else {
            record_mq_publish(
                &self.metrics,
                task.reply_queue_name(),
                "task_result",
                "success",
                publish_start,
            );
        }

        let payload = serde_json::to_value(task).unwrap_or_else(|ser_err| {
            error!(error = %ser_err, "Failed to serialize task for DLQ");
            serde_json::json!({ "task_id": task_id })
        });

        let envelope = DlqEnvelope {
            message_id: task_id.to_string(),
            message_type: DlqMessageType::OperationTask,
            submission_id: None,
            payload,
            error_code: DlqErrorCode::MaxRetriesExceeded,
            error_message: error_str,
            retry_history: history,
        };

        let dlq_publish_start = std::time::Instant::now();
        if let Err(dlq_err) = self
            .mq
            .publish(&self.dlq_queue, None, &envelope, None)
            .await
        {
            record_mq_publish(
                &self.metrics,
                &self.dlq_queue,
                "dlq_envelope",
                "error",
                dlq_publish_start,
            );
            error!(
                job_id = %task_id,
                error = %dlq_err,
                "CRITICAL: Failed to publish to DLQ, message may be lost"
            );
        } else {
            record_mq_publish(
                &self.metrics,
                &self.dlq_queue,
                "dlq_envelope",
                "success",
                dlq_publish_start,
            );
        }

        if let Some(dedup) = self.dedup.as_ref() {
            dedup.release(task_id).await;
        }

        cleanup_guard.defuse();
        record_mq_consume(
            &self.metrics,
            "operation",
            "operation_task",
            "exhausted",
            attempts,
            consume_start,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use common::cancel::{RedisCancelChecker, set_cancel_batch_key, set_cancel_op_keys};
    use common::worker::{Executor, TaskResult};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    struct CountingExecutor {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Executor for CountingExecutor {
        fn if_accept(&self, _task_type: &str) -> bool {
            true
        }

        async fn execute(&self, task: Task) -> anyhow::Result<TaskResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TaskResult {
                task_id: task.id,
                success: true,
                output: serde_json::json!({ "executed": true }),
                error: None,
                task_type: None,
                operation: None,
                worker_id: None,
                enqueued_at_unix_ms: None,
            })
        }
    }

    fn task(id: &str, batch_id: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            task_type: "operation".into(),
            executor_name: "operation".into(),
            payload: serde_json::json!({}),
            result_queue: "worker-cancel-test-results".into(),
            operation_batch_id: batch_id.map(str::to_string),
            reply_queue: None,
            priority: None,
            trace_context: None,
            enqueued_at_unix_ms: None,
        }
    }

    async fn consumer_with_cancel_checker(
        redis_url: &str,
        calls: Arc<AtomicUsize>,
    ) -> WorkerConsumer {
        let worker = Worker::with_no_executors();
        worker.register_executor("operation", Arc::new(CountingExecutor { calls }));
        let mq = mq::init_mq(mq::MqConfig {
            url: redis_url.to_string(),
            pool_size: 1,
        })
        .await
        .expect("test MQ should connect to Redis");
        let (metrics, _registry) =
            common::observability::init_metrics("worker-cancel-consumer-test");

        WorkerConsumer::new(WorkerConsumerDeps {
            worker: Arc::new(worker),
            mq: Arc::new(mq),
            dlq_queue: "worker-cancel-test-dlq".into(),
            dlq_config: DlqConfig::default(),
            retry_tracker: Arc::new(Mutex::new(RetryTracker::default())),
            dedup: None,
            cancel_checker: Some(Arc::new(
                RedisCancelChecker::new(redis_url).expect("cancel checker should connect"),
            )),
            metrics,
            worker_id: "worker-cancel-test".into(),
        })
    }

    #[tokio::test]
    async fn redis_cancel_keys_preempt_execution() {
        let redis = Redis::default()
            .start()
            .await
            .expect("redis testcontainer should start");
        let port = redis
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port should be mapped");
        let redis_url = format!("redis://127.0.0.1:{port}");
        let client = redis::Client::open(redis_url.as_str()).expect("redis URL should be valid");
        let calls = Arc::new(AtomicUsize::new(0));
        let consumer = consumer_with_cancel_checker(&redis_url, calls.clone()).await;

        let op_cancelled = task("cancelled-by-op-key", None);
        set_cancel_op_keys(&client, std::slice::from_ref(&op_cancelled.id))
            .await
            .expect("op cancel key should be written");
        consumer
            .process_message(BrokerMessage::new(op_cancelled, None))
            .await
            .expect("cancelled op-key task should be acknowledged");

        let batch_cancelled = task("cancelled-by-batch-key", Some("batch-cancelled"));
        set_cancel_batch_key(&client, "batch-cancelled")
            .await
            .expect("batch cancel key should be written");
        consumer
            .process_message(BrokerMessage::new(batch_cancelled, None))
            .await
            .expect("cancelled batch-key task should be acknowledged");

        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "cancelled tasks must not reach the executor"
        );
    }

    /// Regression: when only the result PUBLISH fails (here Redis is OOM under
    /// `noeviction`), the worker must retry the publish of the computed result,
    /// not re-run the sandbox job and then discard the verdict.
    #[tokio::test]
    async fn publish_failure_retries_publish_without_re_executing() {
        let redis = Redis::default().start().await.expect("redis");
        let port = redis.get_host_port_ipv4(6379).await.expect("port");
        let redis_url = format!("redis://127.0.0.1:{port}");
        let client = redis::Client::open(redis_url.as_str()).unwrap();
        let mut admin = client.get_multiplexed_async_connection().await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut consumer = consumer_with_cancel_checker(&redis_url, calls.clone()).await;
        consumer.cancel_checker = None;
        consumer.publish_retry = PublishRetry {
            base_delay_ms: 50,
            max_delay_ms: 200,
            deadline: std::time::Duration::from_secs(30),
        };

        let _: () = redis::cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory-policy")
            .arg("noeviction")
            .query_async(&mut admin)
            .await
            .unwrap();
        let _: () = redis::cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory")
            .arg("1")
            .query_async(&mut admin)
            .await
            .unwrap();

        let t = task("publish-retry", None);
        let reply_queue = t.reply_queue_name().to_string();
        let run =
            tokio::spawn(
                async move { consumer.process_message(BrokerMessage::new(t, None)).await },
            );

        // Let several publish attempts fail, then relieve the memory pressure.
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(!run.is_finished(), "the publish must still be retrying");
        let _: () = redis::cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory")
            .arg("0")
            .query_async(&mut admin)
            .await
            .unwrap();

        run.await
            .unwrap()
            .expect("the task completes once the publish goes through");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a publish failure must not re-run the job"
        );
        let depth: i64 = redis::cmd("ZCARD")
            .arg(&reply_queue)
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(depth, 1, "exactly one (successful) result is published");
    }
}
