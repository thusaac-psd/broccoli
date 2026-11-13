//! A plugin's instance pool.
//!
//! Instances are built on demand: a call never waits for one, because
//! admission ([`crate::admission`]) already bounds how many calls run. A
//! returned instance is kept for the next call, most recently used first.
//! The pool never shrinks on its own; the process-wide [`MemoryBudget`]
//! drops idle instances, least recently used first across every plugin, when
//! the resident set is over budget.
//!
//! One lease is one instance, so an instance is never re-entered: a nested
//! call into the same plugin leases a second instance with its own store.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use extism::{Error, Plugin};
use opentelemetry::KeyValue;
use parking_lot::Mutex;

use crate::memory::{MemoryBudget, Trimmable, WARM_PER_PLUGIN};

/// A factory that builds a fresh plugin instance. Not `Send`/`Sync`: the
/// extism host functions it captures hold `Arc<Mutex<dyn Any>>` user data.
/// See the `unsafe impl`s on `Shared` for why sharing it is sound.
pub type PluginSource<T = Plugin> = Arc<dyn Fn() -> Result<T, Error>>;

struct Slot<T> {
    instance: T,
    /// The budget's clock when this instance was last returned.
    idle_since: u64,
}

struct State<T> {
    /// Instances not currently leased, least recently used first.
    free: Vec<Slot<T>>,
    /// Instances leased out.
    leased: usize,
}

struct Shared<T> {
    plugin_id: String,
    state: Mutex<State<T>>,
    source: PluginSource<T>,
    budget: Arc<MemoryBudget>,
    metrics: Option<common::metrics::Metrics>,
}

// SAFETY: mirrors extism's own `unsafe impl`s on its pool. `source` only
// reads its captured manifest and clones the captured host functions, whose
// shared state is behind `Arc`s (atomic reference counts), so concurrent
// invocations from several blocking threads are sound. Instances themselves
// (`extism::Plugin` is `Send + Sync`) are only touched by the one caller that
// holds their lease, and `state` is behind a `Mutex`.
unsafe impl<T: Send> Send for Shared<T> {}
unsafe impl<T: Send> Sync for Shared<T> {}

impl<T> Shared<T> {
    fn live_changed(&self, delta: i64) {
        if let Some(metrics) = &self.metrics {
            metrics
                .plugin_instances_live
                .add(delta, &[KeyValue::new("plugin.id", self.plugin_id.clone())]);
        }
    }
}

/// A plugin's pool of instances. Cheap to clone; clones share one pool.
pub struct InstancePool<T = Plugin> {
    shared: Arc<Shared<T>>,
}

impl<T> Clone for InstancePool<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T: Send + 'static> InstancePool<T> {
    pub fn new(
        plugin_id: &str,
        source: PluginSource<T>,
        budget: Arc<MemoryBudget>,
        metrics: Option<common::metrics::Metrics>,
    ) -> Self {
        let shared = Arc::new(Shared {
            plugin_id: plugin_id.to_string(),
            state: Mutex::new(State {
                free: Vec::new(),
                leased: 0,
            }),
            source,
            budget: budget.clone(),
            metrics,
        });
        let trimmable: Arc<dyn Trimmable> = shared.clone();
        budget.register(Arc::downgrade(&trimmable));
        Self { shared }
    }

    /// Lease an instance: the most recently returned one, or a new one.
    pub fn get(&self) -> Result<PoolLease<T>, Error> {
        let reused = {
            let mut state = self.shared.state.lock();
            let slot = state.free.pop();
            state.leased += 1;
            slot
        };
        let instance = match reused {
            Some(slot) => slot.instance,
            // Built outside the lock so a slow build does not stall callers
            // returning or taking instances.
            None => match (self.shared.source)() {
                Ok(instance) => {
                    self.shared.live_changed(1);
                    instance
                }
                Err(e) => {
                    self.shared.state.lock().leased -= 1;
                    return Err(e);
                }
            },
        };
        Ok(PoolLease {
            instance: Some(instance),
            shared: self.shared.clone(),
        })
    }

    /// Instances the pool owns, leased or idle.
    pub fn count(&self) -> usize {
        let state = self.shared.state.lock();
        state.free.len() + state.leased
    }

    /// Idle instances.
    pub fn idle(&self) -> usize {
        self.shared.state.lock().free.len()
    }
}

impl<T: Send> Trimmable for Shared<T> {
    fn oldest_spare(&self) -> Option<u64> {
        let state = self.state.lock();
        (state.free.len() > WARM_PER_PLUGIN).then(|| state.free[0].idle_since)
    }

    fn drop_oldest_spare(&self) -> bool {
        let evicted = {
            let mut state = self.state.lock();
            if state.free.len() <= WARM_PER_PLUGIN {
                return false;
            }
            state.free.remove(0)
        };
        // Freed outside the lock: dropping an instance unmaps its memory.
        drop(evicted);
        self.live_changed(-1);
        if let Some(metrics) = &self.metrics {
            metrics
                .plugin_instance_evicted_total
                .add(1, &[KeyValue::new("plugin.id", self.plugin_id.clone())]);
        }
        true
    }
}

/// An instance leased from an [`InstancePool`]; returned to the pool on drop.
pub struct PoolLease<T = Plugin> {
    instance: Option<T>,
    shared: Arc<Shared<T>>,
}

