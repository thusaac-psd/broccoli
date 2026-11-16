//! Live worker capacity, and the judging fan-out bounded by it.
//!
//! [`WorkerSlots`] is the one record of the fleet's live capacity: the task
//! slots of the live workers that take work (the sum of `max_concurrency`
//! over their heartbeats, see [`common::liveness`]), how many live workers
//! take none ([`common::worker::HeartbeatPayload::accepts_work`]) and how
//! many servers share them. One task keeps it current
//! ([`WorkerSlots::follow`]) so no request or judging path ever reads Redis
//! for it: it re-reads the liveness keys whenever a worker announces a change
//! (it arrives, leaves, or starts or stops taking work, see
//! [`common::liveness::changes_channel`]), just after the soonest a live
//! worker's key can expire (a crashed worker announces nothing), and at least
//! every [`SLOTS_REFRESH`]. The snapshot therefore drops a worker when its
//! liveness key does. Each server takes its share of the fleet
//! figures ([`WorkerSlots::share`]): the claim fiber's in-flight cap and
//! [`Fanout`], how many test cases this server has outstanding at workers.
//!
//! A test case is outstanding from the moment its evaluate task starts until
//! its verdict is delivered. [`TEST_CASES_PER_WORKER_SLOT`] of them per live
//! slot, across the fleet, keeps every slot fed (one running, one queued
//! behind it) without spawning a task per test case of a whole burst at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::worker::HeartbeatPayload;

use crate::models::system::FleetIntake;
use futures::{FutureExt, StreamExt};
use opentelemetry::KeyValue;
use std::sync::Mutex;
use tokio::sync::Notify;

/// The longest the live worker capacity goes without a re-read: a Redis
/// `SCAN`, too costly to repeat per claim pass or per test case.
pub const SLOTS_REFRESH: Duration = Duration::from_secs(15);

/// How long after a live worker's key can first expire the fleet is re-read:
/// Redis has removed an expired key by the time a read reaches it.
const EXPIRY_MARGIN: Duration = Duration::from_millis(100);

/// Test cases outstanding at workers per live worker slot.
pub const TEST_CASES_PER_WORKER_SLOT: u64 = 2;

#[derive(Default)]
struct SlotsInner {
    slots: AtomicU64,
    /// Live workers that take no work.
    refusing: AtomicU64,
    servers: AtomicU64,
    changed: Notify,
}

/// Task slots of the live workers that take work, the number of live workers
/// that take none, and the number of live servers, as last read from their
/// heartbeats. No workers and one server until the first read, and whenever
/// Redis is unreachable.
#[derive(Clone, Default)]
pub struct WorkerSlots(Arc<SlotsInner>);

impl WorkerSlots {
    pub fn get(&self) -> u64 {
        self.0.slots.load(Ordering::Relaxed)
    }

    /// Live servers, at least this one.
    pub fn servers(&self) -> u64 {
        self.0.servers.load(Ordering::Relaxed).max(1)
    }

    /// Whether the live workers take work, as last read.
    pub fn intake(&self) -> FleetIntake {
        match (self.get(), self.0.refusing.load(Ordering::Relaxed)) {
            (0, 0) => FleetIntake::NoWorkers,
            (0, _) => FleetIntake::NoneAccepting,
            _ => FleetIntake::Accepting,
        }
    }

    /// Some live worker takes work: the one condition under which the claim
    /// takes queued work. With no live worker, or only workers that take
    /// none, whatever is claimed would only wait at the queue, out of sight,
    /// so the work stays `Queued`.
    pub fn takes_work(&self) -> bool {
        self.intake() == FleetIntake::Accepting
    }

    /// Live workers are known and none of them takes work: what this server
    /// waits for at the queue is never run, since every live worker refuses
    /// it (see `consumers::operation_result::hand_back_while_none_accepting`).
    /// An empty fleet is not this: the next worker to arrive reads the queue.
    pub fn none_accepting(&self) -> bool {
        self.intake() == FleetIntake::NoneAccepting
    }

