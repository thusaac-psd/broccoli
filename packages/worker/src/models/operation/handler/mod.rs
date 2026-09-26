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

pub struct OperationHandler {
    sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
    file_cacher: Box<dyn FileCacher>,
    task_cache: Arc<dyn TaskCacheStore>,
    cache_leader: Arc<dyn CacheLeaderElector>,
    follower_poll_interval: Duration,
    follower_max_wait: Duration,
    toolchain_fingerprint: String,
    metrics: common::metrics::Metrics,
    /// Directory holding worker-local platform tools (e.g. `broccoli-compare`).
    /// `MountSource::PlatformTool { name }` resolves to `<tools_dir>/<name>`.
    /// `None` until configured via [`OperationHandler::with_tools_dir`].
    tools_dir: Option<PathBuf>,
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
