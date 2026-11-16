use std::sync::Arc;

use common::metrics::Metrics;
use common::retry::{RetryCleanupGuard, RetryDecision, RetryTracker, calculate_backoff};
use common::worker::{Task, TaskResult};
use common::{DlqConfig, DlqEnvelope, DlqErrorCode, DlqMessageType};
use mq::{Delivery, Mq, StreamRole};
use opentelemetry::KeyValue;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use common::cancel::RedisCancelChecker;

use crate::metrics::{TaskMetricGuard, WorkerPermitMetricGuard, record_mq_consume};
use crate::models::worker::Worker;
use crate::task_runner::{PublishRetry, execute_task, publish_result};

/// What the consume loop does with a delivery once its handler is done.
#[derive(Debug)]
pub enum Settlement {
    /// Done with it: acknowledge and delete.
    Ack,
    /// Move it to the dead-letter stream as this envelope.
    DeadLetter(Box<DlqEnvelope>),
    /// Leave it pending and stop refreshing it, so it is redelivered once it
    /// has been idle for `mq::CLAIM_IDLE`.
    Abandon,
}

#[derive(Clone)]
pub struct WorkerConsumer {
    worker: Arc<Worker>,
    mq: Mq,
    dlq_config: DlqConfig,
    retry_tracker: Arc<Mutex<RetryTracker>>,
    cancel_checker: Option<Arc<RedisCancelChecker>>,
    metrics: Metrics,
    worker_id: String,
    publish_retry: PublishRetry,
    clock: common::clock::PlatformClock,
}

pub struct WorkerConsumerDeps {
    pub worker: Arc<Worker>,
    pub mq: Mq,
    pub dlq_config: DlqConfig,
    pub retry_tracker: Arc<Mutex<RetryTracker>>,
    pub cancel_checker: Option<Arc<RedisCancelChecker>>,
    pub metrics: Metrics,
    pub worker_id: String,
    /// Stamps the retry history the server files with a dead letter.
    pub clock: common::clock::PlatformClock,
}

impl WorkerConsumer {
    pub fn new(deps: WorkerConsumerDeps) -> Self {
        Self {
            worker: deps.worker,
            mq: deps.mq,
            dlq_config: deps.dlq_config,
            retry_tracker: deps.retry_tracker,
            cancel_checker: deps.cancel_checker,
            metrics: deps.metrics,
            worker_id: deps.worker_id,
            publish_retry: PublishRetry::default(),
            clock: deps.clock,
        }
    }

    /// Process one delivery under the per-task worker-permit metric guard.
    ///
    /// The consume loop in `crate::runtime` stops reading the instant shutdown
    /// is signaled, so a delivery handed to this is always run to completion
    /// during the drain window.
    pub async fn run_task(&self, delivery: Delivery<Task>) -> Settlement {
        let _worker_permit_guard = WorkerPermitMetricGuard::new(&self.metrics, &self.worker_id);
        self.process_delivery(delivery).await
    }

