//! Removing an operation's sandboxes and shared channels directory.
//!
//! The handler returns an operation's result before its sandboxes are removed,
//! so the removal overlaps publishing that result and receiving the next
//! operation instead of delaying both. The next operation on the handler waits
//! for every pending removal before it creates a sandbox, so no step ever runs
//! beside a removal, and a box id stays reserved until its box is gone.
//!
//! Boxes are never reused across operations, not even across test cases of one
//! submission. `isolate --init` gives every operation an empty box and a fresh
//! cgroup. A reused box would keep the files the previous test case's program
//! wrote, which could carry answers from one case to the next, and its cgroup,
//! whose CPU time, memory peak and OOM-kill count are cumulative since
//! `--init`, so one case's OOM kill would mark every later case memory-killed.
//! Clearing all of that is what `--cleanup` and `--init` do. Boxes initialised
//! ahead of time do not pay off either: initialising one while a step runs
//! would put process spawns and cgroup setup beside a timed run, and between
//! operations the next one waits for that work anyway.
//!
//! Checkers keep a box of their own, platform tools included. On real isolate
//! a second box adds about 0.4 ms at the median to a whole operation
//! (`operation_1_box` against `operation_2_boxes` in `tests/isolate_overhead.rs`),
//! and `--init` and `--cleanup` cost about 0.7 ms each, so running a checker
//! outside isolate would save less than a millisecond for a jail with its own
//! escape surface.

use super::super::sandbox::SandboxManager;
use super::EnvironmentList;
use futures::future::join_all;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;
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

/// Removals a [`Teardown::settle`] took and has not seen finish. A finished
/// removal is popped in the same poll that returns it, so only unfinished ones
/// are ever put back.
struct Unsettled<'a> {
    teardown: &'a Teardown,
    removals: Vec<JoinHandle<()>>,
}

impl Drop for Unsettled<'_> {
    fn drop(&mut self) {
        if !self.removals.is_empty() {
            self.teardown.pending().append(&mut self.removals);
        }
    }
}

/// Removals running in the background.
#[derive(Default)]
pub(super) struct Teardown {
    pending: Mutex<Vec<JoinHandle<()>>>,
}

impl Teardown {
    /// Start removing `leftovers` in the background.
    pub(super) fn defer(
        &self,
        sandbox_manager: Arc<dyn SandboxManager + Send + Sync>,
        leftovers: Leftovers,
    ) {
        let removal = tokio::spawn(async move { leftovers.remove(&*sandbox_manager).await });
        self.pending().push(removal);
    }

    /// Wait until every removal started so far has finished. Dropping the
    /// wait midway puts the removals it had not seen finish back, so the next
    /// wait still covers them.
    pub(super) async fn settle(&self) {
        let mut unsettled = Unsettled {
            teardown: self,
            removals: std::mem::take(&mut *self.pending()),
        };
        while let Some(removal) = unsettled.removals.last_mut() {
            if let Err(e) = removal.await {
                error!(error = %e, "Sandbox removal task failed");
            }
            unsettled.removals.pop();
        }
    }

    /// The pending removals, recovered from poisoning: every update is a
    /// single push or take.
    fn pending(&self) -> std::sync::MutexGuard<'_, Vec<JoinHandle<()>>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
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
    use tokio::sync::{Barrier, Semaphore};

    /// A mock sandbox that logs creations and finished removals. Barriers make
    /// creations or removals wait for each other; `removals` holds removals
    /// back until the test hands out permits.
    struct Recording {
        inner: MockSandboxManager,
        log: Mutex<Vec<&'static str>>,
        create_barrier: Option<Barrier>,
        remove_barrier: Option<Barrier>,
        removals: Semaphore,
    }

    impl Recording {
        fn new(dir: &std::path::Path) -> Self {
            Self {
                inner: MockSandboxManager::new(dir.to_path_buf()),
                log: Mutex::new(Vec::new()),
                create_barrier: None,
                remove_barrier: None,
                removals: Semaphore::new(Semaphore::MAX_PERMITS),
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
            self.removals.acquire().await.unwrap().forget();
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

    /// A plan needing more boxes than the host provides is an infrastructure
    /// error, so the whole operation fails with `Err` and the consumer retries
    /// it. A verdict built from half an operation's boxes would blame the
    /// contestant for a host limit.
    #[tokio::test]
    async fn an_operation_needing_more_boxes_than_the_host_has_fails_the_whole_job() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(Recording::new(dir.path()));
        let handler = handler(&sandbox)
            .with_box_ids(super::super::BoxIds::open(dir.path().join("ids"), 1).unwrap());

        let err = handler
            .execute(&two_box_operation())
            .await
            .expect_err("a two-box operation on a one-box host must fail");

        assert!(
            err.to_string().contains("needs 2 sandbox boxes")
                && err.to_string().contains("host provides 1"),
            "{err}"
        );
        assert!(sandbox.log().is_empty(), "no box was created");
    }

    #[tokio::test]
    async fn finishing_removals_leaves_no_sandbox_behind() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(Recording::new(dir.path()));
        let handler = handler(&sandbox);

        assert!(handler.execute(&two_box_operation()).await.unwrap().success);
        handler
            .finish_sandbox_removals(Duration::from_secs(60))
            .await;
        assert_eq!(sandbox.log(), ["create", "create", "removed", "removed"]);
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
        handler.teardown.settle().await;
    }

    #[tokio::test]
    async fn an_operations_sandboxes_are_removed_together() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(Recording {
            remove_barrier: Some(Barrier::new(2)),
            ..Recording::new(dir.path())
        });
        let handler = handler(&sandbox);

        assert!(handler.execute(&two_box_operation()).await.unwrap().success);
        bounded(handler.teardown.settle()).await;
        assert_eq!(sandbox.log(), ["create", "create", "removed", "removed"]);
    }

    #[tokio::test]
    async fn the_result_returns_first_and_the_next_operation_waits_for_the_removal() {
        let dir = tempfile::tempdir().unwrap();
        let sandbox = Arc::new(Recording {
            removals: Semaphore::new(0),
            ..Recording::new(dir.path())
        });
        let handler = handler(&sandbox);
        let operation = two_box_operation();

        // The removal is held back, so the result returning proves it is
        // not on the result's path.
        assert!(handler.execute(&operation).await.unwrap().success);

        let mut next = Box::pin(handler.execute(&operation));
        assert!(futures::poll!(next.as_mut()).is_pending());
        assert_eq!(
            sandbox.log(),
            ["create", "create"],
            "the next operation creates nothing while a removal is pending"
        );

        // A caller that gives up on the next operation mid-wait must not
        // take the pending removal with it.
        drop(next);
        let mut retried = Box::pin(handler.execute(&operation));
        assert!(futures::poll!(retried.as_mut()).is_pending());
        assert_eq!(
            sandbox.log(),
            ["create", "create"],
            "a dropped wait leaves the removal pending for the next operation"
        );

        sandbox.removals.add_permits(4);
        assert!(retried.await.unwrap().success);
        handler.teardown.settle().await;
        assert_eq!(
            sandbox.log(),
            [
                "create", "create", "removed", "removed", "create", "create", "removed", "removed"
            ]
        );
    }
}
