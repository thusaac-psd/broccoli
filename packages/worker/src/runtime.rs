use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use common::retry::{RetryTracker, spawn_cleanup_task};
use common::worker::Task;
use mq::{Delivery, EntryRef, GroupConsumer, Mq, Settled};
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use common::cancel::RedisCancelChecker;
use common::liveness::NodeRole;
use common::node_identity::NodeClaim;

use crate::config::WorkerAppConfig;
use crate::consumer::{Settlement, WorkerConsumer, WorkerConsumerDeps};
use crate::fairness::{WorkerCapacity, effective_worker_capacity};
use crate::heartbeat::{HeartbeatConfig, HeartbeatHandle, InFlightCounter};
use crate::metrics::spawn_metrics_server;
use crate::models::worker::Worker;
use crate::system_info::{SystemInfo, is_wsl};

pub struct WorkerRuntime {
    config: WorkerAppConfig,
    mq: Mq,
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

        let mq = Mq::connect(&config.mq.url, &config.mq.operation_queue_name)
            .await
            .context("Failed to initialize MQ")?;

        info!(
            operation_queue_name = %config.mq.operation_queue_name,
            max_retries = config.mq.dlq.max_retries,
            "MQ connected"
        );

        // Claim the worker ID before acting under it: a second live process with
        // the same ID would share this worker's private stream, pending entries
        // and heartbeat, so it refuses to start instead.
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
            mq: mq.clone(),
            dlq_config,
            retry_tracker,
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

        let tasks = self.mq.task_consumer(&self.config.worker.id);
        info!(streams = ?tasks.streams(), "Consuming operation tasks");
        let consumer = self.consumer.clone();
        run_consume_loop(
            self.mq.clone(),
            tasks,
            self.capacity.max_concurrency,
            self.in_flight.clone(),
            shutdown.clone(),
            drain_timeout,
            move |delivery| {
                let consumer = consumer.clone();
                async move { consumer.run_task(delivery).await }
            },
        )
        .await;

        self.heartbeat.shutdown().await;
        info!("Worker stopped");
        Ok(())
    }
}

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

/// The entries this worker holds, read but not yet settled. They are
/// refreshed every [`mq::REFRESH_INTERVAL`] so no other worker reclaims them.
#[derive(Clone, Default)]
struct Held(Arc<StdMutex<HashSet<EntryRef>>>);

impl Held {
    fn insert(&self, entry: &EntryRef) {
        self.0.lock().unwrap().insert(entry.clone());
    }

    fn remove(&self, entry: &EntryRef) {
        self.0.lock().unwrap().remove(entry);
    }

    fn snapshot(&self) -> Vec<EntryRef> {
        self.0.lock().unwrap().iter().cloned().collect()
    }
}

/// The worker's consume loop, extracted from [`WorkerRuntime::run`] so it can
/// be driven directly in tests.
///
/// It reads from the worker's private and shared streams with one blocking
/// read for as many tasks as it has free slots, and every
/// [`mq::REFRESH_INTERVAL`] reclaims tasks other workers abandoned and drains
/// dead workers' private streams. Entries it reads beyond its free slots wait
/// in a local buffer. A task is run by `handle`, which says how to settle it.
/// Every held entry, running or buffered, is refreshed until it is settled.
///
/// Once `shutdown` is set it reads nothing more, gives buffered tasks back to
/// their stream, and waits up to `drain_timeout` for running ones.
async fn run_consume_loop<F, Fut>(
    mq: Mq,
    tasks: GroupConsumer<Task>,
    max_concurrency: usize,
    in_flight: InFlightCounter,
    shutdown: Arc<AtomicBool>,
    drain_timeout: Duration,
    handle: F,
) where
    F: Fn(Delivery<Task>) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Settlement> + Send + 'static,
{
    if let Err(e) = tasks.ensure_groups().await {
        warn!(error = %e, "Creating the consumer groups failed; the first read retries it");
    }
    let permits = Arc::new(tokio::sync::Semaphore::new(max_concurrency.max(1)));
    let held = Held::default();
    let refresher = tokio::spawn(refresh_held(tasks.clone(), held.clone()));
    let mut reader = tasks.reader();
    let mut buffer: VecDeque<Delivery<Task>> = VecDeque::new();
    let mut next_reclaim = Instant::now();

    while !shutdown.load(Ordering::Relaxed) {
        let permit = tokio::select! {
            biased;
            _ = wait_for_shutdown(&shutdown) => break,
            permit = Arc::clone(&permits).acquire_owned() => match permit {
                Ok(p) => p,
                Err(_) => break,
            }
        };

        if buffer.is_empty() {
            // Room for the permit just taken plus every other free one.
            let room = permits.available_permits() + 1;
            let fetched = if Instant::now() >= next_reclaim {
                next_reclaim = Instant::now() + mq::REFRESH_INTERVAL;
                if let Err(e) = mq.drain_dead_private_streams().await {
                    warn!(error = %e, "Draining dead workers' private streams failed");
                }
                tasks.reclaim(room).await
            } else {
                reader.read(room).await
            };
            match fetched {
                Ok(batch) => {
                    for delivery in batch {
                        held.insert(&delivery.entry);
                        buffer.push_back(delivery);
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Operation consume error; backing off");
                    drop(permit);
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                        _ = wait_for_shutdown(&shutdown) => break,
                    }
                    continue;
                }
            }
        }
        let Some(delivery) = buffer.pop_front() else {
            continue;
        };

        let tasks = tasks.clone();
        let held = held.clone();
        let handle = handle.clone();
        // Taken synchronously, before the spawn, so a shutdown right after
        // this never sees the counter at 0 with this task about to start.
        let in_flight_guard = in_flight.guard();
        tokio::spawn(async move {
            let _in_flight = in_flight_guard;
            let _permit = permit;
            let entry = delivery.entry.clone();
            let settled = match handle(delivery).await {
                Settlement::Ack => tasks.ack(&entry).await,
                Settlement::DeadLetter(envelope) => tasks.dead_letter(&entry, &envelope).await,
                Settlement::Abandon => {
                    warn!(entry = %entry.id, "Task abandoned; it is redelivered once idle");
                    Ok(Settled::Done)
                }
            };
            match settled {
                Ok(Settled::Done) => {}
                Ok(Settled::HandedOver) => warn!(
                    entry = %entry.id,
                    "Another worker took this task over while it ran; leaving it to that worker"
                ),
                Err(e) => {
                    error!(entry = %entry.id, error = %e, "Settling a task failed; it will be redelivered")
                }
            }
            held.remove(&entry);
        });
    }

    info!("Shutdown signaled; no longer consuming. Draining in-flight tasks");
    for delivery in buffer.drain(..) {
        match tasks.requeue(&delivery).await {
            Ok(Settled::Done | Settled::HandedOver) => {}
            Err(e) => {
                warn!(entry = %delivery.entry.id, error = %e, "Giving a task back failed; it is redelivered once idle")
            }
        }
        held.remove(&delivery.entry);
    }
    drain_in_flight(&in_flight, drain_timeout).await;
    refresher.abort();
}

