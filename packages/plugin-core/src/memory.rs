//! The memory budget for plugin instances.
//!
//! Plugin pools build instances on demand and keep them for reuse. Memory is
//! bounded globally rather than per pool: when the process's resident set
//! (`/proc/self/statm`) is above the budget, idle instances are dropped least
//! recently used first across every plugin, keeping a small warm set per
//! plugin. Dropping an instance unmaps its linear memory, which wasmtime never
//! shrinks on its own.
//!
//! The budget is a fraction of the memory this process may use, read from
//! local files only: the cgroup v2 limit of the process's cgroup or the
//! nearest parent that sets one, else the cgroup v1 hierarchical limit, else
//! the machine's `MemTotal`. The last case is a shared box (the single-host
//! compose runs the database, the queue and a worker beside the server), so
//! it takes a smaller fraction.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Instant;

use parking_lot::Mutex;

/// Idle instances each plugin keeps however tight memory is, so a quiet
/// plugin answers its next call without building an instance.
pub const WARM_PER_PLUGIN: usize = 1;

/// Share of a cgroup memory limit the plugin budget may fill.
const CGROUP_FRACTION: (u64, u64) = (3, 4);
/// Share of the machine's memory the plugin budget may fill when no cgroup
/// limits this process.
const HOST_FRACTION: (u64, u64) = (1, 4);
/// Ceiling on one instance's linear memory, whatever the budget: a runaway
/// trap, far above a legitimate working set.
const INSTANCE_MEMORY_CEILING: u64 = 1536 << 20;
/// Least time between two resident-set checks.
const CHECK_INTERVAL_MS: u64 = 50;

/// A pool the budget may trim. Implemented by the instance pool.
pub(crate) trait Trimmable: Send + Sync {
    /// The idle stamp of the least recently used instance the pool would
    /// drop, if it holds more idle instances than its warm set.
    fn oldest_spare(&self) -> Option<u64>;
    /// Drop that instance. Returns whether one was dropped.
    fn drop_oldest_spare(&self) -> bool;
}

/// Where the resident set size comes from. Tests swap in a fake.
pub type RssSource = Box<dyn Fn() -> u64 + Send + Sync>;

pub struct MemoryBudget {
    limit: u64,
    rss: RssSource,
    pools: Mutex<Vec<Weak<dyn Trimmable>>>,
    /// Logical clock for idle stamps, so recency compares across pools.
    clock: AtomicU64,
    last_check_ms: AtomicU64,
    started: Instant,
}

impl MemoryBudget {
    pub fn new(limit: u64, rss: RssSource) -> Arc<Self> {
        Arc::new(Self {
            limit,
            rss,
            pools: Mutex::new(Vec::new()),
            clock: AtomicU64::new(0),
            last_check_ms: AtomicU64::new(0),
            started: Instant::now(),
        })
    }

    /// `configured` bytes, or the automatic budget, read against this
    /// process's resident set.
    pub fn for_this_host(configured: Option<u64>) -> Arc<Self> {
        let limit = configured.unwrap_or_else(|| automatic_budget(Path::new("/")));
        Self::new(limit, Box::new(resident_set_bytes))
    }

    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// The per-instance linear memory cap, in 64 KiB pages.
    pub fn instance_memory_pages(&self) -> u32 {
        let bytes = (self.limit / 2).clamp(64 << 20, INSTANCE_MEMORY_CEILING);
        u32::try_from(bytes >> 16).unwrap_or(u32::MAX)
    }

    pub(crate) fn register(&self, pool: Weak<dyn Trimmable>) {
        let mut pools = self.pools.lock();
        pools.retain(|p| p.strong_count() > 0);
        pools.push(pool);
    }

