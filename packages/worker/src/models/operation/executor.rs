use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::cache_leader::{
    CacheLeaderElector, CacheLeaderOpts, NoopCacheLeaderElector, RedisCacheLeaderElector,
};
#[cfg(any(test, feature = "test-support"))]
use super::file_cacher::NoopFileCacher;
use super::file_cacher::{BlobStoreFileCacher, FileCacher};
use super::models::{MountSource, OperationTask};
use super::sandbox::SandboxManager;
use super::sandbox::isolate::IsolateSandboxManager;
use super::sandbox::mock::MockSandboxManager;
use super::task_cache::{
    DatabaseTaskCacheStore, MemoizedTaskCacheStore, NoopTaskCacheStore, TaskCacheStore,
};
use crate::config::WorkerAppConfig;
use crate::models::operation::handler::{
    BoxIds, CompileCache, OperationHandler, isolate_num_boxes,
};
use crate::toolchain_fingerprint;
use anyhow::Result;
use async_trait::async_trait;
use common::storage::config::create_blob_store;
use common::worker::*;
use tracing::{error, info, warn};

/// How many box ids the mock sandbox shares; it has no isolate config.
const DEFAULT_MOCK_BOX_IDS: u32 = 1000;

pub struct OperationTaskExecutor {
    operation_executor: OperationHandler,
    /// The platform tools already reported missing, each logged once.
    reported_missing: std::sync::Mutex<std::collections::HashSet<String>>,
}

/// A job asked for a platform tool this worker does not have: a deployment
/// mistake on this worker, never the job's fault and never fixed by running
/// it again.
#[derive(Debug, thiserror::Error)]
#[error(
    "step '{step}' requests platform tool '{tool}', which this worker does not have ({why}); \
     set [worker.tools] dir (BROCCOLI__WORKER__TOOLS__DIR) to the directory holding it"
)]
pub struct MissingPlatformTool {
    pub step: String,
    pub tool: String,
    pub why: String,
}

/// The tool every judging plan with a standard checker mounts.
const COMPARE_TOOL: &str = "broccoli-compare";

/// Where platform tools are when `[worker.tools] dir` is unset, from where
/// the running binary is: the first of `<bin>/tools`, `<bin>/../../tools`
/// (the native bundle's `<data>/target/release/worker` beside
/// `<data>/tools`) and `<bin>` itself when it holds the tools (a cargo target
/// directory, where `cargo build -p broccoli-compare` puts the comparator next
/// to the worker), that holds the comparator.
pub fn derived_tools_dir(binary: &std::path::Path) -> Option<PathBuf> {
    let bin = binary.parent()?;
    [
        bin.join("tools"),
        bin.join("../../tools"),
        bin.to_path_buf(),
    ]
    .into_iter()
    .find(|dir| dir.join(COMPARE_TOOL).is_file())
}

impl OperationTaskExecutor {
    pub async fn from_app_config(
        config: &WorkerAppConfig,
        metrics: common::metrics::Metrics,
    ) -> Result<Self> {
        let box_ids = Self::box_ids_from_config(config)?;
        let fingerprint = toolchain_fingerprint::SharedFingerprint::probe(
            toolchain_fingerprint::default_probes(),
        )
        .await;
        info!(
            worker_toolchain_fingerprint = %fingerprint.current(),
            "Computed toolchain fingerprint for compile cache key"
        );
        let sandbox_manager =
            Self::sandbox_manager_from_config(Some(config), Some(metrics.clone()));
        let (file_cacher, task_cache) = Self::caching_from_config(config, metrics.clone()).await?;
        let cache_leader = Self::cache_leader_from_config(config);
        let follower_poll = Duration::from_millis(config.worker.cache_follower_poll_interval_ms);
        let follower_max_wait = Duration::from_secs(config.worker.cache_follower_max_wait_secs);

        let tools_dir = config
            .worker
            .tools
            .dir
            .clone()
            .map(PathBuf::from)
            .or_else(|| derived_tools_dir(&std::env::current_exe().ok()?));
        report_tools_dir(tools_dir.as_deref());

        Ok(Self {
            operation_executor: OperationHandler::with_compile_cache(
                sandbox_manager,
                file_cacher,
                CompileCache {
                    store: Arc::from(task_cache),
                    leader: cache_leader,
                    follower_poll_interval: follower_poll,
                    follower_max_wait,
                    toolchain_fingerprint: fingerprint,
                },
                metrics,
            )
            .with_tools_dir(tools_dir)
            .with_box_ids(box_ids),
            reported_missing: Default::default(),
        })
    }