    /// Record the fleet from the live workers' heartbeats and the number of
    /// live servers.
    pub fn observe<'a>(
        &self,
        workers: impl IntoIterator<Item = &'a HeartbeatPayload>,
        servers: u64,
    ) {
        let (mut slots, mut refusing) = (0, 0);
        for worker in workers {
            if worker.accepts_work() {
                slots += u64::from(worker.max_concurrency.unwrap_or(1));
            } else {
                refusing += 1;
            }
        }
        let changed = [
            (&self.0.slots, slots),
            (&self.0.refusing, refusing),
            (&self.0.servers, servers),
        ]
        .into_iter()
        .fold(false, |changed, (cell, now)| {
            cell.swap(now, Ordering::Relaxed) != now || changed
        });
        if changed {
            self.0.changed.notify_waiters();
        }
    }

    /// Read the live workers' heartbeats from `client` once. Returns how soon
    /// the next read is due, at the latest `every` from now (see
    /// [`next_read_in`]).
    pub async fn refresh(
        &self,
        client: &redis::Client,
        namespace: &str,
        every: Duration,
    ) -> Duration {
        let (workers, servers) =
            crate::handlers::system::read_worker_payloads_and_server_count(client, namespace).await;
        self.observe(workers.iter().map(|(_, p)| p), servers as u64);
        next_read_in(workers.iter().map(|(node, _)| node.age), every)
    }

    /// This server's share of a fleet-wide figure: an even split across the
    /// live servers, rounded up.
    pub fn share(&self, fleet: u64) -> u64 {
        fleet.div_ceil(self.servers())
    }

    /// Completes when the slots or the server count next change.
    pub fn changed(&self) -> tokio::sync::futures::Notified<'_> {
        self.0.changed.notified()
    }

    /// Keep these slots current from `client` for the life of the process
    /// (see [`Self::follow`]), re-reading at least every [`SLOTS_REFRESH`].
    pub fn spawn_refresh(&self, client: Arc<redis::Client>, namespace: String) {
        tokio::spawn(self.clone().follow(client, namespace, SLOTS_REFRESH));
    }

    /// Re-read the live workers whenever one announces a change, when a live
    /// worker's key can next expire, and at least every `every`. The
    /// subscription is taken before each read, so a change announced while it
    /// runs is read again; announcements that queue up meanwhile take one
    /// read. While the subscription is down the reads keep their schedule,
    /// and the next one subscribes again.
    pub async fn follow(self, client: Arc<redis::Client>, namespace: String, every: Duration) {
        let channel = common::liveness::changes_channel(&namespace);
        let mut changes = None;
        loop {
            if changes.is_none() {
                changes = subscribe(&client, &channel).await;
            }
            let due = tokio::time::sleep(self.refresh(&client, &namespace, every).await);
            let Some(stream) = changes.as_mut() else {
                due.await;
                continue;
            };
            tokio::select! {
                _ = due => {}
                announced = stream.next() => match announced {
                    Some(_) => while let Some(Some(_)) = stream.next().now_or_never() {},
                    None => changes = None,
                },
            }
        }
    }
}

/// A subscription to `channel`, or `None` while Redis does not answer.
async fn subscribe(client: &redis::Client, channel: &str) -> Option<redis::aio::PubSubStream> {
    let subscribed = async {
        let mut pubsub = client.get_async_pubsub().await?;
        pubsub.subscribe(channel).await?;
        Ok::<_, redis::RedisError>(pubsub.into_on_message())
    };
    match tokio::time::timeout(common::redis_conn::COMMAND_TIMEOUT, subscribed).await {
        Ok(Ok(stream)) => Some(stream),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Subscribing to worker changes failed; retrying on the next read");
            None
        }
        Err(_) => {
            tracing::warn!("Subscribing to worker changes timed out; retrying on the next read");
            None
        }
    }
}

/// How soon the fleet is next read, given the age of every live worker's
/// liveness key: just after the soonest one can expire unrenewed, so a
/// crashed worker leaves the snapshot as its key leaves Redis, and no later
/// than `every`.
pub fn next_read_in(ages: impl IntoIterator<Item = Duration>, every: Duration) -> Duration {
    ages.into_iter()
        .map(|age| common::liveness::TTL.saturating_sub(age) + EXPIRY_MARGIN)
        .fold(every, Duration::min)
}

/// Outstanding test cases a server may have: its share of
/// [`TEST_CASES_PER_WORKER_SLOT`] per live worker slot. With no worker
/// capacity known, `floor_slots` slots, one per local core in production:
/// the shape of a single-host deployment.
pub fn fan_out_limit(slots: &WorkerSlots, floor_slots: u64) -> u64 {
    match slots.get() {
        0 => floor_slots * TEST_CASES_PER_WORKER_SLOT,
        live => slots.share(live * TEST_CASES_PER_WORKER_SLOT).max(1),
    }
}

