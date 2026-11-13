use super::cache_leader::CacheLeaderElector;
use super::file_cacher::FileCacher;
use super::sandbox::SandboxManager;
use super::task_cache::TaskCacheStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod box_id;
pub use box_id::configure_slot_lock_dir;
mod caching;
mod environment;
mod execution;
mod metrics;
mod paths;
mod teardown;

use crate::toolchain_fingerprint::{SharedFingerprint, ToolchainFingerprint};

pub struct OperationHandler {
    sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
    file_cacher: Arc<dyn FileCacher>,
    cache: CompileCache,
    metrics: common::metrics::Metrics,
    /// Directory holding worker-local platform tools (e.g. `broccoli-compare`).
    /// `MountSource::PlatformTool { name }` resolves to `<tools_dir>/<name>`.
    /// `None` until configured via [`OperationHandler::with_tools_dir`].
    tools_dir: Option<PathBuf>,
}

/// How a handler shares cacheable step outputs (compiles) with other
/// workers.
pub struct CompileCache {
    /// Cached outputs, by cache key.
    pub store: Arc<dyn TaskCacheStore>,
    /// Elects one worker to run a key while the others wait for its result.
    pub leader: Arc<dyn CacheLeaderElector>,
    /// How often a waiting follower polls `store` for the leader's result.
    pub follower_poll_interval: Duration,
    /// How long a follower waits before running the step itself.
    pub follower_max_wait: Duration,
    /// Part of every cache key, so only matching toolchains share outputs.
    pub toolchain_fingerprint: SharedFingerprint,
}

impl CompileCache {
    /// A cache in `store` that no other worker coordinates on.
    pub fn local(
        store: Arc<dyn TaskCacheStore>,
        toolchain_fingerprint: ToolchainFingerprint,
    ) -> Self {
        Self {
            store,
            leader: Arc::new(super::cache_leader::NoopCacheLeaderElector),
            follower_poll_interval: Duration::from_millis(250),
            follower_max_wait: Duration::from_secs(30),
            toolchain_fingerprint: toolchain_fingerprint.into(),
        }
    }
}

struct EnvironmentList {
    id: String,
    /// RAII reservation for this environment's isolate box id; released when the
    /// environment (and thus this struct) is dropped. See [`box_id::BoxId`].
    box_id: box_id::BoxId,
    working_dir: PathBuf,
}

impl EnvironmentList {
    fn new(id: String, box_id: box_id::BoxId, working_dir: PathBuf) -> Self {
        Self {
            id,
            box_id,
            working_dir,
        }
    }
}

struct StepMetricRecord {
    start: std::time::Instant,
    step_kind: &'static str,
    outcome: &'static str,
    sandbox_status: String,
    exit_kind: &'static str,
    killed: bool,
    cg_oom_killed: bool,
}
