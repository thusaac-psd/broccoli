use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PluginConfig {
    #[serde(default = "default_plugins_dir")]
    pub plugins_dir: PathBuf,
    #[serde(default = "default_enable_wasi")]
    pub enable_wasi: bool,
    /// Wall-clock execution deadline for a single guest call, applied to the
    /// extism manifest (`timeout_ms`) when the plugin is loaded. Checker and
    /// evaluator plugins process contestant-influenced data, so the host must
    /// bound every execution: when a call exceeds this deadline extism cancels
    /// it, the call fails, and the pooled instance is released instead of
    /// being leaked forever. Must be positive; there is deliberately no way to
    /// disable the deadline. This does NOT govern how long a caller waits for
    /// a free pooled instance - that is `pool_acquire_timeout_secs`.
    #[serde(default = "default_call_timeout")]
    pub call_timeout_secs: u64,
    /// How long a caller may wait to acquire an instance from a plugin's pool
    /// before the call fails with a pool timeout. Distinct from
    /// `call_timeout_secs`, which bounds the guest's own execution once an
    /// instance is acquired.
    #[serde(default = "default_pool_acquire_timeout")]
    pub pool_acquire_timeout_secs: u64,
    #[serde(default = "default_pool_max_instances")]
    pub pool_max_instances: usize,
    /// Maximum concurrent evaluator plugin calls per server. When `None`, falls back
    /// to `std::thread::available_parallelism()`. Override via env var if the host
    /// has spare memory and contention is the bottleneck (recommended >= 64 for
    /// contest workloads with high per-submission test-case fan-out).
    #[serde(default)]
    pub evaluator_parallelism: Option<usize>,
    /// Last-resort runaway trap on a single WASM plugin instance's linear memory,
    /// in 64 KiB pages, applied via the extism manifest. `None` = no cap. This is
    /// *not* the primary memory bound - `instance_reclaim_bytes` recycling is.
    /// The default (24576 pages = 1.5 GiB) sits far above any legitimate
    /// per-instance working set (a checker comparing two 30 MB files peaks a few
    /// hundred MB) *and* above the steady state recycling holds instances at, so
    /// it never traps a real testcase. It only fires for a genuinely runaway
    /// instance (e.g. an unbounded allocation loop), which then fails its own
    /// testcase instead of OOM-killing the server.
    #[serde(default = "default_max_instance_memory_pages")]
    pub max_instance_memory_pages: Option<u32>,
    /// Cumulative bytes (marshalled call I/O + data streamed into the guest via
    /// host functions such as `blob_read_range`) an instance may process before
    /// it becomes eligible to be recycled - dropped and rebuilt to reclaim WASM
    /// linear memory that wasmtime never returns on its own. `None` disables
    /// recycling. The default (128 MiB) is small enough that a *data-heavy*
    /// instance (a checker streaming tens of MB per call) is recycled every few
    /// calls, keeping its resident memory bounded, while *data-light* plugins
    /// never reach it and are never recycled. This is the primary RAM bound;
    /// budget `pool_max_instances` x steady-state-per-instance against host RAM
    /// (and remember multiple server instances may share a box).
    #[serde(default = "default_instance_reclaim_bytes")]
    pub instance_reclaim_bytes: Option<u64>,
    /// Minimum calls an instance must serve before it can be recycled. A hard
    /// floor that makes per-call recreation impossible: a freshly recycled
    /// instance resets its call counter, so it always serves at least this many
    /// calls before becoming recycle-eligible again. Recycling is thus bounded
    /// to at most one rebuild per this many calls, regardless of how low
    /// `instance_reclaim_bytes` is set.
    #[serde(default = "default_instance_min_calls_before_recycle")]
    pub instance_min_calls_before_recycle: usize,
    /// Drop a pooled instance once it has sat unused this long, so the
    /// instances a burst created do not stay resident for the life of the
    /// process. Free instances are reused most recently used first, so steady
    /// traffic keeps the few it needs warm and only the surplus ages out.
    /// Rebuilding one costs ~10 ms. `0` disables eviction.
    #[serde(default = "default_pool_idle_timeout")]
    pub pool_idle_timeout_secs: u64,
}

fn default_plugins_dir() -> PathBuf {
    PathBuf::from("./plugins")
}

fn default_enable_wasi() -> bool {
    true
}

fn default_call_timeout() -> u64 {
    300
}

fn default_pool_acquire_timeout() -> u64 {
    300
}

fn default_pool_max_instances() -> usize {
    32
}

fn default_max_instance_memory_pages() -> Option<u32> {
    Some(24576) // 1.5 GiB — runaway trap only; recycling is the primary bound
}

fn default_instance_reclaim_bytes() -> Option<u64> {
    Some(128 * 1024 * 1024) // 128 MiB cumulative
}

fn default_instance_min_calls_before_recycle() -> usize {
    4
}

fn default_pool_idle_timeout() -> u64 {
    300
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            plugins_dir: default_plugins_dir(),
            enable_wasi: default_enable_wasi(),
            call_timeout_secs: default_call_timeout(),
            pool_acquire_timeout_secs: default_pool_acquire_timeout(),
            pool_max_instances: default_pool_max_instances(),
            evaluator_parallelism: None,
            max_instance_memory_pages: default_max_instance_memory_pages(),
            instance_reclaim_bytes: default_instance_reclaim_bytes(),
            instance_min_calls_before_recycle: default_instance_min_calls_before_recycle(),
            pool_idle_timeout_secs: default_pool_idle_timeout(),
        }
    }
}

