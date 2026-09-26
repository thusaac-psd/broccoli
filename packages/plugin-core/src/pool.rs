//! The plugin instance pool.
//!
//! ## Why this exists
//!
//! extism's own `Pool` reuses plugin instances and **never reclaims** them or
//! their WASM linear memory. wasmtime never shrinks a linear memory, and the
//! guest's allocator never returns freed pages to wasmtime, so every call that
//! pulls a large test case through an instance ratchets that instance's
//! high-water mark up a little (allocator fragmentation). Under concurrent
//! judging of large-test-case problems a hot instance can balloon to multiple
//! gigabytes; `pool_max_instances` such instances then exhaust host RAM and the
//! server is OOM-killed. (See the per-test-case RSS growth investigation.)
//! And because extism's pool can only grow, every instance a burst created
//! stayed resident for the life of the process, even once traffic stopped.
//!
//! ## What it does
//!
//! [`RecyclingPool`] hands out instances as [`PoolLease`]s, creating them on
//! demand up to `max_instances`, and tracks, per instance, the number of calls
//! served and the cumulative bytes marshalled/streamed through it. Two things
//! bound its memory:
//!
//! - **Recycling.** After a call, if an instance has **both** served at least
//!   `min_calls_before_recycle` calls **and** processed more than
//!   `reclaim_bytes` bytes, a fresh instance is built and swapped in, dropping
//!   the bloated one and returning its linear memory to the OS.
//! - **Idle eviction.** [`RecyclingPool::evict_idle`] drops instances that
//!   have sat unused for a while. Free instances are reused most recently used
//!   first, so under light traffic the same few stay warm and the rest of a
//!   burst's instances age out.
//!
//! ## The anti-thrash guarantee
//!
//! The `min_calls_before_recycle` floor makes "recreate on every call"
//! structurally impossible: a freshly recycled instance resets its call counter
//! to zero, so it must serve at least that many calls before it is *eligible*
//! to recycle again. Recreation is therefore bounded to at most one per
//! `min_calls_before_recycle` calls no matter how aggressively `reclaim_bytes`
//! is set. The byte budget further means data-light plugins (which never
//! approach it) are never recycled at all - only data-heavy instances are.
//! Idle eviction only ever touches instances nobody has used for the whole
//! idle timeout, so it cannot take an instance out from under steady traffic.

use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use extism::{Error, Plugin};
use tracing::warn;

/// A factory that builds a fresh plugin instance. Not `Send`/`Sync`: the
/// extism host functions it captures hold `Arc<Mutex<dyn Any>>` user data.
/// See the `unsafe impl`s on [`Shared`] for why sharing it is sound.
pub type PluginSource<T = Plugin> = Arc<dyn Fn() -> Result<T, Error>>;

#[derive(Default, Clone, Copy)]
struct InstanceUsage {
    calls_served: usize,
    cumulative_bytes: u64,
}

struct Slot<T> {
    instance: T,
    usage: InstanceUsage,
    /// When this instance was last returned to the pool.
    idle_since: Instant,
}

struct State<T> {
    /// Instances not currently leased, least recently used first.
    free: Vec<Slot<T>>,
    /// Every instance the pool owns: free, leased, or being built.
    live: usize,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    returned: Condvar,
    source: PluginSource<T>,
    max_instances: usize,
    /// Cumulative-bytes budget above which a recycle is allowed. `None`
    /// disables recycling.
    reclaim_bytes: Option<u64>,
    /// Minimum calls an instance must serve before becoming recycle-eligible.
    /// Guarantees recycling can never degrade into per-call recreation.
    min_calls_before_recycle: usize,
}

// SAFETY: mirrors extism's own `unsafe impl`s on its pool. `source` only
// reads its captured manifest and clones the captured host functions, whose
// shared state is behind `Arc`s (atomic reference counts), so concurrent
// invocations from several blocking threads are sound; extism's pool and the
// previous wrapper invoked it concurrently the same way. Instances themselves
// (`extism::Plugin` is `Send + Sync`) are only touched by the one caller that
// holds their lease, and `state` is behind a `Mutex`.
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