    /// Wait, at most `bound`, for the sandbox removals of finished operations.
    pub async fn finish_sandbox_removals(&self, bound: Duration) {
        self.operation_executor.finish_sandbox_removals(bound).await;
    }

    /// The file cacher jobs materialize blobs through, for the pre-warm
    /// subscriber to fill the same cache.
    pub fn file_cacher(&self) -> Arc<dyn FileCacher> {
        self.operation_executor.file_cacher()
    }

    /// The host's box ids: locked in `[worker] box_id_lock_dir`, numbered
    /// `0..num_boxes` from the isolate config with the isolate backend. A lock
    /// dir that cannot hold locks, or an isolate config that cannot be read,
    /// stops the worker here rather than letting it share ids it cannot lock.
    fn box_ids_from_config(config: &WorkerAppConfig) -> Result<Arc<BoxIds>> {
        let dir = match &config.worker.box_id_lock_dir {
            Some(dir) => PathBuf::from(dir),
            None => {
                if running_in_container() {
                    warn!(
                        "worker is running in a container with no [worker] box_id_lock_dir set: \
                         it locks its isolate box ids in its PRIVATE temp dir. If more than one \
                         worker container runs on this host they will initialise each other's \
                         boxes under running programs. Point every worker on the host at one \
                         shared directory via BROCCOLI__WORKER__BOX_ID_LOCK_DIR."
                    );
                }
                std::env::temp_dir().join("broccoli-box-ids")
            }
        };
        let count = if config.worker.sandbox_backend.eq_ignore_ascii_case("mock") {
            DEFAULT_MOCK_BOX_IDS
        } else {
            let path = &config.worker.isolate_config;
            let text = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("cannot read the isolate config {path}: {e}"))?;
            isolate_num_boxes(&text).map_err(|e| anyhow::anyhow!("isolate config {path}: {e}"))?
        };
        let box_ids = BoxIds::open(&dir, count)?;
        info!(box_id_lock_dir = %dir.display(), box_ids = count, "Isolate box ids configured");
        Ok(box_ids)
    }

    /// Reject an operation up front when it requests a platform tool this
    /// worker cannot resolve, before any environment or step is launched (so
    /// no step can block on a channel whose peer never started).
    fn validate_platform_tools(
        &self,
        operation: &OperationTask,
    ) -> Result<(), MissingPlatformTool> {
        for step in &operation.tasks {
            for mount in &step.mounts {
                let MountSource::PlatformTool { name } = &mount.source else {
                    continue;
                };
                let missing = |why: String| MissingPlatformTool {
                    step: step.id.clone(),
                    tool: name.clone(),
                    why,
                };
                let Some(tools_dir) = self.operation_executor.tools_dir() else {
                    return Err(missing("no platform tools directory".into()));
                };
                let tool_path = tools_dir.join(name);
                if !tool_path.exists() {
                    return Err(missing(format!("not present at {}", tool_path.display())));
                }
            }
        }
        Ok(())
    }

    /// The failed result of a job this worker cannot run for lack of a
    /// platform tool. Failing it at once, without the execution retries, keeps
    /// one misconfigured worker from holding every such job through its retry
    /// budget: the server sees a failed operation (a `SystemError` for the
    /// test case), and the error is logged once per tool.
    fn missing_tool_result(&self, task_id: String, missing: &MissingPlatformTool) -> TaskResult {
        let first = self
            .reported_missing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(missing.tool.clone());
        if first {
            error!(error = %missing, "Failing jobs that need a platform tool this worker does not have");
        }
        let message = missing.to_string();
        TaskResult {
            task_id,
            success: false,
            output: serde_json::json!({ "error": message }),
            error: Some(message),
            task_type: None,
            operation: None,
            worker_id: None,
        }
    }

    fn cache_leader_from_config(config: &WorkerAppConfig) -> Arc<dyn CacheLeaderElector> {
        if !config.worker.cache_leader_election_enabled || !config.mq.enabled {
            info!("cache-leader-election disabled, using NoopCacheLeaderElector");
            return Arc::new(NoopCacheLeaderElector);
        }
        let opts = CacheLeaderOpts {
            ttl_secs: config.worker.cache_leader_ttl_secs,
            heartbeat_interval_secs: config.worker.cache_leader_heartbeat_interval_secs,
        };
        match RedisCacheLeaderElector::from_url(&config.mq.url, opts) {
            Ok(elector) => {
                info!(
                    redis_url = %common::redact::redact_url(&config.mq.url),
                    ttl_secs = opts.ttl_secs,
                    heartbeat_interval_secs = opts.heartbeat_interval_secs,
                    "RedisCacheLeaderElector initialized"
                );
                Arc::new(elector)
            }
            Err(e) => {
                warn!(
                    redis_url = %common::redact::redact_url(&config.mq.url),
                    error = %e,
                    "Failed to build RedisCacheLeaderElector, falling back to no-op"
                );
                Arc::new(NoopCacheLeaderElector)
            }
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn new_with_sandbox_manager(
        sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
        metrics: common::metrics::Metrics,
    ) -> Self {
        Self {
            operation_executor: OperationHandler::new(
                sandbox_manager,
                Box::new(NoopFileCacher),
                Box::new(NoopTaskCacheStore),
                toolchain_fingerprint::ToolchainFingerprint::Known(String::new()),
                metrics,
            ),
            reported_missing: Default::default(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub async fn new_with_sandbox_manager_and_blob_store(
        sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
        blob_store: Arc<dyn common::storage::BlobStore>,
        cache_dir: PathBuf,
        metrics: common::metrics::Metrics,
        tools_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let file_cacher = BlobStoreFileCacher::new(blob_store, cache_dir, 1_000_000_000, None)
            .await
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            operation_executor: OperationHandler::new(
                sandbox_manager,
                Box::new(file_cacher),
                Box::new(NoopTaskCacheStore),
                toolchain_fingerprint::ToolchainFingerprint::Known(String::new()),
                metrics,
            )
            .with_tools_dir(tools_dir),
            reported_missing: Default::default(),
        })
    }

    fn sandbox_manager_from_config(
        config: Option<&WorkerAppConfig>,
        metrics: Option<common::metrics::Metrics>,
    ) -> Box<dyn SandboxManager + Send + Sync> {
        let backend = config
            .map(|c| c.worker.sandbox_backend.clone())
            .unwrap_or_else(|| {
                warn!("No config available, fallback to isolate sandbox");
                "isolate".to_string()
            });

        if backend.eq_ignore_ascii_case("mock") {
            info!(sandbox_backend = "mock", "Using operation sandbox backend");
            Box::new(MockSandboxManager::default())
        } else {
            if !backend.eq_ignore_ascii_case("isolate") {
                warn!(sandbox_backend = %backend, "Unknown sandbox backend, fallback to isolate");
            }
            info!(
                sandbox_backend = "isolate",
                "Using operation sandbox backend"
            );
            let isolate_bin = config
                .map(|c| c.worker.isolate_bin.clone())
                .unwrap_or_else(|| "isolate".to_string());
            let enable_cgroups = config.map(|c| c.worker.enable_cgroups).unwrap_or(false);
            Box::new(IsolateSandboxManager::new_with_metrics(
                isolate_bin,
                enable_cgroups,
                metrics,
            ))
        }
    }

    async fn caching_from_config(
        config: &WorkerAppConfig,
        metrics: common::metrics::Metrics,
    ) -> Result<(Arc<dyn FileCacher>, Box<dyn TaskCacheStore>)> {
        let blob_store_config = config.storage.blob_store.clone();
        let cache_dir = config.storage.cache_dir.clone();
        let max_cache_size = config.storage.max_cache_size;

        let database_config = &config.database;
        let db = match common::db::connect(
            &database_config.url,
            &common::db::PgPoolSpec::new(config.database_pool_size(), 1),
            &database_config.session(),
        )
        .await
        {
            Ok(db) => db,
            Err(e) => {
                error!(
                    error = %e,
                    "Failed to connect to database"
                );
                return Err(e.into());
            }
        };

        let db_for_cache = db.clone();

        let blob_store =
            match create_blob_store(&blob_store_config, db, Some(metrics.clone())).await {
                Ok(store) => {
                    info!(
                        backend = %blob_store_config.backend,
                        cache_dir = %cache_dir,
                        max_cache_size,
                        "Blob store initialized"
                    );
                    store
                }
                Err(e) => {
                    error!(error = %e, "Failed to initialize blob store");
                    return Err(e.into());
                }
            };

        let file_cacher: Arc<dyn FileCacher> = match BlobStoreFileCacher::new(
            blob_store,
            PathBuf::from(&cache_dir),
            max_cache_size,
            Some(metrics.clone()),
        )
        .await
        {
            Ok(cacher) => Arc::new(cacher),
            Err(e) => {
                error!(
                    error = %e,
                    "Failed to initialize BlobStoreFileCacher"
                );
                return Err(anyhow::anyhow!(e));
            }
        };

        let task_cache: Box<dyn TaskCacheStore> = match DatabaseTaskCacheStore::ensure_table(
            &db_for_cache,
        )
        .await
        {
            Ok(()) => {
                info!("DatabaseTaskCacheStore initialized");
                Box::new(MemoizedTaskCacheStore::new(DatabaseTaskCacheStore::new(
                    db_for_cache,
                )))
            }
            Err(e) => {
                warn!(error = %e, "Failed to ensure task_cache table, using NoopTaskCacheStore");
                Box::new(NoopTaskCacheStore)
            }
        };

        Ok((file_cacher, task_cache))
    }
}

#[async_trait]
impl Executor for OperationTaskExecutor {
    fn if_accept(&self, task_type: &str) -> bool {
        task_type == "operation"
    }
    async fn execute(&self, task: Task) -> Result<TaskResult> {
        let operation: OperationTask = serde_json::from_value(task.payload.clone())
            .map_err(|e| anyhow::anyhow!("Failed to deserialize operation config: {}", e))?;

        if let Err(missing) = self.validate_platform_tools(&operation) {
            return Ok(self.missing_tool_result(task.id, &missing));
        }

        match self.operation_executor.execute(&operation).await {
            Ok(result) => Ok(TaskResult {
                task_id: task.id,
                success: result.success,
                output: serde_json::to_value(&result)
                    .map_err(|e| anyhow::anyhow!("Failed to serialize result: {}", e))?,
                error: if result.success {
                    None
                } else {
                    Some(
                        result
                            .error
                            .clone()
                            .unwrap_or_else(|| "Operation failed".into()),
                    )
                },
                task_type: None,
                operation: None,
                worker_id: None,
            }),
            // Propagate infra/execution errors (blob fetch, sandbox setup, IO) so
            // the worker can classify transient failures and retry them via the
            // RetryTracker instead of finalizing a spurious SystemError. A *ran*
            // operation that merely produced a failing verdict returns through the
            // Ok arm with result.success=false and is unaffected.
            Err(e) => Err(e),
        }
    }
}

/// Report at boot where platform tools resolve from, so a deployment
/// mistake shows before the first job that needs one.
fn report_tools_dir(tools_dir: Option<&std::path::Path>) {
    let Some(dir) = tools_dir else {
        error!(
            "No platform tools directory: [worker.tools] dir is unset and no {COMPARE_TOOL} \
             was found beside the worker binary. Every job with a standard checker fails on \
             this worker until BROCCOLI__WORKER__TOOLS__DIR names the directory holding it"
        );
        return;
    };
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            let tools: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            info!(
                tools_dir = %dir.display(),
                resolvable_platform_tools = ?tools,
                "Platform tools directory"
            );
        }
        Err(e) => error!(
            tools_dir = %dir.display(),
            error = %e,
            "The platform tools directory is not readable; jobs that need a platform tool fail on this worker"
        ),
    }
}