    pub(crate) fn stamp(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    /// Called whenever an instance goes idle. Checks the resident set at
    /// most every [`CHECK_INTERVAL_MS`] and trims while it is over budget.
    pub(crate) fn after_return(&self) {
        let now = self.started.elapsed().as_millis() as u64;
        let last = self.last_check_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < CHECK_INTERVAL_MS
            || self
                .last_check_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_err()
        {
            return;
        }
        self.trim();
    }

    /// Drop idle instances, globally least recently used first, until the
    /// resident set is within budget or only warm sets are left. Returns how
    /// many were dropped.
    pub fn trim(&self) -> usize {
        let mut dropped = 0;
        while (self.rss)() > self.limit {
            let pools: Vec<Arc<dyn Trimmable>> = {
                let mut pools = self.pools.lock();
                pools.retain(|p| p.strong_count() > 0);
                pools.iter().filter_map(Weak::upgrade).collect()
            };
            let Some(oldest) = pools
                .iter()
                .filter_map(|p| p.oldest_spare().map(|stamp| (stamp, p)))
                .min_by_key(|(stamp, _)| *stamp)
            else {
                break;
            };
            if oldest.1.drop_oldest_spare() {
                dropped += 1;
            }
        }
        dropped
    }
}

/// Resident set of this process, from `/proc/self/statm`. 0 when unreadable,
/// which never trims.
pub fn resident_set_bytes() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let pages: u64 = statm
        .split_whitespace()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    pages * PAGE_SIZE
}

/// statm counts base pages, 4 KiB on the x86-64 and aarch64 targets Broccoli
/// ships for.
const PAGE_SIZE: u64 = 4096;

/// The automatic budget for a filesystem rooted at `root` (`/` outside
/// tests).
pub(crate) fn automatic_budget(root: &Path) -> u64 {
    let fraction = |bytes: u64, (num, den): (u64, u64)| bytes / den * num;
    if let Some(limit) = cgroup_limit(root) {
        return fraction(limit, CGROUP_FRACTION);
    }
    fraction(mem_total(root).unwrap_or(4 << 30), HOST_FRACTION)
}

/// The memory limit of this process's cgroup: v2 (`memory.max` of its
/// cgroup, else of the nearest parent that sets one), then v1
/// (`hierarchical_memory_limit`).
fn cgroup_limit(root: &Path) -> Option<u64> {
    let membership = std::fs::read_to_string(root.join("proc/self/cgroup")).ok()?;
    if let Some(path) = membership.lines().find_map(|l| l.strip_prefix("0::")) {
        let base = root.join("sys/fs/cgroup");
        let mut dir = base.join(path.trim_start_matches('/'));
        loop {
            if let Ok(max) = std::fs::read_to_string(dir.join("memory.max"))
                && let Ok(limit) = max.trim().parse::<u64>()
            {
                return Some(limit);
            }
            if dir == base || !dir.pop() || !dir.starts_with(&base) {
                break;
            }
        }
    }
    let stat = std::fs::read_to_string(root.join("sys/fs/cgroup/memory/memory.stat")).ok()?;
    let limit = stat
        .lines()
        .find_map(|l| l.strip_prefix("hierarchical_memory_limit "))?
        .trim()
        .parse::<u64>()
        .ok()?;
    // v1 reports "no limit" as a huge page-aligned number.
    (limit < 1 << 60).then_some(limit)
}

