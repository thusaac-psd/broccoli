use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Context;
use common::retry::{RetryTracker, spawn_cleanup_task};
use common::worker::Task;
use mq::{BroccoliError, BrokerMessage, MqConfig, init_mq};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use common::cancel::RedisCancelChecker;
use common::node_identity::{NodeClaim, NodeRole};

use crate::config::WorkerAppConfig;
use crate::consumer::{WorkerConsumer, WorkerConsumerDeps};
use crate::dedup::RedisTaskDedup;
use crate::fairness::{WorkerCapacity, effective_worker_capacity};
use crate::heartbeat::{HeartbeatConfig, HeartbeatHandle, InFlightCounter};
use crate::metrics::spawn_metrics_server;
use crate::models::worker::Worker;
use crate::system_info::{SystemInfo, is_wsl};

pub struct WorkerRuntime {
    config: WorkerAppConfig,
    mq: Arc<mq::Mq>,
    consumer: WorkerConsumer,
    in_flight: InFlightCounter,
    heartbeat: HeartbeatHandle,
    capacity: WorkerCapacity,
    _cleanup_handle: tokio::task::JoinHandle<()>,
    _warm_handle: tokio::task::JoinHandle<()>,
}

impl WorkerRuntime {
    pub async fn build(config: WorkerAppConfig) -> anyhow::Result<Self> {
        info!("Worker starting: {}", config.worker.id);

        crate::sandbox_preflight::validate_sandbox_config(
            &config.worker.sandbox_backend,
            config.worker.enable_cgroups,
            config.worker.allow_insecure_no_cgroups,
        )
        .map_err(|e| anyhow::anyhow!("sandbox startup refused: {e}"))?;

        if crate::sandbox_preflight::uses_isolate(&config.worker.sandbox_backend)
            && config.worker.enable_cgroups
        {
            let controllers = std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
                .map_err(|e| {
                    anyhow::anyhow!(
                        "sandbox startup refused: cgroup v2 controllers are not visible: {e}"
                    )
                })?;
            crate::sandbox_preflight::controllers_present(&controllers)
                .map_err(|e| anyhow::anyhow!("sandbox startup refused: {e}"))?;

            // Live self-test: run a real over-limit allocation inside isolate and
            // confirm the `cg-oom-killed` signal reaches us. If it does not, the
            // batch evaluator would silently misclassify MemoryLimitExceeded as
            // RuntimeError (it derives MLE solely from `cg_oom_killed`), so we
            // refuse to consume jobs rather than mis-judge every over-limit run.
            // The probe needs nothing from the built `Worker`, so it runs here
            // (fail fast, before executors are constructed) against a dedicated
            // manager built from config with cgroups ENABLED — `cg-oom-killed` is
            // only emitted under `--cg`.
            let probe_mgr = crate::models::operation::sandbox::isolate::IsolateSandboxManager::new(
                config.worker.isolate_bin.clone(),
                config.worker.enable_cgroups,
            );
            let outcome = crate::sandbox_preflight::probe_isolate_oom(&probe_mgr).await;
            crate::sandbox_preflight::gate_on_probe(outcome)
                .map_err(|e| anyhow::anyhow!("sandbox startup refused: {e}"))?;
        }

        let system_info = SystemInfo::detect();
        let capacity = effective_worker_capacity(
            config.worker.max_concurrency,
            config.worker.fairness_unsafe_allow,
            &system_info.os,
            is_wsl(),
        );
        if capacity.was_clamped {
            warn!(
                configured_max_concurrency = config.worker.max_concurrency,
                effective_max_concurrency = capacity.max_concurrency,
                os = %system_info.os,
                "Clamping worker max_concurrency to 1; set worker.fairness_unsafe_allow=true to override on this platform"
            );
        } else if capacity.max_concurrency > 1 {
            info!(
                configured_max_concurrency = config.worker.max_concurrency,
                effective_max_concurrency = capacity.max_concurrency,
                fairness_mode = capacity.fairness_mode.as_str(),
                "Worker max_concurrency > 1 enabled"
            );
        }

        let (metrics, prometheus_registry) =
            common::observability::init_metrics(&config.observability.otlp.service_name);
        // Reflect the real permit ceiling: run_consume_loop admits work through a
        // single Semaphore::new(max_concurrency.max(1)). The gauge previously
        // multiplied by a phantom queue_consumer_count = 2, reporting twice the
        // permits that actually exist and skewing saturation dashboards.
        metrics.worker_permits_max.add(
            capacity.max_concurrency.max(1) as i64,
            &[opentelemetry::KeyValue::new(
                "worker_id",
                config.worker.id.clone(),
            )],
        );

        spawn_metrics_server(prometheus_registry);

        check_clock_skew(&config).await?;

        let mq = Arc::new(
            init_mq(MqConfig {
                url: config.mq.url.clone(),
                pool_size: config.mq.pool_size,
            })
            .await
            .context("Failed to initialize MQ")?,
        );

        info!(
            operation_queue_name = %config.mq.operation_queue_name,
            operation_dlq_queue_name = %config.mq.operation_dlq_queue_name,
            max_retries = config.mq.dlq.max_retries,
            "MQ connected"
        );

        // Claim the worker ID before acting under it: a second live process with
        // the same ID would share this worker's private queue, dedup claims and
        // heartbeat, so it refuses to start instead.
        info!(
            hostname = ?system_info.hostname,
            ip_addresses = ?system_info.ip_addresses,
            os = %system_info.os,
            arch = %system_info.arch,
            cpu_count = system_info.cpu_count,
            pid = system_info.pid,
            "Detected system info for heartbeat"
        );
        let in_flight = InFlightCounter::new();
        let started_at = chrono::Utc::now();
        let heartbeat_config = HeartbeatConfig {
            worker_id: config.worker.id.clone(),
            sandbox_backend: config.worker.sandbox_backend.clone(),
            max_concurrency: Some(capacity.max_concurrency as u32),
            fairness_mode: capacity.fairness_mode.as_str().into(),
            system_info,
        };
        let claim = NodeClaim::claim_at_boot(
            redis::Client::open(config.mq.url.as_str()).context("Invalid MQ URL")?,
            NodeRole::Worker,
            &config.worker.id,
            heartbeat_config.payload(started_at, &in_flight),
        )
        .await?;
        // Renew from now on: executor setup below can outlast the claim's TTL.
        let heartbeat =
            crate::heartbeat::spawn(claim, heartbeat_config, started_at, in_flight.clone());

        let dedup = match RedisTaskDedup::new(
            &config.mq.url,
            config.worker.dedup_ttl_secs,
            config.worker.id.clone(),
        ) {
            Ok(d) => {
                info!(
                    "Task dedup initialized (fallback TTL={}s; per-operation TTL derived from step resource limits)",
                    config.worker.dedup_ttl_secs
                );
                Some(Arc::new(d))
            }
            Err(e) => {
                warn!(error = %e, "Failed to initialize task dedup, running without dedup");
                None
            }
        };

        let cancel_checker = match RedisCancelChecker::new(&config.mq.url) {
            Ok(c) => Some(Arc::new(c)),
            Err(e) => {
                warn!(error = %e, "Failed to initialize Redis cancel checker; tasks will not honor host-side cancel keys");
                None
            }
        };

        let worker = Arc::new(
            Worker::from_config(&config, metrics.clone())
                .await
                .context("Failed to initialize worker executors")?,
        );

        // Pre-warm subscriber: listens for contest cache-warm jobs and fills the
        // local file cache ahead of the opening burst. Independent of judging.
        let warm_handle = crate::warm::spawn(config.clone(), Some(metrics.clone()));

        let dlq_config = config.mq.dlq.clone();
        let retry_tracker = Arc::new(Mutex::new(RetryTracker::new(dlq_config.max_retries)));

        let cleanup_handle = spawn_cleanup_task(
            retry_tracker.clone(),
            Duration::from_secs(dlq_config.retry_cleanup_interval_secs),
            Duration::from_secs(dlq_config.retry_max_age_secs),
        );

        let consumer = WorkerConsumer::new(WorkerConsumerDeps {
            worker,
            mq: Arc::clone(&mq),
            dlq_queue: config.mq.operation_dlq_queue_name.clone(),
            dlq_config,
            retry_tracker,
            dedup,
            cancel_checker,
            metrics,
            worker_id: config.worker.id.clone(),
        });

        Ok(Self {
            config,
            mq,
            consumer,
            in_flight,
            heartbeat,
            capacity,
            _cleanup_handle: cleanup_handle,
            _warm_handle: warm_handle,
        })
    }