/// Best-effort container detection (Docker's `/.dockerenv`, Podman's
/// `/run/.containerenv`). Only used to decide whether to warn.
fn running_in_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
}

#[cfg(test)]
mod platform_tool_validation_tests {
    use super::*;
    use crate::models::operation::models::{IOConfig, MountSpec, RunOptions, Step, StepKind};

    fn executor_with_tools_dir(tools_dir: Option<PathBuf>) -> OperationTaskExecutor {
        // Use a LOCAL meter provider (not the global one): these tests do not
        // assert metrics, and calling the global-installing init_metrics here
        // races with and drops the provider the metrics permit-guard test relies
        // on, making it flaky.
        let (metrics, _provider) =
            common::observability::init_metrics_local("broccoli-worker-test");
        let mut executor = OperationTaskExecutor::new_with_sandbox_manager(
            Box::new(MockSandboxManager::default()),
            metrics,
        );
        executor.operation_executor = executor.operation_executor.with_tools_dir(tools_dir);
        executor
    }

    fn step_with_mounts(id: &str, mounts: Vec<MountSpec>) -> Step {
        Step {
            id: id.to_string(),
            kind: StepKind::Generic,
            env_ref: "env".to_string(),
            argv: vec!["true".to_string()],
            conf: RunOptions::default(),
            io: IOConfig::default(),
            collect: vec![],
            depends_on: vec![],
            cache: None,
            mounts,
        }
    }

