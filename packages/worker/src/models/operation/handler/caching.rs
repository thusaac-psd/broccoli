use super::super::models::{Step, StepCacheConfig, TaskExecutionResult};
use super::super::sandbox::ExecutionResult;
use super::super::task_cache::{TaskCachePutOutcome, compute_cache_key};
use super::metrics::{cached_output_materialization_path_kind, step_kind};
use super::paths::safe_join;
use super::{EnvironmentList, OperationHandler};
use anyhow::{Context, Result};
use opentelemetry::KeyValue;
use std::collections::HashMap;
use std::path::Path;
use tracing::{debug, info, warn};

impl OperationHandler {
    pub(super) async fn ensure_cache_key(
        &self,
        step: &Step,
        environments: &HashMap<String, EnvironmentList>,
        cache_spec: &StepCacheConfig,
        cache_key_cache: &mut Option<String>,
    ) -> Option<String> {
        if let Some(k) = cache_key_cache {
            return Some(k.clone());
        }
        let env = environments.get(&step.env_ref)?;
        match self
            .build_cache_key(&env.working_dir, &step.argv, &cache_spec.key_inputs)
            .await
        {
            Ok(key) => {
                *cache_key_cache = Some(key.clone());
                Some(key)
            }
            Err(e) => {
                debug!(step_id = %step.id, error = %e, "Failed to compute cache key");
                None
            }
        }
    }