/// Slots assumed while no worker capacity is known: one per local core.
pub fn local_floor_slots() -> u64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(1)
}

/// Keep the executor's judging and work caps at this server's in-flight cap
/// and fan-out bound as the fleet changes (see
/// [`plugin_core::admission::Admission::set_background_caps`]).
pub fn spawn_admission_sizing(slots: WorkerSlots, admission: plugin_core::admission::Admission) {
    let size = move |slots: &WorkerSlots| {
        let in_flight = crate::dispatcher::claim::in_flight_cap(slots) as usize;
        let fan_out = fan_out_limit(slots, local_floor_slots()) as usize;
        admission.set_background_caps(in_flight, fan_out);
    };
    size(&slots);
    tokio::spawn(async move {
        loop {
            slots.changed().await;
            size(&slots);
        }
    });
}

struct FanoutInner {
    outstanding: Mutex<u64>,
    released: Notify,
    slots: WorkerSlots,
    /// Slots assumed while no worker capacity is known.
    floor_slots: u64,
    metrics: Option<common::metrics::Metrics>,
}

/// The server-wide bound on outstanding test cases. Cheap to clone.
#[derive(Clone)]
pub struct Fanout(Arc<FanoutInner>);

/// One outstanding test case; dropping it lets the next one start.
pub struct FanoutPermit(Arc<FanoutInner>);

impl Fanout {
    pub fn new(slots: WorkerSlots, metrics: Option<common::metrics::Metrics>) -> Self {
        Self::with_floor(slots, local_floor_slots(), metrics)
    }

    fn with_floor(
        slots: WorkerSlots,
        floor_slots: u64,
        metrics: Option<common::metrics::Metrics>,
    ) -> Self {
        Self(Arc::new(FanoutInner {
            outstanding: Mutex::new(0),
            released: Notify::new(),
            slots,
            floor_slots: floor_slots.max(1),
            metrics,
        }))
    }

    /// Outstanding test cases this server may have now; see
    /// [`fan_out_limit`].
    pub fn limit(&self) -> u64 {
        fan_out_limit(&self.0.slots, self.0.floor_slots)
    }

    /// Wait until one more test case may be outstanding.
    pub async fn acquire(&self) -> FanoutPermit {
        let start = Instant::now();
        let mut waited = false;
        loop {
            let released = self.0.released.notified();
            let resized = self.0.slots.changed();
            tokio::pin!(released, resized);
            released.as_mut().enable();
            resized.as_mut().enable();
            {
                let mut outstanding = lock(&self.0.outstanding);
                if *outstanding < self.limit() {
                    *outstanding += 1;
                    break;
                }
            }
            waited = true;
            tokio::select! {
                _ = released => {}
                _ = resized => {}
            }
        }
        if let Some(metrics) = self.0.metrics.as_ref() {
            let labels = [KeyValue::new("batch.kind", "evaluate")];
            metrics
                .evaluate_batch_fanout_wait_duration
                .record(start.elapsed().as_secs_f64() * 1_000.0, &labels);
            if waited {
                metrics
                    .evaluate_batch_fanout_saturated_total
                    .add(1, &labels);
            }
        }
        FanoutPermit(self.0.clone())
    }

    pub fn outstanding(&self) -> u64 {
        *lock(&self.0.outstanding)
    }
}