    pub async fn run(mut self) -> anyhow::Result<()> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_for_signal = shutdown.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                info!("Shutdown signal received; refusing new tasks and beginning drain");
                shutdown_for_signal.store(true, Ordering::SeqCst);
            }
        });

        let drain_timeout = Duration::from_secs(30);

        let shared_queue = self.config.mq.operation_queue_name.clone();
        let private_queue = common::worker::worker_private_queue_name(
            &self.config.mq.operation_queue_name,
            &self.config.worker.id,
        );
        info!(
            shared_queue = %shared_queue,
            private_queue = %private_queue,
            "Subscribing to operation queues"
        );

        // The manual consume loop the worker fully controls (see
        // [`run_consume_loop`]): on shutdown it stops pulling new tasks, leaving
        // still-queued work in Redis, instead of the old `process_messages(Some(N))`
        // whose DETACHED consume tasks kept pulling through the drain window and
        // rejected every pulled task into the failed queue on every shutdown.
        let consumer = self.consumer.clone();
        run_consume_loop(
            Arc::clone(&self.mq),
            [shared_queue, private_queue],
            self.capacity.max_concurrency,
            self.in_flight.clone(),
            shutdown.clone(),
            drain_timeout,
            move |message| {
                let consumer = consumer.clone();
                async move { consumer.run_task(message).await }
            },
        )
        .await;

        self.heartbeat.shutdown().await;
        info!("Worker stopped");
        Ok(())
    }
}