    fn operation_with_steps(steps: Vec<Step>) -> OperationTask {
        OperationTask {
            environments: vec![],
            tasks: steps,
            channels: vec![],
            target_worker_id: None,
            evaluate_batch_id: None,
            test_case_id: None,
        }
    }

    fn platform_tool_mount(name: &str) -> MountSpec {
        MountSpec {
            inside_path: format!("/tools/{name}"),
            source: MountSource::PlatformTool {
                name: name.to_string(),
            },
        }
    }

    #[test]
    fn rejects_platform_tool_job_when_no_tools_dir_is_configured() {
        let executor = executor_with_tools_dir(None);
        let operation = operation_with_steps(vec![step_with_mounts(
            "check",
            vec![platform_tool_mount("broccoli-compare")],
        )]);

        let err = executor
            .validate_platform_tools(&operation)
            .expect_err("job requiring a platform tool must be rejected up front");
        let msg = err.to_string();
        assert!(msg.contains("broccoli-compare"), "names the tool: {msg}");
        assert!(
            msg.contains("BROCCOLI__WORKER__TOOLS__DIR"),
            "names the setting: {msg}"
        );
    }

    /// A job this worker cannot run for lack of a platform tool fails at
    /// once with a result, not an error the consumer would retry: one
    /// misconfigured worker must not hold every such job through its retries.
    #[tokio::test]
    async fn a_job_needing_a_missing_tool_fails_at_once_without_retries() {
        let executor = executor_with_tools_dir(None);
        let operation = operation_with_steps(vec![step_with_mounts(
            "check",
            vec![platform_tool_mount("broccoli-compare")],
        )]);
        let task = Task {
            id: "t".into(),
            task_type: "operation".into(),
            executor_name: "operation".into(),
            payload: serde_json::to_value(&operation).unwrap(),
            reply_to: "s".into(),
            operation_batch_id: None,
            trace_context: None,
            judgement: None,
        };
        let result = executor
            .execute(task)
            .await
            .expect("a result, not an error");
        assert!(!result.success);
        assert!(result.error.unwrap().contains("broccoli-compare"));
    }