    #[tracing::instrument(skip(self, delivery), fields(job_id, task_type, executor))]
    pub async fn process_delivery(&self, delivery: Delivery<Task>) -> Settlement {
        let consume_start = std::time::Instant::now();
        let stream = self
            .mq
            .topology()
            .role_of(&delivery.entry.stream)
            .unwrap_or(StreamRole::Shared);
        let deliveries = delivery.deliveries;
        let task = match delivery.payload {
            Ok(task) => task,
            Err(e) => {
                error!(error = %e, "Dead-lettering an undecodable operation task");
                record_mq_consume(
                    &self.metrics,
                    stream,
                    "undecodable",
                    deliveries,
                    consume_start,
                );
                return Settlement::DeadLetter(Box::new(DlqEnvelope {
                    message_id: delivery.entry.id.clone(),
                    message_type: DlqMessageType::OperationTask,
                    submission_id: None,
                    payload: serde_json::json!({ "raw": e.raw }),
                    error_code: DlqErrorCode::DeserializationError,
                    error_message: e.reason,
                    retry_history: vec![],
                }));
            }
        };
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

        let queued_secs = delivery.queued_for.as_secs_f64();
        self.metrics.mq_message_age.record(
            queued_secs,
            &[
                KeyValue::new("queue", stream.as_str()),
                KeyValue::new("message.type", "operation_task"),
            ],
        );
        self.metrics
            .task_queue_wait_duration
            .record(queued_secs, &task_attrs);
        if deliveries > 1 {
            self.metrics
                .mq_redeliveries_total
                .add(1, &[KeyValue::new("queue", stream.as_str())]);
        }

        if delivery.deliveries > mq::MAX_DELIVERIES {
            return self
                .finish_poison(&task, &task_attrs, stream, deliveries, consume_start)
                .await;
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
                        stream,
                        "cancelled_by_host",
                        deliveries,
                        consume_start,
                    );
                    return Settlement::Ack;
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
            deliveries,
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
                    record_mq_consume(&self.metrics, stream, outcome, deliveries, consume_start);
                    // A result that could not be delivered within the publish
                    // window leaves the task to be redelivered.
                    return match published {
                        Ok(()) => Settlement::Ack,
                        Err(_) => Settlement::Abandon,
                    };
                }
                Err(e) => {
                    let error_str = e.to_string();
                    let decision = self.retry_tracker.lock().await.record_failure(
                        &task_id,
                        &error_str,
                        self.clock.now(),
                    );

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
                            let mut exhausted_attrs = task_attrs.clone();
                            exhausted_attrs.push(KeyValue::new("outcome", "exhausted"));
                            self.metrics.task_retries_total.add(1, &exhausted_attrs);
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
                            cleanup_guard.defuse();
                            record_mq_consume(
                                &self.metrics,
                                stream,
                                "exhausted",
                                deliveries,
                                consume_start,
                            );
                            let message = format!(
                                "Operation failed after {} retries: {error_str}",
                                history.len()
                            );
                            return self
                                .dead_letter_with_reply(
                                    &task,
                                    &task_attrs,
                                    message,
                                    DlqErrorCode::MaxRetriesExceeded,
                                    error_str,
                                    history,
                                )
                                .await;
                        }
                    }
                }
            }
        }
    }

    /// A task every earlier consumer died or wedged on: do not run it again.
    async fn finish_poison(
        &self,
        task: &Task,
        task_attrs: &[KeyValue],
        stream: StreamRole,
        deliveries: u32,
        consume_start: std::time::Instant,
    ) -> Settlement {
        error!(
            job_id = %task.id,
            deliveries,
            "Task was delivered more than {} times; sending to DLQ without running it",
            mq::MAX_DELIVERIES
        );
        let mut attrs = task_attrs.to_vec();
        attrs.push(KeyValue::new("outcome", "redelivery_exhausted"));
        self.metrics.tasks_completed_total.add(1, &attrs);
        record_mq_consume(
            &self.metrics,
            stream,
            "redelivery_exhausted",
            deliveries,
            consume_start,
        );
        let reason = format!(
            "Operation was delivered {deliveries} times and never finished: the workers running it stopped"
        );
        self.dead_letter_with_reply(
            task,
            task_attrs,
            reason.clone(),
            DlqErrorCode::RedeliveryExhausted,
            reason,
            vec![],
        )
        .await
    }

    /// Tell the waiting server the task failed with `reply_error`, then hand
    /// the task to the dead-letter stream.
    async fn dead_letter_with_reply(
        &self,
        task: &Task,
        task_attrs: &[KeyValue],
        reply_error: String,
        error_code: DlqErrorCode,
        error_message: String,
        retry_history: Vec<common::retry::RetryAttempt>,
    ) -> Settlement {
        let mut dlq_attrs = task_attrs.to_vec();
        dlq_attrs.push(KeyValue::new("error_code", error_code.as_str()));
        self.metrics.dlq_messages_total.add(1, &dlq_attrs);

        let mut error_result = TaskResult {
            task_id: task.id.clone(),
            success: false,
            output: serde_json::json!({}),
            error: Some(reply_error),
            task_type: None,
            operation: None,
            worker_id: None,
        };
        error_result.attach_worker_metadata(task, &self.worker_id);
        if publish_result(
            task,
            &error_result,
            &self.mq,
            &self.metrics,
            self.publish_retry,
        )
        .await
        .is_err()
        {
            error!(job_id = %task.id, "Failed to publish the error result for a dead-lettered task");
        }

        let payload = serde_json::to_value(task).unwrap_or_else(|ser_err| {
            error!(error = %ser_err, "Failed to serialize task for DLQ");
            serde_json::json!({ "task_id": task.id })
        });
        Settlement::DeadLetter(Box::new(DlqEnvelope {
            message_id: task.id.clone(),
            message_type: DlqMessageType::OperationTask,
            submission_id: None,
            payload,
            error_code,
            error_message,
            retry_history,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use common::cancel::{RedisCancelChecker, set_cancel_batch_key, set_cancel_op_keys};
    use common::liveness::NodeRole;
    use common::node_identity::NodeClaim;
    use common::worker::Executor;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    const SERVER: &str = "server-1";

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
            })
        }
    }

    fn task(id: &str, batch_id: Option<&str>) -> Task {
        Task {
            id: id.to_string(),
            task_type: "operation".into(),
            executor_name: "operation".into(),
            payload: serde_json::json!({}),
            reply_to: SERVER.into(),
            operation_batch_id: batch_id.map(str::to_string),
            trace_context: None,
        }
    }

    struct Harness {
        _redis: testcontainers::ContainerAsync<Redis>,
        _server: NodeClaim,
        url: String,
        mq: Mq,
        consumer: WorkerConsumer,
        calls: Arc<AtomicUsize>,
        admin: redis::aio::MultiplexedConnection,
    }

    /// A worker consumer against a fresh Redis in which server `SERVER` is
    /// live and has its reply stream.
    async fn harness() -> Harness {
        let redis = common::test_util::redis_image()
            .start()
            .await
            .expect("redis");
        let port = redis.get_host_port_ipv4(6379).await.expect("port");
        let url = format!("redis://127.0.0.1:{port}");
        let mq = Mq::connect(&url, "ops").await.expect("mq");
        let admin = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        // Server `SERVER` as it runs: its claim owns its reply stream, which
        // the first renewal creates.
        let server = NodeClaim::claim_at_boot(
            redis::Client::open(url.as_str()).unwrap(),
            "ops",
            NodeRole::Server,
            SERVER,
            serde_json::json!({}),
        )
        .await
        .unwrap()
        .own_stream(mq.topology().reply(SERVER), mq::REPLY_STREAM_TTL, || {});
        server.renew(serde_json::json!({})).await.unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let executor: Arc<dyn Executor> = Arc::new(CountingExecutor {
            calls: calls.clone(),
        });
        let worker = Worker::new([("operation".to_string(), executor)]);
        let (metrics, _registry) = common::observability::init_metrics("worker-consumer-test");
        let consumer = WorkerConsumer::new(WorkerConsumerDeps {
            worker: Arc::new(worker),
            mq: mq.clone(),
            dlq_config: DlqConfig::default(),
            retry_tracker: Arc::new(Mutex::new(RetryTracker::default())),
            cancel_checker: Some(Arc::new(
                RedisCancelChecker::new(&url).expect("cancel checker should connect"),
            )),
            metrics,
            worker_id: "worker-test".into(),
            clock: common::clock::PlatformClock::local_for_tests(),
        });
        Harness {
            _redis: redis,
            _server: server,
            url,
            mq,
            consumer,
            calls,
            admin,
        }
    }

    impl Harness {
        /// Publish `t` and take it as worker `worker-test`.
        async fn deliver(&self, t: &Task) -> Delivery<Task> {
            let group = self.mq.task_consumer("worker-test");
            group.ensure_groups().await.unwrap();
            self.mq.publish_task(t, None).await.unwrap();
            group.reader().read(1).await.unwrap().remove(0)
        }

        /// Commands Redis has refused for lack of memory so far: each failed
        /// reply publish is one.
        async fn oom_rejections(&mut self) -> u64 {
            let stats: String = redis::cmd("INFO")
                .arg("errorstats")
                .query_async(&mut self.admin)
                .await
                .unwrap();
            stats
                .lines()
                .find_map(|line| line.strip_prefix("errorstat_OOM:count="))
                .and_then(|n| n.trim().parse().ok())
                .unwrap_or(0)
        }

        async fn replies(&mut self) -> u64 {
            redis::cmd("XLEN")
                .arg(self.mq.topology().reply(SERVER))
                .query_async(&mut self.admin)
                .await
                .unwrap()
        }
    }

    #[tokio::test]
    async fn redis_cancel_keys_preempt_execution() {
        let h = harness().await;
        let client = redis::Client::open(h.url.as_str()).unwrap();

        let op_cancelled = task("cancelled-by-op-key", None);
        set_cancel_op_keys(&client, std::slice::from_ref(&op_cancelled.id))
            .await
            .expect("op cancel key should be written");
        let d = h.deliver(&op_cancelled).await;
        assert!(matches!(
            h.consumer.process_delivery(d).await,
            Settlement::Ack
        ));

        let batch_cancelled = task("cancelled-by-batch-key", Some("batch-cancelled"));
        set_cancel_batch_key(&client, "batch-cancelled")
            .await
            .expect("batch cancel key should be written");
        let d = h.deliver(&batch_cancelled).await;
        assert!(matches!(
            h.consumer.process_delivery(d).await,
            Settlement::Ack
        ));

        assert_eq!(
            h.calls.load(Ordering::SeqCst),
            0,
            "cancelled tasks must not reach the executor"
        );
    }

    /// When only the result PUBLISH fails (here Redis is OOM under
    /// `noeviction`), the worker retries the publish of the computed result,
    /// not the sandbox job.
    #[tokio::test]
    async fn publish_failure_retries_publish_without_re_executing() {
        let mut h = harness().await;
        let mut consumer = h.consumer.clone();
        consumer.cancel_checker = None;
        consumer.publish_retry = PublishRetry {
            base_delay_ms: 50,
            max_delay_ms: 200,
            deadline: std::time::Duration::from_secs(30),
        };
        let d = h.deliver(&task("publish-retry", None)).await;

        let config = |value: &str| {
            redis::cmd("CONFIG")
                .arg("SET")
                .arg("maxmemory")
                .arg(value)
                .to_owned()
        };
        let _: () = redis::cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory-policy")
            .arg("noeviction")
            .query_async(&mut h.admin)
            .await
            .unwrap();
        let _: () = config("1").query_async(&mut h.admin).await.unwrap();

        let run = tokio::spawn(async move { consumer.process_delivery(d).await });
        // Wait until the publish has failed (and been retried) twice, then
        // relieve the memory pressure.
        while h.oom_rejections().await < 2 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(!run.is_finished(), "the publish must still be retrying");
        let _: () = config("0").query_async(&mut h.admin).await.unwrap();

        assert!(matches!(run.await.unwrap(), Settlement::Ack));
        assert_eq!(
            h.calls.load(Ordering::SeqCst),
            1,
            "a publish failure must not re-run the job"
        );
        assert_eq!(h.replies().await, 1, "exactly one result is published");
    }

    /// A result that cannot be delivered within the publish window leaves
    /// the task pending, to be redelivered.
    #[tokio::test]
    async fn an_undeliverable_result_abandons_the_task() {
        let mut h = harness().await;
        let mut consumer = h.consumer.clone();
        consumer.cancel_checker = None;
        consumer.publish_retry = PublishRetry {
            base_delay_ms: 50,
            max_delay_ms: 100,
            deadline: std::time::Duration::from_millis(500),
        };
        let d = h.deliver(&task("publish-exhausted", None)).await;
        // Redis is full: every reply publish fails until the window runs out.
        let _: () = redis::pipe()
            .cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory-policy")
            .arg("noeviction")
            .ignore()
            .cmd("CONFIG")
            .arg("SET")
            .arg("maxmemory")
            .arg("1")
            .ignore()
            .query_async(&mut h.admin)
            .await
            .unwrap();

        assert!(matches!(
            consumer.process_delivery(d).await,
            Settlement::Abandon
        ));
        assert_eq!(h.calls.load(Ordering::SeqCst), 1);
    }

    /// A reply for a server that is gone is dropped, and the task is done.
    #[tokio::test]
    async fn a_result_for_a_gone_server_is_dropped() {
        let mut h = harness().await;
        let mut consumer = h.consumer.clone();
        consumer.cancel_checker = None;
        let mut t = task("orphan", None);
        t.reply_to = "gone".into();
        let d = h.deliver(&t).await;
        assert!(matches!(
            consumer.process_delivery(d).await,
            Settlement::Ack
        ));
        assert_eq!(h.replies().await, 0);
    }

    /// A task over the delivery limit is not run: the server gets a failed
    /// reply and the task becomes a dead letter.
    #[tokio::test]
    async fn a_poison_task_is_dead_lettered_without_running() {
        let mut h = harness().await;
        let t = task("poison", None);
        let group = h.mq.task_consumer("worker-test");
        group.ensure_groups().await.unwrap();
        h.mq.publish_task(&t, None).await.unwrap();
        let mut d = group.reader().read(1).await.unwrap().remove(0);
        let mut owner = "worker-test".to_string();
        // Each worker that takes it dies holding it.
        for n in 0..mq::MAX_DELIVERIES {
            common::test_util::age_entry(
                &mut h.admin,
                &d.entry.stream,
                mq::WORKERS_GROUP,
                &owner,
                &d.entry.id,
                mq::CLAIM_IDLE * 2,
            )
            .await;
            owner = format!("crashed-{n}");
            d =
                h.mq.task_consumer(&owner)
                    .reclaim(1)
                    .await
                    .unwrap()
                    .remove(0);
        }
        assert!(d.is_poison());

        let settlement = h.consumer.process_delivery(d).await;
        let Settlement::DeadLetter(envelope) = settlement else {
            panic!("expected a dead letter, got {settlement:?}");
        };
        assert_eq!(envelope.error_code, DlqErrorCode::RedeliveryExhausted);
        assert_eq!(h.calls.load(Ordering::SeqCst), 0);
        assert_eq!(h.replies().await, 1, "the server is told the task failed");
    }
}