/// The worker's operation consume loop, extracted from [`WorkerRuntime::run`] so
/// it can be driven directly in tests. It OWNS the consume decision: it checks
/// `shutdown` before every `try_consume`, so the instant shutdown is signaled it
/// stops pulling and any still-queued work stays in the broker for another
/// worker. A task already taken is run to completion via `handle` and then acked
/// (or rejected for retry on `Err`) during the drain. One semaphore bounds total
/// in-flight work to `max_concurrency` across both queues.
/// Compare this worker's clock with Postgres and Redis (see `common::clock`)
/// on a short-lived connection; the worker keeps no shared pool of its own.
async fn check_clock_skew(config: &WorkerAppConfig) -> anyhow::Result<()> {
    let db = common::db::connect(
        &config.database.url,
        &common::db::PgPoolSpec::new(1, 0),
        &config.database.session(),
    )
    .await
    .context("Failed to connect to the database for the clock skew check")?;
    let redis = if config.mq.enabled {
        Some(redis::Client::open(config.mq.url.as_str()).context("Invalid mq.url")?)
    } else {
        None
    };
    let result = common::clock::check_clock_skew(&db, redis.as_ref(), &config.clock).await;
    let _ = db.close().await;
    result
}

async fn run_consume_loop<F, Fut>(
    mq: Arc<mq::Mq>,
    queues: [String; 2],
    max_concurrency: usize,
    in_flight: InFlightCounter,
    shutdown: Arc<AtomicBool>,
    drain_timeout: Duration,
    handle: F,
) where
    F: Fn(BrokerMessage<Task>) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<(), BroccoliError>> + Send + 'static,
{
    let permits = Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1)));
    let idle_poll = Duration::from_millis(50);
    let consume_backoff = Duration::from_secs(1);
    let mut next_queue = 0usize;

    while !shutdown.load(Ordering::Relaxed) {
        // Bound concurrency, but wake on shutdown rather than blocking on a permit
        // while every slot is busy.
        let permit = tokio::select! {
            biased;
            _ = wait_for_shutdown(&shutdown) => break,
            permit = Arc::clone(&permits).acquire_owned() => match permit {
                Ok(p) => p,
                Err(_) => break,
            }
        };
        if shutdown.load(Ordering::Relaxed) {
            break;
        }

        // Poll each queue once (round-robin) for a ready task. `try_consume` is
        // non-blocking, so an empty queue never parks us past a shutdown check.
        let mut taken: Option<(String, BrokerMessage<Task>)> = None;
        let mut had_error = false;
        for _ in 0..queues.len() {
            let queue = queues[next_queue % queues.len()].clone();
            next_queue = next_queue.wrapping_add(1);
            match mq.try_consume::<Task>(&queue, None).await {
                Ok(Some(message)) => {
                    taken = Some((queue, message));
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    warn!(queue = %queue, error = %e, "Operation consume error; backing off");
                    had_error = true;
                    break;
                }
            }
        }

        let Some((queue, message)) = taken else {
            // Nothing ready (or a transient consume error): release the permit and
            // idle briefly, still responsive to shutdown. The broker returns a
            // connection error after a few bounded retries, so a consume error
            // backs off here and the next poll reconnects.
            drop(permit);
            let backoff = if had_error {
                consume_backoff
            } else {
                idle_poll
            };
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = wait_for_shutdown(&shutdown) => break,
            }
            continue;
        };

        let mq = Arc::clone(&mq);
        let handle = handle.clone();
        // Acquire the in-flight guard SYNCHRONOUSLY here, before the spawn, so the
        // counter is incremented the instant the task is consumed - not on the
        // spawned future's first poll. Otherwise, if shutdown fires right after
        // this consume, the loop can break and `drain_in_flight` read current()==0
        // (the future not yet polled) and return, abandoning this just-consumed
        // task mid-flight (recovered only later via broker redelivery). The guard
        // is moved into the task and dropped when it is fully resolved.
        let in_flight_guard = in_flight.guard();
        tokio::spawn(async move {
            // Hold the in-flight guard and the permit across BOTH processing and
            // the ack/reject, so `drain_in_flight` waits until the task is fully
            // resolved (acked or rejected), not just executed.
            let _in_flight = in_flight_guard;
            let _permit = permit;
            match handle(message.clone()).await {
                Ok(()) => {
                    if let Err(e) = mq.acknowledge::<Task>(&queue, message).await {
                        error!(queue = %queue, error = %e, "Failed to acknowledge task");
                    }
                }
                Err(e) => {
                    warn!(queue = %queue, error = %e, "Task processing failed; rejecting for retry");
                    if let Err(e) = mq.reject::<Task>(&queue, message).await {
                        error!(queue = %queue, error = %e, "Failed to reject task");
                    }
                }
            }
        });
    }

    info!("Shutdown signaled; no longer consuming. Draining in-flight tasks");
    drain_in_flight(&in_flight, drain_timeout).await;
}

