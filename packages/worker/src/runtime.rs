use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use common::retry::{RetryTracker, spawn_cleanup_task};
use common::worker::{Executor, Task};
use futures::FutureExt;
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
use crate::models::operation::OperationTaskExecutor;
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

        let mq = Mq::connect(&config.mq.url, &config.mq.operation_queue_name)
            .await
            .context("Failed to initialize MQ")?;

        info!(
            operation_queue_name = %config.mq.operation_queue_name,
            max_retries = config.mq.dlq.max_retries,
            "MQ connected"
        );
        mq.task_consumer(&config.worker.id)
            .ensure_groups()
            .await
            .context("Creating this worker's consumer groups")?;

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
        // The worker stamps wall time that servers compare with theirs
        // (heartbeats, retry history, warm progress), so it keeps the same
        // database clock they do.
        let clock = common::clock::PlatformClock::connect(&config.database.url)
            .await
            .context("Failed to read the database clock")?;
        let in_flight = InFlightCounter::new();
        let started_at = clock.now();
        let heartbeat_config = HeartbeatConfig {
            worker_id: config.worker.id.clone(),
            sandbox_backend: config.worker.sandbox_backend.clone(),
            max_concurrency: Some(capacity.max_concurrency as u32),
            fairness_mode: capacity.fairness_mode.as_str().into(),
            system_info,
            clock: clock.clone(),
        };
        let claim = NodeClaim::claim_at_boot(
            redis::Client::open(config.mq.url.as_str()).context("Invalid MQ URL")?,
            &config.mq.operation_queue_name,
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

        let operation = OperationTaskExecutor::from_app_config(&config, metrics.clone())
            .await
            .context("Failed to initialize the operation executor")?;

        // Pre-warm subscriber: listens for contest cache-warm jobs and fills the
        // local file cache ahead of the opening burst. Independent of judging,
        // but warms through the executor's own cacher.
        let warm_handle = crate::warm::spawn(&config, operation.file_cacher(), clock.clone());

        let operation: Arc<dyn Executor> = Arc::new(operation);
        let worker = Arc::new(Worker::new([("operation".to_string(), operation)]));

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
            clock,
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
            common::shutdown::signal().await;
            info!("Shutdown signal received; refusing new tasks and beginning drain");
            shutdown_for_signal.store(true, Ordering::SeqCst);
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

    /// Stop holding `entry` when the guard drops, however its task ends.
    fn release_on_drop(&self, entry: EntryRef) -> HeldGuard {
        HeldGuard {
            held: self.clone(),
            entry,
        }
    }
}

struct HeldGuard {
    held: Held,
    entry: EntryRef,
}

impl Drop for HeldGuard {
    fn drop(&mut self) {
        self.held.remove(&self.entry);
    }
}