fn lock(outstanding: &Mutex<u64>) -> std::sync::MutexGuard<'_, u64> {
    outstanding
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Drop for FanoutPermit {
    fn drop(&mut self) {
        *lock(&self.0.outstanding) -= 1;
        self.0.released.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queue an acquire on its own task and wait until it has parked.
    async fn parked(fanout: &Fanout) -> tokio::task::JoinHandle<FanoutPermit> {
        let task = tokio::spawn({
            let fanout = fanout.clone();
            async move { fanout.acquire().await }
        });
        // An acquire that cannot proceed leaves `outstanding` unchanged and
        // the task pending; yielding lets it reach its wait.
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        task
    }

    fn heartbeat(max_concurrency: u32, missing_platform_tools: &[&str]) -> HeartbeatPayload {
        HeartbeatPayload {
            id: "w".into(),
            started_at: chrono::DateTime::UNIX_EPOCH,
            last_seen: chrono::DateTime::UNIX_EPOCH,
            in_flight: 0,
            max_concurrency: Some(max_concurrency),
            fairness_mode: "unknown".into(),
            sandbox_backend: "mock".into(),
            version: "test".into(),
            hostname: None,
            ip_addresses: Vec::new(),
            os: None,
            arch: None,
            cpu_count: None,
            pid: None,
            intake: common::worker::Intake::from_missing(
                missing_platform_tools
                    .iter()
                    .map(|t| t.to_string())
                    .collect(),
            ),
        }
    }

    #[test]
    fn only_workers_that_take_work_are_capacity() {
        let slots = WorkerSlots::default();
        assert!(!slots.none_accepting(), "no worker known yet");
        assert!(!slots.takes_work(), "no worker known yet");
        slots.observe(&[heartbeat(4, &["broccoli-compare"])], 1);
        assert_eq!(slots.get(), 0);
        assert!(slots.none_accepting());
        slots.observe(&[heartbeat(4, &["broccoli-compare"]), heartbeat(2, &[])], 1);
        assert_eq!(slots.get(), 2);
        assert!(!slots.none_accepting());
        assert!(slots.takes_work());
        slots.observe(&[], 1);
        assert!(
            !slots.none_accepting(),
            "no live worker is not a refusing one"
        );
        assert!(!slots.takes_work(), "and takes no work either");
    }

    #[test]
    fn the_limit_follows_live_worker_slots() {
        let slots = WorkerSlots::default();
        let fanout = Fanout::with_floor(slots.clone(), 4, None);
        assert_eq!(fanout.limit(), 4 * TEST_CASES_PER_WORKER_SLOT);
        slots.observe(&[heartbeat(10, &[])], 1);
        assert_eq!(fanout.limit(), 10 * TEST_CASES_PER_WORKER_SLOT);
    }

    #[test]
    fn replicas_split_the_fleet_bound() {
        let slots = WorkerSlots::default();
        let fanout = Fanout::with_floor(slots.clone(), 4, None);
        slots.observe(&[heartbeat(10, &[])], 3);
        // 20 outstanding across the fleet, 7 per server rounded up.
        assert_eq!(fanout.limit(), 7);
        assert_eq!(slots.share(20), 7);
    }

    #[tokio::test]
    async fn an_acquire_waits_for_a_release_at_the_limit() {
        let fanout = Fanout::with_floor(WorkerSlots::default(), 1, None);
        let held: Vec<_> = futures::future::join_all((0..2).map(|_| fanout.acquire())).await;
        let waiter = parked(&fanout).await;
        assert!(!waiter.is_finished());
        drop(held);
        let _permit = waiter.await.unwrap();
        assert_eq!(fanout.outstanding(), 1);
    }

    #[tokio::test]
    async fn more_live_slots_let_waiters_through() {
        let slots = WorkerSlots::default();
        let fanout = Fanout::with_floor(slots.clone(), 1, None);
        let _held: Vec<_> = futures::future::join_all((0..2).map(|_| fanout.acquire())).await;
        let waiter = parked(&fanout).await;
        assert!(!waiter.is_finished());
        slots.observe(&[heartbeat(4, &[])], 1);
        let _permit = waiter.await.unwrap();
        assert_eq!(fanout.outstanding(), 3);
    }

    #[test]
    fn the_next_read_follows_the_soonest_key_expiry() {
        let every = Duration::from_secs(3600);
        assert_eq!(next_read_in([], every), every, "no worker: the fallback");
        let ttl = common::liveness::TTL;
        assert_eq!(
            next_read_in([Duration::from_secs(1), Duration::from_secs(4)], every),
            ttl - Duration::from_secs(4) + EXPIRY_MARGIN,
            "just after the oldest renewal runs out"
        );
        assert_eq!(
            next_read_in([ttl + Duration::from_secs(1)], every),
            EXPIRY_MARGIN,
            "a key past its TTL is read again at once"
        );
        assert_eq!(
            next_read_in([Duration::ZERO], Duration::from_secs(1)),
            Duration::from_secs(1),
            "never later than the fallback"
        );
    }

    #[tokio::test]
    async fn a_fleet_that_stops_taking_work_is_a_change() {
        let slots = WorkerSlots::default();
        slots.observe(&[heartbeat(0, &[])], 1);
        let changed = slots.changed();
        tokio::pin!(changed);
        changed.as_mut().enable();
        // The same slots (none) and servers, but the worker now refuses work.
        slots.observe(&[heartbeat(0, &["broccoli-compare"])], 1);
        assert!(slots.none_accepting());
        changed.await;
    }
}