async fn wait_for_shutdown(flag: &Arc<AtomicBool>) {
    while !flag.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn drain_in_flight(in_flight: &InFlightCounter, timeout: Duration) {
    if in_flight.current() == 0 {
        info!("No in-flight tasks; shutting down immediately");
        return;
    }
    info!(
        in_flight = in_flight.current(),
        timeout_secs = timeout.as_secs(),
        "Waiting for in-flight tasks to drain"
    );
    let drain = async {
        while in_flight.current() > 0 {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    match tokio::time::timeout(timeout, drain).await {
        Ok(()) => info!("All in-flight tasks drained cleanly"),
        Err(_) => warn!(
            remaining = in_flight.current(),
            "Drain timeout exceeded; exiting with in-flight tasks still active"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    fn task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            task_type: "operation".to_string(),
            executor_name: "operation".to_string(),
            payload: serde_json::json!({}),
            result_queue: "results".to_string(),
            operation_batch_id: None,
            reply_queue: None,
            priority: None,
            trace_context: None,
            enqueued_at_unix_ms: None,
        }
    }

    async fn redis_mq() -> (Arc<mq::Mq>, testcontainers::ContainerAsync<Redis>) {
        let container = Redis::default().start().await.expect("start redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port");
        let mq = init_mq(MqConfig {
            url: format!("redis://127.0.0.1:{port}"),
            pool_size: 4,
        })
        .await
        .expect("init mq");
        (Arc::new(mq), container)
    }

    /// Regression test for the shutdown task-loss bug: with shutdown ALREADY
    /// signaled, the loop consumes nothing, so queued tasks stay in the broker
    /// instead of being pulled and rejected into the failed queue.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_before_start_leaves_queued_tasks_in_the_broker() {
        let (mq, _container) = redis_mq().await;
        let queue = "operation_tasks".to_string();
        for i in 0..3 {
            mq.publish::<Task>(&queue, None, &task(&format!("t{i}")), None)
                .await
                .expect("publish");
        }

        run_consume_loop(
            Arc::clone(&mq),
            [queue.clone(), format!("{queue}:worker:w")],
            4,
            InFlightCounter::new(),
            Arc::new(AtomicBool::new(true)), // already shutting down
            Duration::from_secs(1),
            |_msg| async { Ok(()) },
        )
        .await;

        let mut remaining = 0;
        while mq
            .try_consume::<Task>(&queue, None)
            .await
            .unwrap()
            .is_some()
        {
            remaining += 1;
        }
        assert_eq!(
            remaining, 3,
            "a worker shutting down must leave every queued task untouched in the broker"
        );
    }

    /// Happy path: the loop consumes queued tasks, runs the handler, and acks
    /// them (they do not reappear), then stops cleanly when shutdown is signaled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn consumes_and_acks_queued_tasks_then_stops_on_shutdown() {
        let (mq, _container) = redis_mq().await;
        let queue = "operation_tasks".to_string();
        for i in 0..5 {
            mq.publish::<Task>(&queue, None, &task(&format!("t{i}")), None)
                .await
                .expect("publish");
        }

        let processed = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));

        let loop_processed = Arc::clone(&processed);
        let loop_shutdown = Arc::clone(&shutdown);
        let loop_mq = Arc::clone(&mq);
        let loop_queue = queue.clone();
        let join = tokio::spawn(async move {
            run_consume_loop(
                loop_mq,
                [loop_queue.clone(), format!("{loop_queue}:worker:w")],
                4,
                InFlightCounter::new(),
                loop_shutdown,
                Duration::from_secs(5),
                move |_msg| {
                    let processed = Arc::clone(&loop_processed);
                    async move {
                        processed.fetch_add(1, Ordering::Relaxed);
                        Ok(())
                    }
                },
            )
            .await;
        });

        for _ in 0..100 {
            if processed.load(Ordering::Relaxed) >= 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(
            processed.load(Ordering::Relaxed),
            5,
            "the loop must consume and process every queued task"
        );

        shutdown.store(true, Ordering::Relaxed);
        join.await.expect("consume loop joins after shutdown");

        assert!(
            mq.try_consume::<Task>(&queue, None)
                .await
                .unwrap()
                .is_none(),
            "acked tasks must not reappear in the queue"
        );
    }
}