impl<T> Deref for PoolLease<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.instance.as_ref().expect("present until drop")
    }
}

impl<T> DerefMut for PoolLease<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.instance.as_mut().expect("present until drop")
    }
}

impl<T> Drop for PoolLease<T> {
    fn drop(&mut self) {
        let Some(instance) = self.instance.take() else {
            return;
        };
        let idle_since = self.shared.budget.stamp();
        {
            let mut state = self.shared.state.lock();
            state.leased -= 1;
            state.free.push(Slot {
                instance,
                idle_since,
            });
        }
        self.shared.budget.after_return();
    }
}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let live = self.state.get_mut().free.len();
        self.live_changed(-(live as i64));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    /// A pool of stand-in instances (their build number) under a budget whose
    /// resident set is `rss`.
    fn counting_pool(
        budget: &Arc<MemoryBudget>,
        id: &str,
    ) -> (InstancePool<usize>, Arc<AtomicUsize>) {
        let built = Arc::new(AtomicUsize::new(0));
        let counter = built.clone();
        let source: PluginSource<usize> =
            Arc::new(move || Ok(counter.fetch_add(1, Ordering::SeqCst)));
        (InstancePool::new(id, source, budget.clone(), None), built)
    }

    fn budget(limit: u64, rss: &Arc<AtomicU64>) -> Arc<MemoryBudget> {
        let rss = rss.clone();
        MemoryBudget::new(limit, Box::new(move || rss.load(Ordering::SeqCst)))
    }

    #[test]
    fn instances_are_built_on_demand_and_reused() {
        let b = budget(u64::MAX, &Arc::new(AtomicU64::new(0)));
        let (pool, built) = counting_pool(&b, "p");
        let first = *pool.get().unwrap();
        let again = *pool.get().unwrap();
        assert_eq!(first, again, "a returned instance is reused");
        assert_eq!(built.load(Ordering::SeqCst), 1);
        assert_eq!(pool.count(), 1);
    }

    #[test]
    fn concurrent_leases_never_share_an_instance() {
        let b = budget(u64::MAX, &Arc::new(AtomicU64::new(0)));
        let (pool, _) = counting_pool(&b, "p");
        let a = pool.get().unwrap();
        let c = pool.get().unwrap();
        assert_ne!(*a, *c);
        assert_eq!(pool.count(), 2);
    }

    #[test]
    fn a_failed_build_leaves_nothing_leased() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let source: PluginSource<usize> = Arc::new(move || {
            if c.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(extism::Error::msg("boom"))
            } else {
                Ok(7)
            }
        });
        let b = budget(u64::MAX, &Arc::new(AtomicU64::new(0)));
        let pool = InstancePool::new("p", source, b, None);
        assert!(pool.get().is_err());
        assert_eq!(pool.count(), 0);
        assert_eq!(*pool.get().unwrap(), 7);
    }

    #[test]
    fn free_instances_are_reused_most_recent_first() {
        let b = budget(u64::MAX, &Arc::new(AtomicU64::new(0)));
        let (pool, _) = counting_pool(&b, "p");
        let a = pool.get().unwrap();
        let c = pool.get().unwrap();
        let c_id = *c;
        drop(a);
        drop(c);
        assert_eq!(*pool.get().unwrap(), c_id);
    }

    #[test]
    fn over_budget_the_least_recently_used_idle_instances_go_across_plugins() {
        let rss = Arc::new(AtomicU64::new(0));
        let b = budget(100, &rss);
        let (icpc, _) = counting_pool(&b, "icpc");
        let (checker, _) = counting_pool(&b, "checker");
        let leases: Vec<_> = (0..3)
            .flat_map(|_| [icpc.get().unwrap(), checker.get().unwrap()])
            .collect();
        // Returned interleaved, oldest first: icpc, checker, icpc, ...
        drop(leases);
        rss.store(1_000, Ordering::SeqCst);
        // Over budget forever: everything but one warm instance per plugin.
        assert_eq!(b.trim(), 4);
        assert_eq!((icpc.idle(), checker.idle()), (1, 1));
    }

    #[test]
    fn leased_instances_are_never_trimmed() {
        let rss = Arc::new(AtomicU64::new(1_000));
        let b = budget(100, &rss);
        let (pool, _) = counting_pool(&b, "p");
        let _held: Vec<_> = (0..3).map(|_| pool.get().unwrap()).collect();
        assert_eq!(b.trim(), 0);
        assert_eq!(pool.count(), 3);
    }

    #[test]
    fn a_pool_replaced_by_a_reload_is_trimmed_until_its_last_lease_returns() {
        let rss = Arc::new(AtomicU64::new(0));
        let b = budget(100, &rss);
        let (old, _) = counting_pool(&b, "p");
        let leases: Vec<_> = (0..3).map(|_| old.get().unwrap()).collect();
        // The registry drops the old pool; its leases keep it alive.
        drop(old);
        let mut leases = leases.into_iter();
        let last = leases.next().unwrap();
        drop(leases);
        rss.store(1_000, Ordering::SeqCst);
        assert_eq!(b.trim(), 1, "the old pool's spare idle instance goes");
        drop(last);
        assert_eq!(b.trim(), 0, "the old pool is gone with its last lease");
    }
}
