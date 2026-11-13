//! Live worker capacity, and the judging fan-out bounded by it.
//!
//! [`WorkerSlots`] is the one record of the fleet's live capacity: the task
//! slots of the live workers (the sum of `max_concurrency` over their
//! heartbeats, see [`common::liveness`]) and how many servers share them,
//! refreshed by one task every [`SLOTS_REFRESH`] so no request or judging
//! path ever reads Redis for it. Each server takes its share of the fleet
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

use opentelemetry::KeyValue;
use std::sync::Mutex;
use tokio::sync::Notify;

/// How often the live worker capacity is re-read: a Redis `SCAN`, too costly
/// to repeat per claim pass or per test case.
pub const SLOTS_REFRESH: Duration = Duration::from_secs(15);

/// Test cases outstanding at workers per live worker slot.
pub const TEST_CASES_PER_WORKER_SLOT: u64 = 2;

#[derive(Default)]
struct SlotsInner {
    slots: AtomicU64,
    servers: AtomicU64,
    changed: Notify,
}

/// Task slots of the live workers and the number of live servers, as last
/// read from their heartbeats. No slots and one server until the first
/// read, and whenever Redis is unreachable.
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

    pub fn set(&self, slots: u64, servers: u64) {
        let slots_changed = self.0.slots.swap(slots, Ordering::Relaxed) != slots;
        let servers_changed = self.0.servers.swap(servers, Ordering::Relaxed) != servers;
        if slots_changed || servers_changed {
            self.0.changed.notify_waiters();
        }
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

    /// Re-read the live workers' slots from `client` every [`SLOTS_REFRESH`]
    /// for the life of the process.
    pub fn spawn_refresh(&self, client: Arc<redis::Client>, namespace: String) {
        let slots = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SLOTS_REFRESH);
            loop {
                interval.tick().await;
                let (workers, servers) =
                    crate::handlers::system::read_worker_payloads_and_server_count(
                        &client, &namespace,
                    )
                    .await;
                let total = workers
                    .iter()
                    .map(|(_, p)| u64::from(p.max_concurrency.unwrap_or(1)))
                    .sum();
                slots.set(total, servers as u64);
            }
        });
    }
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
        let cores = std::thread::available_parallelism()
            .map(|n| n.get() as u64)
            .unwrap_or(1);
        Self::with_floor(slots, cores, metrics)
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

    /// Outstanding test cases this server may have now: its share of
    /// [`TEST_CASES_PER_WORKER_SLOT`] per live worker slot. With no worker
    /// capacity known, one slot per local core, the shape of a single-host
    /// deployment.
    pub fn limit(&self) -> u64 {
        match self.0.slots.get() {
            0 => self.0.floor_slots * TEST_CASES_PER_WORKER_SLOT,
            slots => self
                .0
                .slots
                .share(slots * TEST_CASES_PER_WORKER_SLOT)
                .max(1),
        }
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

    #[test]
    fn the_limit_follows_live_worker_slots() {
        let slots = WorkerSlots::default();
        let fanout = Fanout::with_floor(slots.clone(), 4, None);
        assert_eq!(fanout.limit(), 4 * TEST_CASES_PER_WORKER_SLOT);
        slots.set(10, 1);
        assert_eq!(fanout.limit(), 10 * TEST_CASES_PER_WORKER_SLOT);
    }

    #[test]
    fn replicas_split_the_fleet_bound() {
        let slots = WorkerSlots::default();
        let fanout = Fanout::with_floor(slots.clone(), 4, None);
        slots.set(10, 3);
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
        slots.set(4, 1);
        let _permit = waiter.await.unwrap();
        assert_eq!(fanout.outstanding(), 3);
    }
}