/// The worker's consume loop, extracted from [`WorkerRuntime::run`] so it can
/// be driven directly in tests.
///
/// It starts by taking back the entries it held before a restart, and does
/// so again after a failed read, whose reply may have delivered entries that
/// never arrived. It reads from the worker's private and shared streams with one blocking
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
    let mut adopt = true;

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
            let fetched = if adopt {
                // Entries this worker owns but does not track: held before
                // a restart, or read by a request whose reply was lost.
                adopt = false;
                tasks.adopt_own(room, &held.snapshot()).await
            } else if Instant::now() >= next_reclaim {
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
                    adopt = true;
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
            let _held = held.release_on_drop(entry.clone());
            // A panicking handler abandons the task: it is redelivered once
            // idle, and dead-lettered once it has failed MAX_DELIVERIES times.
            let settlement = std::panic::AssertUnwindSafe(handle(delivery))
                .catch_unwind()
                .await
                .unwrap_or_else(|_| {
                    error!(entry = %entry.id, "Task handler panicked; abandoning the task");
                    Settlement::Abandon
                });
            settle(&tasks, &entry, settlement).await;
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

/// Settle a finished task. A settle that fails (Redis unreachable) is retried
/// with backoff while the entry stays held, and so refreshed: letting it go
/// would have another worker run a task that already finished.
async fn settle(tasks: &GroupConsumer<Task>, entry: &EntryRef, settlement: Settlement) {
    let mut attempt: u8 = 0;
    loop {
        let settled = match &settlement {
            Settlement::Ack => tasks.ack(entry).await,
            Settlement::DeadLetter(envelope) => tasks.dead_letter(entry, envelope).await,
            Settlement::Abandon => {
                warn!(entry = %entry.id, "Task abandoned; it is redelivered once idle");
                return;
            }
        };
        match settled {
            Ok(Settled::Done) => return,
            Ok(Settled::HandedOver) => {
                warn!(
                    entry = %entry.id,
                    "Another worker took this task over while it ran; leaving it to that worker"
                );
                return;
            }
            Err(e) => {
                attempt = attempt.saturating_add(1);
                let delay = common::retry::calculate_backoff(attempt, 250, 5_000);
                warn!(entry = %entry.id, attempt, error = %e, "Settling a task failed; retrying");
                tokio::time::sleep(delay).await;
            }
        }
    }
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
        let container = common::test_util::redis_image()
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
        let all_done = Arc::new(tokio::sync::Notify::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = tokio::spawn({
            let processed = Arc::clone(&processed);
            let all_done = Arc::clone(&all_done);
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
                        let all_done = Arc::clone(&all_done);
                        async move {
                            if processed.fetch_add(1, Ordering::SeqCst) + 1 == 5 {
                                all_done.notify_one();
                            }
                            Settlement::Ack
                        }
                    },
                )
                .await;
            }
        });

        all_done.notified().await;
        assert_eq!(processed.load(Ordering::SeqCst), 5);

        shutdown.store(true, Ordering::Relaxed);
        join.await.expect("consume loop joins after shutdown");

        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!(
            (depths[0].unread, depths[0].pending),
            (0, 0),
            "acknowledged tasks are gone"
        );
    }

    /// A worker that restarts takes back what it held before at once,
    /// without waiting out the claim threshold.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_restarted_worker_takes_back_its_own_pending_tasks() {
        let (mq, _container) = redis_mq().await;
        let before_crash = mq.task_consumer("w");
        before_crash.ensure_groups().await.unwrap();
        mq.publish_task(&task("held-at-crash"), None).await.unwrap();
        assert_eq!(before_crash.reader().read(1).await.unwrap().len(), 1);

        let (seen_tx, seen_rx) = tokio::sync::oneshot::channel::<u32>();
        let seen_tx = Arc::new(StdMutex::new(Some(seen_tx)));
        let shutdown = Arc::new(AtomicBool::new(false));
        let join = tokio::spawn({
            let shutdown = Arc::clone(&shutdown);
            let mq = mq.clone();
            async move {
                run_consume_loop(
                    mq.clone(),
                    mq.task_consumer("w"),
                    1,
                    InFlightCounter::new(),
                    shutdown,
                    Duration::from_secs(5),
                    move |d| {
                        let seen_tx = Arc::clone(&seen_tx);
                        async move {
                            if let Some(tx) = seen_tx.lock().unwrap().take() {
                                let _ = tx.send(d.deliveries);
                            }
                            Settlement::Ack
                        }
                    },
                )
                .await;
            }
        });
        assert_eq!(seen_rx.await.unwrap(), 2, "taken back as a second delivery");
        shutdown.store(true, Ordering::Relaxed);
        join.await.unwrap();
        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!((depths[0].unread, depths[0].pending), (0, 0));
    }

    /// An abandoned task stays pending under this worker, to be reclaimed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_abandoned_task_stays_pending() {
        let (mq, _container) = redis_mq().await;
        mq.publish_task(&task("t"), None).await.expect("publish");
        let shutdown = Arc::new(AtomicBool::new(false));
        let done = Arc::new(tokio::sync::Notify::new());
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
                            done.notify_one();
                            Settlement::Abandon
                        }
                    },
                )
                .await;
            }
        });
        done.notified().await;
        shutdown.store(true, Ordering::Relaxed);
        join.await.unwrap();

        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!((depths[0].unread, depths[0].pending), (0, 1));
    }

    /// A handler that panics abandons its task: the worker stops holding it,
    /// so it becomes reclaimable instead of being refreshed forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_task_is_released_for_redelivery() {
        let (mq, _container) = redis_mq().await;
        mq.publish_task(&task("boom"), None).await.expect("publish");
        let shutdown = Arc::new(AtomicBool::new(false));
        let (ran_tx, ran_rx) = tokio::sync::oneshot::channel::<()>();
        let ran_tx = Arc::new(StdMutex::new(Some(ran_tx)));
        let in_flight = InFlightCounter::new();
        let join = tokio::spawn({
            let shutdown = Arc::clone(&shutdown);
            let mq = mq.clone();
            let in_flight = in_flight.clone();
            async move {
                run_consume_loop(
                    mq.clone(),
                    mq.task_consumer("w"),
                    1,
                    in_flight,
                    shutdown,
                    Duration::from_secs(5),
                    move |_d| {
                        let ran_tx = Arc::clone(&ran_tx);
                        async move {
                            if let Some(tx) = ran_tx.lock().unwrap().take() {
                                let _ = tx.send(());
                            }
                            panic!("handler bug");
                        }
                    },
                )
                .await;
            }
        });
        ran_rx.await.expect("the handler ran");
        shutdown.store(true, Ordering::Relaxed);
        join.await.expect("the loop survives a panicking handler");
        assert_eq!(
            in_flight.current(),
            0,
            "the panicked task is no longer in flight"
        );

        // Nothing holds it any more: aged past the threshold, it is reclaimable.
        let mut conn = redis::Client::open(mq_url(&_container).await.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let pending: Vec<(String, String, u64, u64)> = redis::cmd("XPENDING")
            .arg(mq.topology().shared())
            .arg(mq::WORKERS_GROUP)
            .arg("-")
            .arg("+")
            .arg(10)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(pending.len(), 1, "the task stays pending for redelivery");
        common::test_util::age_entry(
            &mut conn,
            &mq.topology().shared(),
            mq::WORKERS_GROUP,
            "w",
            &pending[0].0,
            mq::CLAIM_IDLE * 2,
        )
        .await;
        let reclaimed = mq.task_consumer("w2").reclaim(1).await.unwrap();
        assert_eq!(reclaimed.len(), 1);
    }

    #[tokio::test]
    async fn a_held_entry_is_released_when_its_task_panics() {
        let held = Held::default();
        let entry = EntryRef {
            stream: Arc::from("ops"),
            id: "1-0".into(),
        };
        held.insert(&entry);
        let task = tokio::spawn({
            let guard = held.release_on_drop(entry.clone());
            async move {
                let _guard = guard;
                panic!("handler bug");
            }
        });
        assert!(task.await.unwrap_err().is_panic());
        assert!(held.snapshot().is_empty());
    }

    /// A settle that fails while Redis is down is retried, not given up:
    /// the finished task is acknowledged once Redis is back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_settle_is_retried_until_it_lands() {
        let (container, url) = common::test_util::restartable_redis().await;
        let mq = Mq::connect(&url, "ops").await.unwrap();
        let tasks = mq.task_consumer("w");
        tasks.ensure_groups().await.unwrap();
        mq.publish_task(&task("done"), None).await.unwrap();
        let delivery = tasks.reader().read(1).await.unwrap().remove(0);

        container.stop().await.expect("stop redis");
        let settling = tokio::spawn({
            let tasks = tasks.clone();
            let entry = delivery.entry.clone();
            async move { settle(&tasks, &entry, Settlement::Ack).await }
        });
        container.start().await.expect("restart redis");
        settling.await.expect("the settle lands once Redis is back");

        let depths = mq.depths(&[mq.topology().shared()]).await.unwrap();
        assert_eq!((depths[0].unread, depths[0].pending), (0, 0));
    }

    async fn mq_url(container: &testcontainers::ContainerAsync<Redis>) -> String {
        let port = container.get_host_port_ipv4(6379).await.unwrap();
        format!("redis://127.0.0.1:{port}")
    }
}