fn mem_total(root: &Path) -> Option<u64> {
    let meminfo = std::fs::read_to_string(root.join("proc/meminfo")).ok()?;
    let kib: u64 = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::AtomicUsize;

    fn write(root: &Path, path: &str, content: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    const GIB: u64 = 1 << 30;

    #[test]
    fn a_cgroup_v2_limit_sets_the_budget() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "proc/self/cgroup", "0::/system.slice/server\n");
        write(
            root.path(),
            "sys/fs/cgroup/system.slice/server/memory.max",
            &format!("{}\n", 8 * GIB),
        );
        assert_eq!(automatic_budget(root.path()), 6 * GIB);
    }

    #[test]
    fn an_unlimited_cgroup_v2_takes_the_nearest_limited_parent() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "proc/self/cgroup", "0::/pod/server\n");
        write(root.path(), "sys/fs/cgroup/pod/server/memory.max", "max\n");
        write(
            root.path(),
            "sys/fs/cgroup/pod/memory.max",
            &format!("{}\n", 4 * GIB),
        );
        assert_eq!(automatic_budget(root.path()), 3 * GIB);
    }

    #[test]
    fn cgroup_v1_uses_the_hierarchical_limit() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "proc/self/cgroup", "4:memory:/docker/abc\n");
        write(
            root.path(),
            "sys/fs/cgroup/memory/memory.stat",
            &format!("cache 0\nhierarchical_memory_limit {}\n", 2 * GIB),
        );
        assert_eq!(automatic_budget(root.path()), 3 * GIB / 2);
    }

    #[test]
    fn without_a_cgroup_limit_the_budget_is_a_share_of_the_machine() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "proc/self/cgroup", "0::/\n");
        write(root.path(), "sys/fs/cgroup/memory.max", "max\n");
        write(
            root.path(),
            "proc/meminfo",
            "MemTotal:       16777216 kB\nMemFree: 1 kB\n",
        );
        assert_eq!(automatic_budget(root.path()), 4 * GIB);
    }

    #[test]
    fn the_resident_set_of_this_process_is_readable() {
        assert!(resident_set_bytes() > 0);
    }

    /// A pool stand-in: `spare` idle instances stamped in order.
    struct FakePool {
        stamps: Mutex<Vec<u64>>,
        rss: Arc<AtomicUsize>,
        per_instance: usize,
    }

    impl Trimmable for FakePool {
        fn oldest_spare(&self) -> Option<u64> {
            let stamps = self.stamps.lock();
            (stamps.len() > WARM_PER_PLUGIN).then(|| stamps[0])
        }
        fn drop_oldest_spare(&self) -> bool {
            let mut stamps = self.stamps.lock();
            if stamps.len() <= WARM_PER_PLUGIN {
                return false;
            }
            stamps.remove(0);
            self.rss.fetch_sub(self.per_instance, Ordering::SeqCst);
            true
        }
    }

    #[test]
    fn trimming_drops_the_oldest_idle_instances_across_plugins_down_to_warm_sets() {
        let rss = Arc::new(AtomicUsize::new(1000));
        let budget = MemoryBudget::new(650, {
            let rss = rss.clone();
            Box::new(move || rss.load(Ordering::SeqCst) as u64)
        });
        let a: Arc<dyn Trimmable> = Arc::new(FakePool {
            stamps: Mutex::new(vec![0, 2, 4]),
            rss: rss.clone(),
            per_instance: 100,
        });
        let b: Arc<dyn Trimmable> = Arc::new(FakePool {
            stamps: Mutex::new(vec![1, 3, 5]),
            rss: rss.clone(),
            per_instance: 100,
        });
        budget.register(Arc::downgrade(&a));
        budget.register(Arc::downgrade(&b));
        // 1000 -> 900 (a:0) -> 800 (b:1) -> 700 (a:2) -> 600 (b:3).
        assert_eq!(budget.trim(), 4);
        assert_eq!(a.oldest_spare(), None);
        assert_eq!(b.oldest_spare(), None);
        assert_eq!(rss.load(Ordering::SeqCst), 600);
    }

    #[test]
    fn warm_sets_survive_any_pressure() {
        let rss = Arc::new(AtomicUsize::new(1_000_000));
        let budget = MemoryBudget::new(1, {
            let rss = rss.clone();
            Box::new(move || rss.load(Ordering::SeqCst) as u64)
        });
        let a: Arc<dyn Trimmable> = Arc::new(FakePool {
            stamps: Mutex::new(vec![0, 1]),
            rss: rss.clone(),
            per_instance: 1,
        });
        budget.register(Arc::downgrade(&a));
        assert_eq!(budget.trim(), 1);
        assert_eq!(budget.trim(), 0);
    }

    #[test]
    fn a_dropped_pool_is_forgotten() {
        let budget = MemoryBudget::new(0, Box::new(|| 1));
        let a: Arc<dyn Trimmable> = Arc::new(FakePool {
            stamps: Mutex::new(vec![0, 1]),
            rss: Arc::new(AtomicUsize::new(0)),
            per_instance: 0,
        });
        budget.register(Arc::downgrade(&a));
        drop(a);
        assert_eq!(budget.trim(), 0);
        assert!(budget.pools.lock().iter().all(|p| p.strong_count() == 0));
    }
}