/// A pool that recycles instances once they have churned through more than a
/// configured amount of data, and drops instances that sit idle.
pub struct RecyclingPool<T = Plugin> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for RecyclingPool<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> RecyclingPool<T> {
    pub fn new(
        source: PluginSource<T>,
        max_instances: usize,
        reclaim_bytes: Option<u64>,
        min_calls_before_recycle: usize,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    free: Vec::new(),
                    live: 0,
                }),
                returned: Condvar::new(),
                source,
                max_instances: max_instances.max(1),
                reclaim_bytes,
                min_calls_before_recycle: min_calls_before_recycle.max(1),
            }),
        }
    }

    /// Lease an instance: a free one if there is one, otherwise a new one if
    /// the pool is below `max_instances`, otherwise the next one returned.
    /// `Ok(None)` if none became available within `timeout`.
    pub fn get(&self, timeout: Duration) -> Result<Option<PoolLease<T>>, Error> {
        let deadline = Instant::now() + timeout;
        let mut state = self.shared.state.lock().unwrap();
        loop {
            if let Some(slot) = state.free.pop() {
                return Ok(Some(self.lease(slot)));
            }
            if state.live < self.shared.max_instances {
                // Reserve the slot, then build outside the lock so a slow
                // build does not stall callers returning or taking instances.
                state.live += 1;
                drop(state);
                return match (self.shared.source)() {
                    Ok(instance) => Ok(Some(self.lease(Slot {
                        instance,
                        usage: InstanceUsage::default(),
                        idle_since: Instant::now(),
                    }))),
                    Err(e) => {
                        self.shared.state.lock().unwrap().live -= 1;
                        self.shared.returned.notify_one();
                        Err(e)
                    }
                };
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            state = self
                .shared
                .returned
                .wait_timeout(state, deadline - now)
                .unwrap()
                .0;
        }
    }

    fn lease(&self, slot: Slot<T>) -> PoolLease<T> {
        PoolLease {
            slot: Some(slot),
            shared: self.shared.clone(),
        }
    }

    /// Number of instances the pool currently owns (free or leased).
    pub fn count(&self) -> usize {
        self.shared.state.lock().unwrap().live
    }

    /// Drop every free instance that has been idle for at least `idle_for`,
    /// returning how many were dropped. Leased instances are never touched.
    pub fn evict_idle(&self, idle_for: Duration) -> usize {
        let evicted: Vec<Slot<T>> = {
            let mut state = self.shared.state.lock().unwrap();
            // `free` is ordered by return time, so the idle ones are a prefix.
            let stale = state
                .free
                .iter()
                .take_while(|slot| slot.idle_since.elapsed() >= idle_for)
                .count();
            state.live -= stale;
            state.free.drain(..stale).collect()
        };
        // Freed outside the lock: dropping an instance unmaps its memory.
        evicted.len()
    }
}

/// An instance leased from a [`RecyclingPool`]; returned to the pool on drop.
pub struct PoolLease<T = Plugin> {
    slot: Option<Slot<T>>,
    shared: Arc<Shared<T>>,
}

impl<T> PoolLease<T> {
    fn slot(&mut self) -> &mut Slot<T> {
        self.slot.as_mut().expect("slot is present until drop")
    }

    /// Record that `bytes` were processed during the just-completed call,
    /// then recycle the instance if it is now eligible. Returns `true` if the
    /// instance was recycled (its linear memory freed).
    pub fn note_call(&mut self, bytes: u64) -> bool {
        let reclaim_bytes = self.shared.reclaim_bytes;
        let min_calls = self.shared.min_calls_before_recycle;
        let usage = &mut self.slot().usage;
        usage.calls_served = usage.calls_served.saturating_add(1);
        usage.cumulative_bytes = usage.cumulative_bytes.saturating_add(bytes);
        let (calls, cumulative) = (usage.calls_served, usage.cumulative_bytes);

        let Some(budget) = reclaim_bytes else {
            return false;
        };
        // Both clauses required. The `calls < min` clause is the anti-thrash
        // guarantee: a just-recycled instance has 0 calls, so it can never be
        // recycled again on its very next call.
        if calls < min_calls || cumulative <= budget {
            return false;
        }

        // Build the replacement first (briefly coexists with the old
        // instance), then swap it in so the bloated one is dropped.
        let fresh = match (self.shared.source)() {
            Ok(instance) => instance,
            Err(e) => {
                // Keep the existing (bloated) instance rather than fail the
                // call path; we'll get another chance to recycle next call.
                warn!(error = %e, "Failed to build replacement plugin instance; skipping recycle");
                return false;
            }
        };
        let slot = self.slot();
        slot.instance = fresh;
        slot.usage = InstanceUsage::default();
        true
    }
}

impl<T> Deref for PoolLease<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self
            .slot
            .as_ref()
            .expect("slot is present until drop")
            .instance
    }
}