    #[test]
    fn the_tools_dir_is_derived_from_the_release_layouts() {
        let root = tempfile::tempdir().unwrap();
        let place = |dir: &std::path::Path| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(COMPARE_TOOL), b"").unwrap();
        };
        // `tools` beside the binary.
        let bin = root.path().join("a/bin");
        place(&bin.join("tools"));
        assert_eq!(
            derived_tools_dir(&bin.join("worker")),
            Some(bin.join("tools"))
        );
        // The native bundle: `<data>/target/release/worker`, `<data>/tools`.
        let data = root.path().join("b");
        place(&data.join("tools"));
        let release = data.join("target/release");
        std::fs::create_dir_all(&release).unwrap();
        let found = derived_tools_dir(&release.join("worker")).unwrap();
        assert_eq!(
            found.canonicalize().unwrap(),
            data.join("tools").canonicalize().unwrap()
        );
        // A cargo target dir with the comparator built next to the worker.
        let debug = root.path().join("c/target/debug");
        place(&debug);
        assert_eq!(
            derived_tools_dir(&debug.join("worker")),
            Some(debug.clone())
        );
        // Nothing found.
        let bare = root.path().join("d");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(derived_tools_dir(&bare.join("worker")), None);
    }

    #[test]
    fn rejects_platform_tool_job_when_tool_is_missing_from_tools_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let executor = executor_with_tools_dir(Some(dir.clone()));
        let operation = operation_with_steps(vec![step_with_mounts(
            "check",
            vec![platform_tool_mount("broccoli-compare")],
        )]);

        let err = executor
            .validate_platform_tools(&operation)
            .expect_err("job requiring an absent tool must be rejected up front");
        assert!(err.to_string().contains("broccoli-compare"));
    }

    #[test]
    fn accepts_platform_tool_job_when_tool_is_present() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        std::fs::write(dir.join("broccoli-compare"), b"#!/bin/sh\n").unwrap();
        let executor = executor_with_tools_dir(Some(dir.clone()));
        let operation = operation_with_steps(vec![step_with_mounts(
            "check",
            vec![platform_tool_mount("broccoli-compare")],
        )]);

        executor
            .validate_platform_tools(&operation)
            .expect("resolvable platform tool must pass validation");
    }

    #[test]
    fn job_without_platform_tools_passes_even_without_tools_dir() {
        let executor = executor_with_tools_dir(None);
        let operation = operation_with_steps(vec![step_with_mounts("run", vec![])]);

        executor
            .validate_platform_tools(&operation)
            .expect("plain job must not require a tools dir");
    }
}