impl PluginConfig {
    /// `pool_idle_timeout_secs` as a duration; `None` when eviction is off.
    pub fn pool_idle_timeout(&self) -> Option<std::time::Duration> {
        (self.pool_idle_timeout_secs > 0)
            .then(|| std::time::Duration::from_secs(self.pool_idle_timeout_secs))
    }

    pub fn check_plugins_dir(&self) -> bool {
        self.plugins_dir.exists() && self.plugins_dir.is_dir()
    }

    /// Guest-call execution deadline, for wiring into the extism manifest
    /// (`Manifest::with_timeout`) at plugin load. Clamped to at least one
    /// second so a misconfigured `0` never means "cancel every call
    /// instantly" (and can never be read as "unbounded").
    pub fn call_timeout(&self) -> Duration {
        Duration::from_secs(self.call_timeout_secs.max(1))
    }

    /// How long to wait on `pool.get` for a free instance.
    pub fn pool_acquire_timeout(&self) -> Duration {
        Duration::from_secs(self.pool_acquire_timeout_secs)
    }

    /// Resolve the effective evaluator parallelism: configured value, or fall
    /// back to `available_parallelism()`, clamped to at least 1. This is the
    /// number of concurrent test-case evaluations the server admits.
    pub fn resolve_evaluator_parallelism(&self) -> usize {
        self.evaluator_parallelism
            .unwrap_or_else(|| {
                std::thread::available_parallelism()
                    .map(|p| p.get())
                    .unwrap_or(1)
            })
            .max(1)
    }

    /// Ensure `pool_max_instances >= evaluator_parallelism`. Returns whether
    /// the value was raised; callers should log a warning on `true`.
    ///
    /// Why this matters: each evaluator slot, once admitted, calls
    /// `plugin_manager.call_raw(evaluator_plugin)` which acquires from the
    /// evaluator plugin's pool. If the pool is smaller than the semaphore,
    /// every excess admission queues inside `call_raw`, holding an evaluator
    /// permit while waiting for a pool slot. For plugins that recurse into
    /// themselves (a self-checking contest plugin where the contest call
    /// re-enters its own pool to invoke a checker handler), this is a real
    /// deadlock - every slot is held by a caller waiting for another slot.
    /// Without recursion it's "merely" hidden queueing, but the evaluator
    /// semaphore is then sized larger than actual capacity, which inflates
    /// admission and produces unbounded latency tails.
    pub fn normalize_for_safe_pool_sizing(&mut self) -> bool {
        let required = self.resolve_evaluator_parallelism();
        if self.pool_max_instances < required {
            self.pool_max_instances = required;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_evaluator_parallelism_uses_configured_value() {
        let cfg = PluginConfig {
            evaluator_parallelism: Some(16),
            ..Default::default()
        };
        assert_eq!(cfg.resolve_evaluator_parallelism(), 16);
    }

    #[test]
    fn resolve_evaluator_parallelism_clamps_zero() {
        let cfg = PluginConfig {
            evaluator_parallelism: Some(0),
            ..Default::default()
        };
        assert_eq!(cfg.resolve_evaluator_parallelism(), 1);
    }

    #[test]
    fn normalize_bumps_pool_to_match_evaluator_parallelism() {
        let mut cfg = PluginConfig {
            pool_max_instances: 8,
            evaluator_parallelism: Some(64),
            ..Default::default()
        };
        let bumped = cfg.normalize_for_safe_pool_sizing();
        assert!(bumped);
        assert_eq!(cfg.pool_max_instances, 64);
    }

    #[test]
    fn execution_and_acquire_timeouts_are_distinct_knobs() {
        let cfg = PluginConfig {
            call_timeout_secs: 30,
            pool_acquire_timeout_secs: 5,
            ..Default::default()
        };
        assert_eq!(cfg.call_timeout(), Duration::from_secs(30));
        assert_eq!(cfg.pool_acquire_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn call_timeout_is_always_positive() {
        let cfg = PluginConfig {
            call_timeout_secs: 0,
            ..Default::default()
        };
        assert_eq!(cfg.call_timeout(), Duration::from_secs(1));
    }

    #[test]
    fn defaults_bound_both_timeouts() {
        let cfg = PluginConfig::default();
        assert_eq!(cfg.call_timeout(), Duration::from_secs(300));
        assert_eq!(cfg.pool_acquire_timeout(), Duration::from_secs(300));
    }

    #[test]
    fn missing_timeout_fields_deserialize_to_bounded_defaults() {
        // Operators upgrading an existing config file must still get an
        // execution deadline and an acquisition timeout.
        let cfg: PluginConfig =
            serde_json::from_str(r#"{"plugins_dir": "./plugins", "enable_wasi": true}"#)
                .expect("deserialize minimal config");
        assert_eq!(cfg.call_timeout_secs, 300);
        assert_eq!(cfg.pool_acquire_timeout_secs, 300);
    }

    #[test]
    fn normalize_leaves_pool_alone_when_already_sufficient() {
        let mut cfg = PluginConfig {
            pool_max_instances: 128,
            evaluator_parallelism: Some(32),
            ..Default::default()
        };
        let bumped = cfg.normalize_for_safe_pool_sizing();
        assert!(!bumped);
        assert_eq!(cfg.pool_max_instances, 128);
    }
}