impl<T> DerefMut for PoolLease<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.slot().instance
    }
}

impl<T> Drop for PoolLease<T> {
    fn drop(&mut self) {
        if let Some(mut slot) = self.slot.take() {
            slot.idle_since = Instant::now();
            self.shared.state.lock().unwrap().free.push(slot);
            self.shared.returned.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A stand-in instance: just the build number that produced it.
    fn counting_pool(
        max: usize,
        reclaim: Option<u64>,
        min_calls: usize,
    ) -> (RecyclingPool<usize>, Arc<AtomicUsize>) {
        let built = Arc::new(AtomicUsize::new(0));
        let counter = built.clone();
        let source: PluginSource<usize> =
            Arc::new(move || Ok(counter.fetch_add(1, Ordering::SeqCst)));
        (RecyclingPool::new(source, max, reclaim, min_calls), built)
    }

    const NOW: Duration = Duration::ZERO;

    #[test]
    fn instances_are_built_on_demand_and_reused() {
        let (pool, built) = counting_pool(4, None, 1);
        let first = *pool.get(NOW).unwrap().unwrap();
        let again = *pool.get(NOW).unwrap().unwrap();
        assert_eq!(first, again, "a returned instance is reused");
        assert_eq!(built.load(Ordering::SeqCst), 1);
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn the_pool_never_exceeds_max_instances() {
        let (pool, _) = counting_pool(2, None, 1);
        let _a = pool.get(NOW).unwrap().unwrap();
        let _b = pool.get(NOW).unwrap().unwrap();
        assert!(pool.get(Duration::from_millis(20)).unwrap().is_none());
        assert_eq!(pool.count(), 2);
    }

    #[test]
    fn a_waiting_caller_gets_the_next_returned_instance() {
        let (pool, _) = counting_pool(1, None, 1);
        let held = pool.get(NOW).unwrap().unwrap();
        let waiter = {
            let pool = pool.clone();
            std::thread::spawn(move || pool.get(Duration::from_secs(5)).unwrap().map(|l| *l))
        };
        std::thread::sleep(Duration::from_millis(50));
        drop(held);
        assert_eq!(waiter.join().unwrap(), Some(0));
    }

    #[test]
    fn a_failed_build_releases_its_reserved_slot() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let source: PluginSource<usize> = Arc::new(move || {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(extism::Error::msg("boom"))
            } else {
                Ok(7)
            }
        });
        let pool = RecyclingPool::new(source, 1, None, 1);
        assert!(pool.get(NOW).is_err());
        assert_eq!(pool.count(), 0);
        assert_eq!(*pool.get(NOW).unwrap().unwrap(), 7);
    }

    #[test]
    fn a_data_heavy_instance_is_recycled_but_never_twice_in_a_row() {
        let (pool, built) = counting_pool(1, Some(100), 2);
        let mut lease = pool.get(NOW).unwrap().unwrap();
        assert!(!lease.note_call(500), "below the call floor");
        assert!(lease.note_call(500), "floor met and over budget");
        assert_eq!(*lease, 1, "a fresh instance was swapped in");
        assert!(!lease.note_call(500), "the fresh instance starts over");
        assert_eq!(built.load(Ordering::SeqCst), 2);
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn idle_instances_are_evicted_and_busy_or_recent_ones_kept() {
        let (pool, _) = counting_pool(4, None, 1);
        let a = pool.get(NOW).unwrap().unwrap();
        let b = pool.get(NOW).unwrap().unwrap();
        let c = pool.get(NOW).unwrap().unwrap();
        drop(a);
        drop(b);
        std::thread::sleep(Duration::from_millis(60));
        let recent = pool.get(NOW).unwrap().unwrap(); // takes b back
        drop(recent);

        // a has been idle ~60 ms, b was just returned, c is leased.
        assert_eq!(pool.evict_idle(Duration::from_millis(50)), 1);
        assert_eq!(pool.count(), 2);
        drop(c);
        assert_eq!(pool.evict_idle(Duration::ZERO), 2);
        assert_eq!(pool.count(), 0);
    }

    #[test]
    fn free_instances_are_reused_most_recent_first() {
        let (pool, _) = counting_pool(4, None, 1);
        let a = pool.get(NOW).unwrap().unwrap();
        let b = pool.get(NOW).unwrap().unwrap();
        let b_id = *b;
        drop(a);
        drop(b);
        assert_eq!(*pool.get(NOW).unwrap().unwrap(), b_id);
    }
}
