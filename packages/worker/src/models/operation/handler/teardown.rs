//! Removing an operation's sandboxes and shared channels directory.

use super::super::sandbox::SandboxManager;
use super::EnvironmentList;
use futures::future::join_all;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::error;

/// What an operation leaves behind for removal.
pub(super) struct Leftovers {
    pub(super) environments: HashMap<String, EnvironmentList>,
    pub(super) channels_dir: Option<PathBuf>,
}

impl Leftovers {
    /// Remove the channels directory and every sandbox, the sandboxes at once.
    /// The environments, and so their box ids, are released once all are gone.
    pub(super) async fn remove(self, sandbox_manager: &(dyn SandboxManager + Send + Sync)) {
        if let Some(dir) = &self.channels_dir
            && let Err(e) = tokio::fs::remove_dir_all(dir).await
        {
            error!(error = %e, "Failed to clean up shared channels directory");
        }
        join_all(self.environments.values().map(|env| async move {
            if let Err(e) = sandbox_manager.remove_sandbox(env.box_id.as_str()).await {
                error!(env_id = %env.id, error = %e, "Failed to cleanup sandbox");
            }
        }))
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::super::OperationHandler;
    use crate::models::operation::file_cacher::NoopFileCacher;
    use crate::models::operation::models::{
        Environment, IOConfig, OperationTask, RunOptions, Step, StepKind,
    };
    use crate::models::operation::sandbox::error::SandboxError;
    use crate::models::operation::sandbox::mock::MockSandboxManager;
    use crate::models::operation::sandbox::{ExecutionResult, SandboxManager};
    use crate::models::operation::task_cache::NoopTaskCacheStore;
    use crate::toolchain_fingerprint::ToolchainFingerprint;
    use async_trait::async_trait;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::sync::Barrier;

    /// A mock sandbox that logs creations and finished removals. Barriers make
    /// creations or removals wait for each other.
    struct Recording {
        inner: MockSandboxManager,
        log: Mutex<Vec<&'static str>>,
        create_barrier: Option<Barrier>,
        remove_barrier: Option<Barrier>,
    }

    impl Recording {
        fn new(dir: &std::path::Path) -> Self {
            Self {
                inner: MockSandboxManager::new(dir.to_path_buf()),
                log: Mutex::new(Vec::new()),
                create_barrier: None,
                remove_barrier: None,
            }
        }

        fn log(&self) -> Vec<&'static str> {
            self.log.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl SandboxManager for Arc<Recording> {
        async fn create_sandbox(&self, id: Option<&str>) -> Result<PathBuf, SandboxError> {
            self.log.lock().unwrap().push("create");
            if let Some(barrier) = &self.create_barrier {
                barrier.wait().await;
            }
            self.inner.create_sandbox(id).await
        }

        async fn remove_sandbox(&self, id: &str) -> Result<(), SandboxError> {
            if let Some(barrier) = &self.remove_barrier {
                barrier.wait().await;
            }
            let removed = self.inner.remove_sandbox(id).await;
            self.log.lock().unwrap().push("removed");
            removed
        }

        async fn execute(
            &self,
            box_id: &str,
            argv: Vec<String>,
            run_options: &RunOptions,
        ) -> Result<ExecutionResult, SandboxError> {
            self.inner.execute(box_id, argv, run_options).await
        }
    }

    fn handler(sandbox: &Arc<Recording>) -> OperationHandler {
        let (metrics, _provider) =
            common::observability::init_metrics_local("broccoli-worker-test");
        OperationHandler::new(
            Box::new(sandbox.clone()),
            Box::new(NoopFileCacher),
            Box::new(NoopTaskCacheStore),
            ToolchainFingerprint::Known(String::new()),
            metrics,
        )
    }

    /// Two environments, a solution and a checker, each running `true`.
    fn two_box_operation() -> OperationTask {
        let step = |id: &str| Step {
            id: id.into(),
            kind: StepKind::Generic,
            env_ref: id.into(),
            argv: vec!["true".into()],
            conf: RunOptions::default(),
            io: IOConfig::default(),
            collect: vec![],
            depends_on: vec![],
            cache: None,
            mounts: vec![],
        };
        let env = |id: &str| Environment {
            id: id.into(),
            files_in: vec![],
        };
        OperationTask {
            environments: vec![env("solution"), env("checker")],
            tasks: vec![step("solution"), step("checker")],
            channels: vec![],
            target_worker_id: None,
            evaluate_batch_id: None,
            test_case_id: None,
        }
    }

    /// Fails a test whose sandboxes wait on each other forever, instead of
    /// hanging it. Only a broken step graph ever reaches the bound.
    async fn bounded<T>(fut: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(60), fut)
            .await
            .expect("the sandboxes never met")
    }

    #[tokio::test]
    async fn an_operations_sandboxes_are_created_together() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(Recording {
            create_barrier: Some(Barrier::new(2)),
            ..Recording::new(dir.path())
        });
        let handler = handler(&sandbox);

        let result = bounded(handler.execute(&two_box_operation()))
            .await
            .unwrap();
        assert!(result.success, "{result:?}");
    }

    #[tokio::test]
    async fn an_operations_sandboxes_are_removed_together() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(Recording {
            remove_barrier: Some(Barrier::new(2)),
            ..Recording::new(dir.path())
        });
        let handler = handler(&sandbox);

        let result = bounded(handler.execute(&two_box_operation()))
            .await
            .unwrap();
        assert!(result.success, "{result:?}");
        assert_eq!(sandbox.log(), ["create", "create", "removed", "removed"]);
    }
}