/// Refresh every held entry each [`mq::REFRESH_INTERVAL`] until aborted.
async fn refresh_held(tasks: GroupConsumer<Task>, held: Held) {
    let mut ticker = tokio::time::interval(mq::REFRESH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let entries = held.snapshot();
        if entries.is_empty() {
            continue;
        }
        match tasks.refresh(&entries).await {
            Ok(lost) => {
                for entry in lost {
                    warn!(entry = %entry.id, "Another worker took over a task this worker still holds");
                    held.remove(&entry);
                }
            }
            Err(e) => warn!(error = %e, "Refreshing held tasks failed"),
        }
    }
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
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    fn task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            task_type: "operation".to_string(),
            executor_name: "operation".to_string(),
            payload: serde_json::json!({}),
            reply_to: "server-1".to_string(),
            operation_batch_id: None,
            trace_context: None,
        }
    }

    async fn redis_mq() -> (Mq, testcontainers::ContainerAsync<Redis>) {
        let container = Redis::default()
            .with_tag("7-alpine")
            .start()
            .await
            .expect("start redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port");
        let mq = Mq::connect(&format!("redis://127.0.0.1:{port}"), "ops")
            .await
            .expect("connect mq");
        (mq, container)
    }

    /// With shutdown already signaled the loop takes nothing, so queued tasks
    /// stay unread for other workers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_before_start_leaves_queued_tasks_unread() {
        let (mq, _container) = redis_mq().await;
        for i in 0..3 {
            mq.publish_task(&task(&format!("t{i}")), None)
                .await
                .expect("publish");
        }

        run_consume_loop(
            mq.clone(),
            mq.task_consumer("w"),
            4,
            InFlightCounter::new(),
            Arc::new(AtomicBool::new(true)),
            Duration::from_secs(1),
            |_d| async { Settlement::Ack },
        )
        .await;

        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!(
            (depths[0].unread, depths[0].pending),
            (3, 0),
            "a worker shutting down must leave every queued task unread"
        );
    }

    /// The loop runs every queued task and acknowledges it, then stops
    /// cleanly on shutdown.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn consumes_and_acks_queued_tasks_then_stops_on_shutdown() {
        let (mq, _container) = redis_mq().await;
        for i in 0..5 {
            mq.publish_task(&task(&format!("t{i}")), None)
                .await
                .expect("publish");
        }

        let processed = Arc::new(AtomicUsize::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = tokio::spawn({
            let processed = Arc::clone(&processed);
            let shutdown = Arc::clone(&shutdown);
            let mq = mq.clone();
            async move {
                run_consume_loop(
                    mq.clone(),
                    mq.task_consumer("w"),
                    2,
                    InFlightCounter::new(),
                    shutdown,
                    Duration::from_secs(5),
                    move |_d| {
                        let processed = Arc::clone(&processed);
                        async move {
                            processed.fetch_add(1, Ordering::Relaxed);
                            Settlement::Ack
                        }
                    },
                )
                .await;
            }
        });

        for _ in 0..100 {
            if processed.load(Ordering::Relaxed) >= 5 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(processed.load(Ordering::Relaxed), 5);

        shutdown.store(true, Ordering::Relaxed);
        join.await.expect("consume loop joins after shutdown");

        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!(
            (depths[0].unread, depths[0].pending),
            (0, 0),
            "acknowledged tasks are gone"
        );
    }

    /// An abandoned task stays pending under this worker, to be reclaimed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_abandoned_task_stays_pending() {
        let (mq, _container) = redis_mq().await;
        mq.publish_task(&task("t"), None).await.expect("publish");
        let shutdown = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let join = tokio::spawn({
            let shutdown = Arc::clone(&shutdown);
            let done = Arc::clone(&done);
            let mq = mq.clone();
            async move {
                run_consume_loop(
                    mq.clone(),
                    mq.task_consumer("w"),
                    1,
                    InFlightCounter::new(),
                    shutdown,
                    Duration::from_secs(5),
                    move |_d| {
                        let done = Arc::clone(&done);
                        async move {
                            done.fetch_add(1, Ordering::Relaxed);
                            Settlement::Abandon
                        }
                    },
                )
                .await;
            }
        });
        for _ in 0..100 {
            if done.load(Ordering::Relaxed) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        shutdown.store(true, Ordering::Relaxed);
        join.await.unwrap();

        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!((depths[0].unread, depths[0].pending), (0, 1));
    }
}
