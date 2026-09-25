use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::cache_leader::{
    CacheLeaderElector, CacheLeaderOpts, NoopCacheLeaderElector, RedisCacheLeaderElector,
};
use super::file_cacher::{BlobStoreFileCacher, FileCacher, NoopFileCacher};
use super::models::{MountSource, OperationTask};
use super::sandbox::SandboxManager;
use super::sandbox::isolate::IsolateSandboxManager;
use super::sandbox::mock::MockSandboxManager;
use super::task_cache::{DatabaseTaskCacheStore, NoopTaskCacheStore, TaskCacheStore};
use crate::config::WorkerAppConfig;
use crate::models::operation::handler::OperationHandler;
use crate::toolchain_fingerprint;
use anyhow::Result;
use async_trait::async_trait;
use common::storage::config::create_blob_store;
use common::worker::*;
use tracing::{error, info, warn};

pub struct OperationTaskExecutor {
    operation_executor: OperationHandler,
    /// Worker-local platform tools directory (`[worker.tools] dir`), mirrored
    /// here so a job that requests a `MountSource::PlatformTool` this worker
    /// cannot resolve is rejected *before* execution starts (see
    /// [`Self::validate_platform_tools`]).
    tools_dir: Option<PathBuf>,
}

impl OperationTaskExecutor {
    pub async fn from_app_config(
        config: &WorkerAppConfig,
        metrics: common::metrics::Metrics,
    ) -> Result<Self> {
        Self::configure_box_slot_lock_dir(config);
        let fingerprint =
            toolchain_fingerprint::compute(toolchain_fingerprint::default_probes()).await;
        info!(
            worker_toolchain_fingerprint = %fingerprint,
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
            .map(std::path::PathBuf::from);

        // Surface tool resolvability at startup so a deployment mistake is
        // visible immediately instead of only when the first job needing a
        // platform tool arrives.
        match &tools_dir {
            Some(dir) => match std::fs::read_dir(dir) {
                Ok(entries) => {
                    let tools: Vec<String> = entries
                        .filter_map(|e| e.ok())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect();
                    info!(
                        tools_dir = %dir.display(),
                        resolvable_platform_tools = ?tools,
                        "Platform tools directory configured"
                    );
                }
                Err(e) => {
                    warn!(
                        tools_dir = %dir.display(),
                        error = %e,
                        "[worker.tools] dir is configured but not readable; \
                         jobs requesting platform tools will be rejected as retryable errors"
                    );
                }
            },
            None => {
                warn!(
                    "no [worker.tools] dir configured; jobs requesting platform tools \
                     (e.g. fused standard checkers using broccoli-compare) will be \
                     rejected as retryable errors on this worker"
                );
            }
        }

        Ok(Self {
            operation_executor: OperationHandler::with_cache_leader(
                sandbox_manager,
                file_cacher,
                Arc::from(task_cache),
                cache_leader,
                follower_poll,
                follower_max_wait,
                fingerprint,
                metrics,
            )
            .with_tools_dir(tools_dir.clone()),
            tools_dir,
        })
    }

    /// Apply `[worker] box_slot_lock_dir` before any sandbox is created (the
    /// box-id slot is claimed lazily on first use, so it must be set first).
    ///
    /// A worker in a container with no shared lock dir configured is the exact
    /// setup that collapses every worker on a host into box-id slot 0, so warn
    /// loudly: nothing else surfaces it until correct solutions start coming
    /// back SystemError / TimeLimitExceeded under load.
    fn configure_box_slot_lock_dir(config: &WorkerAppConfig) {
        match &config.worker.box_slot_lock_dir {
            Some(dir) => {
                super::handler::configure_slot_lock_dir(std::path::PathBuf::from(dir));
                info!(box_slot_lock_dir = %dir, "Isolate box-id slot lock dir configured");
            }
            None if running_in_container() => {
                warn!(
                    "worker is running in a container with no [worker] box_slot_lock_dir set: \
                     it will claim its isolate box-id slot from its PRIVATE temp dir. If more \
                     than one worker container runs on this host they will all claim slot 0, \
                     run contestant code as the same UID, and exhaust each other's process \
                     limit (execve EAGAIN -> SystemError / TimeLimitExceeded). Point every \
                     worker on the host at one shared directory via \
                     BROCCOLI__WORKER__BOX_SLOT_LOCK_DIR."
                );
            }
            None => {}
        }
    }

    /// Reject an operation up front when it requests a platform tool this
    /// worker cannot resolve (no `[worker.tools]` dir configured, or the tool
    /// is missing from it).
    ///
    /// Without this guard the requesting step fails mid-plan with
    /// `success = false`, which evaluator plugins interpret as a judging
    /// outcome - a pure worker deployment mistake then surfaces as a wrong
    /// contestant-visible verdict. Failing the whole job with an `Err` before
    /// any environment or sibling task is launched means the consumer's
    /// RetryTracker treats it as a transient/system error (bounded retries,
    /// then DLQ/SystemError), and no concurrently launched step can block on
    /// a channel whose peer never started.
    fn validate_platform_tools(&self, operation: &OperationTask) -> Result<()> {
        for step in &operation.tasks {
            for mount in &step.mounts {
                let MountSource::PlatformTool { name } = &mount.source else {
                    continue;
                };
                let Some(tools_dir) = self.tools_dir.as_deref() else {
                    anyhow::bail!(
                        "step '{}' requests platform tool '{}' but no [worker.tools] dir \
                         is configured on this worker; rejecting the job before execution",
                        step.id,
                        name
                    );
                };
                let tool_path = tools_dir.join(name);
                if !tool_path.exists() {
                    anyhow::bail!(
                        "step '{}' requests platform tool '{}' but it is not present at {}; \
                         rejecting the job before execution",
                        step.id,
                        name,
                        tool_path.display()
                    );
                }
            }
        }
        Ok(())
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
                    redis_url = %config.mq.url,
                    ttl_secs = opts.ttl_secs,
                    heartbeat_interval_secs = opts.heartbeat_interval_secs,
                    "RedisCacheLeaderElector initialized"
                );
                Arc::new(elector)
            }
            Err(e) => {
                warn!(
                    redis_url = %config.mq.url,
                    error = %e,
                    "Failed to build RedisCacheLeaderElector, falling back to no-op"
                );
                Arc::new(NoopCacheLeaderElector)
            }
        }
    }

    #[allow(dead_code)]
    pub fn new_with_sandbox_manager(
        sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
        metrics: common::metrics::Metrics,
    ) -> Self {
        Self {
            operation_executor: OperationHandler::new(
                sandbox_manager,
                Box::new(NoopFileCacher),
                Box::new(NoopTaskCacheStore),
                String::new(),
                metrics,
            ),
            tools_dir: None,
        }
    }

    #[allow(dead_code)]
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
                String::new(),
                metrics,
            ),
            tools_dir,
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
    ) -> Result<(Box<dyn FileCacher>, Box<dyn TaskCacheStore>)> {
        let blob_store_config = config.storage.blob_store.clone();
        let cache_dir = config.storage.cache_dir.clone();
        let max_cache_size = config.storage.max_cache_size;

        let database_config = config.database.clone();
        let mut connect_options = sea_orm::ConnectOptions::new(database_config.url.clone());
        connect_options
            .max_connections(database_config.max_connections)
            .min_connections(database_config.max_connections.min(1))
            .connect_timeout(Duration::from_secs(30))
            .acquire_timeout(Duration::from_secs(30))
            .idle_timeout(Duration::from_secs(600))
            .max_lifetime(Duration::from_secs(1800));

        let db = match sea_orm::Database::connect(connect_options).await {
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

        let file_cacher: Box<dyn FileCacher> = match BlobStoreFileCacher::new(
            blob_store,
            PathBuf::from(&cache_dir),
            max_cache_size,
            Some(metrics.clone()),
        )
        .await
        {
            Ok(cacher) => Box::new(cacher),
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
                Box::new(DatabaseTaskCacheStore::new(db_for_cache))
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

        // Fail fast (as a retryable infra error, never a verdict) if this
        // worker cannot resolve a platform tool the plan requires.
        self.validate_platform_tools(&operation)?;

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
                enqueued_at_unix_ms: None,
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
        executor.tools_dir = tools_dir;
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
            priority: None,
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
        assert!(
            msg.contains("broccoli-compare"),
            "message names the tool: {msg}"
        );
        assert!(
            msg.contains("[worker.tools]"),
            "message names the config key: {msg}"
        );
        // The rejection must be classified as retryable by the worker (see
        // is_permanent_execution_failure in models/worker.rs, which only
        // treats blob-storage errors as permanent) so it surfaces as a
        // SystemError, never a contestant-facing verdict.
        let lower = msg.to_ascii_lowercase();
        for permanent_marker in [
            "blob not found",
            "invalid content hash",
            "exceeds size limit",
            "blob storage is unavailable",
        ] {
            assert!(
                !lower.contains(permanent_marker),
                "missing-tools rejection must stay retryable, but message matches \
                 permanent marker '{permanent_marker}': {msg}"
            );
        }
    }

    #[test]
    fn rejects_platform_tool_job_when_tool_is_missing_from_tools_dir() {
        let dir = std::env::temp_dir().join(format!(
            "broccoli-tools-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let executor = executor_with_tools_dir(Some(dir.clone()));
        let operation = operation_with_steps(vec![step_with_mounts(
            "check",
            vec![platform_tool_mount("broccoli-compare")],
        )]);

        let err = executor
            .validate_platform_tools(&operation)
            .expect_err("job requiring an absent tool must be rejected up front");
        assert!(err.to_string().contains("broccoli-compare"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accepts_platform_tool_job_when_tool_is_present() {
        let dir = std::env::temp_dir().join(format!(
            "broccoli-tools-test-present-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("broccoli-compare"), b"#!/bin/sh\n").unwrap();
        let executor = executor_with_tools_dir(Some(dir.clone()));
        let operation = operation_with_steps(vec![step_with_mounts(
            "check",
            vec![platform_tool_mount("broccoli-compare")],
        )]);

        executor
            .validate_platform_tools(&operation)
            .expect("resolvable platform tool must pass validation");
        let _ = std::fs::remove_dir_all(&dir);
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