    /// Restore cached outputs from `task_cache` outputs map into the env's
    /// working directory. Returns `Some(TaskExecutionResult)` on success, or
    /// `None` if any file restore failed (in which case the caller should
    /// fall through to normal execution).
    async fn restore_cached_outputs(
        &self,
        step: &Step,
        env: &EnvironmentList,
        cached_outputs: HashMap<String, String>,
        cache_key: &str,
    ) -> Option<TaskExecutionResult> {
        for (filename, content_hash) in &cached_outputs {
            let dest = match safe_join(&env.working_dir, filename) {
                Ok(p) => p,
                Err(e) => {
                    warn!(step_id = %step.id, error = %e, "Unsafe cached output path");
                    return None;
                }
            };
            if let Some(parent) = dest.parent()
                && let Err(e) = tokio::fs::create_dir_all(parent).await
            {
                warn!(step_id = %step.id, error = %e, "Failed to create parent dir for cached output");
                return None;
            }
            let materialization_start = std::time::Instant::now();
            if let Err(e) = self.file_cacher.fetch_to_path(content_hash, &dest).await {
                warn!(
                    step_id = %step.id,
                    file = %filename,
                    error = %e,
                    "Failed to restore cached output, falling back to execution"
                );
                return None;
            }
            self.metrics.file_materialization_copy_seconds.record(
                materialization_start.elapsed().as_secs_f64(),
                &[
                    KeyValue::new("source.kind", "blob"),
                    KeyValue::new("path_kind", cached_output_materialization_path_kind()),
                    KeyValue::new("outcome", "success"),
                ],
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Restore the cached compile output as 0o755 (owner rwx + world
                // r-x) - never a mode that lacks the execute bit. fetch_to_path
                // hard-links `dest` to the content-addressed on-disk blob, so in
                // an nproc>=2 op (or under concurrent judging) the SAME binary is
                // shared by one inode across boxes. chmod acts on the inode, not
                // the path: if any op drives that shared inode through a no-exec
                // state (e.g. 0o644) while another box is exec'ing it, the run
                // fails with `execve(...): Permission denied` (exit 127) -> a
                // spurious RuntimeError. Keeping every cache/restore perm
                // execute-inclusive (see file_cacher::ensure_world_readable, also
                // 0o755) keeps the shared inode owner-x at all times, closing the
                // race window. Reproduced at 56% failure with a no-x (0o644)
                // lifecycle; 0% once every perm retains the execute bit.
                if let Err(e) =
                    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
                {
                    warn!(path = %dest.display(), error = %e, "Failed to set permissions on cached output file");
                }
            }
        }

        info!(step_id = %step.id, cache_key = %cache_key, "Cache hit — skipped execution");
        Some(TaskExecutionResult {
            task_id: step.id.clone(),
            success: true,
            sandbox_result: ExecutionResult::default(),
            collected_outputs: cached_outputs,
        })
    }

    pub(super) async fn try_cache_hit(
        &self,
        step: &Step,
        environments: &HashMap<String, EnvironmentList>,
        cache_spec: &StepCacheConfig,
        cache_key_cache: &mut Option<String>,
    ) -> Option<TaskExecutionResult> {
        let env = environments.get(&step.env_ref)?;
        let cache_key = self
            .ensure_cache_key(step, environments, cache_spec, cache_key_cache)
            .await?;

        let cache_start = std::time::Instant::now();
        let cached_outputs = match self.cache.store.get(&cache_key).await {
            Ok(Some(outputs)) => {
                self.record_task_cache_metric(cache_start, "get", "hit");
                outputs
            }
            Ok(None) => {
                self.record_task_cache_metric(cache_start, "get", "miss");
                debug!(step_id = %step.id, cache_key = %cache_key, "Cache miss");
                return None;
            }
            Err(e) => {
                self.record_task_cache_metric(cache_start, "get", "error");
                warn!(step_id = %step.id, error = %e, "Cache lookup failed, executing normally");
                return None;
            }
        };

        self.restore_cached_outputs(step, env, cached_outputs, &cache_key)
            .await
    }

    /// As a follower, repeatedly poll task_cache until we see the leader's
    /// stored entry. Returns `Some(TaskExecutionResult)` if we successfully
    /// restored the cached outputs; `None` on timeout or any failure (caller
    /// then falls back to executing the step itself).
    pub(super) async fn follower_poll_loop(
        &self,
        step: &Step,
        environments: &HashMap<String, EnvironmentList>,
        cache_key: &str,
    ) -> Option<TaskExecutionResult> {
        let env = environments.get(&step.env_ref)?;
        let deadline = tokio::time::Instant::now() + self.cache.follower_max_wait;
        loop {
            tokio::time::sleep(self.cache.follower_poll_interval).await;
            let cache_start = std::time::Instant::now();
            match self.cache.store.get(cache_key).await {
                Ok(Some(outputs)) => {
                    self.record_task_cache_metric(cache_start, "get", "follower_hit");
                    return self
                        .restore_cached_outputs(step, env, outputs, cache_key)
                        .await;
                }
                Ok(None) => {
                    self.record_task_cache_metric(cache_start, "get", "follower_miss");
                }
                Err(e) => {
                    self.record_task_cache_metric(cache_start, "get", "follower_error");
                    warn!(step_id = %step.id, error = %e, "Follower cache poll failed");
                    return None;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
        }
    }

    pub(super) async fn store_in_cache(
        &self,
        step: &Step,
        environments: &HashMap<String, EnvironmentList>,
        cache_spec: &StepCacheConfig,
        existing_hashes: &HashMap<String, String>,
        cache_key_cache: &mut Option<String>,
    ) {
        let env = match environments.get(&step.env_ref) {
            Some(e) => e,
            None => return,
        };

        let cache_key = match self
            .ensure_cache_key(step, environments, cache_spec, cache_key_cache)
            .await
        {
            Some(k) => k,
            None => {
                warn!(step_id = %step.id, "Failed to compute cache key for storage");
                return;
            }
        };

        let mut output_hashes = HashMap::new();
        for filename in &cache_spec.outputs {
            if let Some(hash) = existing_hashes.get(filename) {
                output_hashes.insert(filename.clone(), hash.clone());
                continue;
            }
            let src = match safe_join(&env.working_dir, filename) {
                Ok(p) => p,
                Err(e) => {
                    warn!(step_id = %step.id, error = %e, "Unsafe cache output path");
                    return;
                }
            };
            if !tokio::fs::try_exists(&src).await.unwrap_or(false) {
                debug!(step_id = %step.id, file = %filename, "Cache output file not found, skipping cache store");
                return;
            }
            match self.file_cacher.upload_from_path(&src).await {
                Ok(hash) => {
                    output_hashes.insert(filename.clone(), hash);
                }
                Err(e) => {
                    warn!(step_id = %step.id, file = %filename, error = %e, "Failed to upload output for cache");
                    return;
                }
            }
        }

        let cache_start = std::time::Instant::now();
        match self.cache.store.put(&cache_key, output_hashes).await {
            Ok(TaskCachePutOutcome::Inserted) => {
                self.record_task_cache_metric(cache_start, "put", "success");
                info!(step_id = %step.id, cache_key = %cache_key, "Stored step outputs in task cache");
            }
            Ok(TaskCachePutOutcome::AlreadyExists) => {
                self.record_task_cache_metric(cache_start, "put", "already_exists");
                self.metrics
                    .worker_compile_cache_redundancy_total
                    .add(1, &[KeyValue::new("step.kind", step_kind(step))]);
                info!(step_id = %step.id, cache_key = %cache_key, "Skipped redundant compile-cache store");
            }
            Err(e) => {
                self.record_task_cache_metric(cache_start, "put", "error");
                warn!(step_id = %step.id, error = %e, "Failed to store task cache entry");
            }
        }
    }

    async fn build_cache_key(
        &self,
        working_dir: &Path,
        argv: &[String],
        key_inputs: &[String],
    ) -> Result<String> {
        let fingerprint = self
            .cache
            .toolchain_fingerprint
            .cache_key_part()
            .context("Toolchain fingerprint is unreliable; compile cache disabled")?;
        let mut input_files = Vec::new();
        for filename in key_inputs {
            let path = safe_join(working_dir, filename)?;
            let content = tokio::fs::read(&path)
                .await
                .with_context(|| format!("Failed to read cache key input: {}", path.display()))?;
            input_files.push((filename.clone(), content));
        }
        Ok(compute_cache_key(fingerprint, argv, &input_files))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::operation::file_cacher::NoopFileCacher;
    use crate::models::operation::sandbox::mock::MockSandboxManager;
    use crate::models::operation::task_cache::NoopTaskCacheStore;
    use crate::toolchain_fingerprint::ToolchainFingerprint;

    fn handler(fingerprint: ToolchainFingerprint) -> OperationHandler {
        let (metrics, _provider) =
            common::observability::init_metrics_local("broccoli-worker-test");
        OperationHandler::new(
            Box::new(MockSandboxManager::new(std::env::temp_dir())),
            Box::new(NoopFileCacher),
            Box::new(NoopTaskCacheStore),
            fingerprint,
            metrics,
        )
    }

    #[tokio::test]
    async fn an_unreliable_toolchain_fingerprint_builds_no_cache_key() {
        let argv = vec!["g++".to_string()];
        let dir = std::env::temp_dir();
        assert!(
            handler(ToolchainFingerprint::Unreliable)
                .build_cache_key(&dir, &argv, &[])
                .await
                .is_err()
        );
        assert!(
            handler(ToolchainFingerprint::Known("fp".into()))
                .build_cache_key(&dir, &argv, &[])
                .await
                .is_ok()
        );
    }
}
