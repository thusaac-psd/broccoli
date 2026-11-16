use super::super::models::{IOConfig, IOTarget, SessionFile};
use super::metrics::{materialization_path_kind, session_file_kind};
use super::paths::{safe_join, validate_pipe_name};
use super::{EnvironmentList, OperationHandler};
use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, instrument, warn};

impl OperationHandler {
    pub(super) async fn create_sandbox(&self, box_id: &str) -> Result<PathBuf> {
        let sandbox_path = self
            .sandbox_manager
            .create_sandbox(Some(box_id))
            .await
            .context("Sandbox creation failed")?;
        Ok(sandbox_path)
    }

    #[instrument(skip(self, files), fields(file_count = files.len()))]
    pub(super) async fn load_environment_files(
        &self,
        working_dir: &Path,
        files: &[(String, SessionFile)],
    ) -> Result<()> {
        for (target_path, source) in files {
            let start = std::time::Instant::now();
            let source_kind = session_file_kind(source);
            let path_kind = materialization_path_kind(target_path, source);
            let dest = safe_join(working_dir, target_path)?;
            if let Some(parent) = dest.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .context("Failed to create parent directory")?;
            }
            match source {
                SessionFile::Path { path: src } => {
                    let bytes = tokio::fs::copy(src, &dest).await.with_context(|| {
                        format!("Failed to copy file {} -> {}", src, dest.display())
                    })?;
                    self.record_file_materialization(
                        start,
                        source_kind,
                        path_kind,
                        "success",
                        bytes,
                    );
                }
                SessionFile::Content { content } => {
                    tokio::fs::write(&dest, content).await.with_context(|| {
                        format!("Failed to write content to {}", dest.display())
                    })?;
                    self.record_file_materialization(
                        start,
                        source_kind,
                        path_kind,
                        "success",
                        content.len() as u64,
                    );
                }
                SessionFile::Blob { hash: content_hash } => {
                    self.file_cacher
                        .fetch_to_path(content_hash, &dest)
                        .await
                        .map_err(|e| anyhow!("Failed to fetch blob {}: {}", content_hash, e))?;
                    let bytes = tokio::fs::metadata(&dest)
                        .await
                        .map(|m| m.len())
                        .unwrap_or(0);
                    self.record_file_materialization(
                        start,
                        source_kind,
                        path_kind,
                        "success",
                        bytes,
                    );
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        // 0o755 (world r-x), not 0o700: isolate runs the step as a
                        // per-box sandbox uid (first_uid + box_id), which does NOT
                        // own this worker-written file. A loaded source/input at
                        // 0o700 is unreadable to that uid -> the compiler (run as the
                        // sandbox uid) fails to open it. The execute bit is harmless
                        // for data files and required for any loaded executable.
                        // Matches restore_cached_outputs and file_cacher, all 0o755.
                        if let Err(e) =
                            std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
                        {
                            warn!(path = %dest.display(), error = %e, "Failed to set permissions on blob file");
                        }
                    }
                }
            }
            debug!(target = %target_path, dest = %dest.display(), "Loaded environment file");
        }
        Ok(())
    }

    pub(super) async fn prepare_io(
        &self,
        working_dir: &Path,
        io_config: &IOConfig,
        shared_channels_dir: Option<&Path>,
        channel_names: &HashSet<String>,
    ) -> Result<(Option<PathBuf>, Option<PathBuf>, Option<PathBuf>)> {
        let stdin = self
            .prepare_io_target(
                working_dir,
                &io_config.stdin,
                shared_channels_dir,
                channel_names,
            )
            .await?;
        let stdout = self
            .prepare_io_target(
                working_dir,
                &io_config.stdout,
                shared_channels_dir,
                channel_names,
            )
            .await?;
        let stderr = self
            .prepare_io_target(
                working_dir,
                &io_config.stderr,
                shared_channels_dir,
                channel_names,
            )
            .await?;

        Ok((stdin, stdout, stderr))
    }

    async fn prepare_io_target(
        &self,
        working_dir: &Path,
        target: &IOTarget,
        shared_channels_dir: Option<&Path>,
        channel_names: &HashSet<String>,
    ) -> Result<Option<PathBuf>> {
        match target {
            IOTarget::Null | IOTarget::Inherit => Ok(None),
            IOTarget::File { path } => {
                // Validate the redirect target is a safe box-relative path (no
                // absolute root, no `..`). Without this, an op-supplied
                // `io.stdout = File{path:"/etc/passwd"}` (or "../..") reads or
                // overwrites a host file outside the box -- under the mock
                // backend `resolve_runtime_path` joins it (an absolute path
                // replaces the base and escapes), and isolate's `--stdin`/etc.
                // must stay box-relative too. `safe_join` errors on any unsafe
                // component; we keep the original relative path so both backends
                // resolve it inside the box exactly as before.
                safe_join(working_dir, path)?;
                Ok(Some(Path::new(path).to_path_buf()))
            }
            IOTarget::Pipe { name } => {
                validate_pipe_name(name)?;

                if channel_names.contains(name) {
                    shared_channels_dir.ok_or_else(|| {
                        anyhow!(
                            "Pipe '{}' references a channel but no channels directory exists",
                            name
                        )
                    })?;
                    let fifo_path = PathBuf::from("/channels").join(name);
                    return Ok(Some(fifo_path));
                }

                let pipes_dir = working_dir.join("pipes");
                tokio::fs::create_dir_all(&pipes_dir)
                    .await
                    .context("Failed to create pipes directory")?;

                let pipe_path = pipes_dir.join(name);

                if let Ok(_meta) = tokio::fs::metadata(&pipe_path).await {
                    #[cfg(unix)]
                    if !_meta.file_type().is_fifo() {
                        return Err(anyhow!(
                            "Pipe target exists but is not a FIFO: {}",
                            pipe_path.display()
                        ));
                    }
                    return Ok(Some(pipe_path));
                }

                let output = tokio::process::Command::new("mkfifo")
                    .arg(&pipe_path)
                    .output()
                    .await
                    .context("Failed to execute mkfifo")?;

                if !output.status.success() {
                    if let Ok(meta) = tokio::fs::metadata(&pipe_path).await
                        && meta.file_type().is_fifo()
                    {
                        return Ok(Some(pipe_path));
                    }
                    return Err(anyhow!(
                        "mkfifo failed for {}: {}",
                        pipe_path.display(),
                        String::from_utf8_lossy(&output.stderr).trim()
                    ));
                }

                Ok(Some(pipe_path))
            }
        }
    }

    pub(super) async fn collect_output(
        &self,
        working_dir: &Path,
        collect_files: &[String],
    ) -> Result<HashMap<String, String>> {
        let mut collected = HashMap::new();
        let real_base = tokio::fs::canonicalize(working_dir)
            .await
            .unwrap_or_else(|_| working_dir.to_path_buf());
        for file_path in collect_files {
            let src = safe_join(working_dir, file_path)?;
            if tokio::fs::try_exists(&src).await.unwrap_or(false) {
                // `safe_join` only validates the path STRING. The sandboxed
                // program can plant a symlink at the collect path (or an
                // intermediate directory) pointing at a host file, and
                // `upload_from_path` follows symlinks. Resolve the real target
                // and confirm it is still inside the box before uploading, so a
                // contestant cannot exfiltrate a host secret (or substitute the
                // answer) by symlinking their output file.
                let real = match tokio::fs::canonicalize(&src).await {
                    Ok(real) => real,
                    Err(e) => {
                        warn!(path = %src.display(), error = %e, "Could not resolve collect target; skipping");
                        continue;
                    }
                };
                if !real.starts_with(&real_base) {
                    warn!(
                        path = %src.display(),
                        resolved = %real.display(),
                        "Collect target resolves outside the box (symlink escape); skipping"
                    );
                    continue;
                }
                let hash = self
                    .file_cacher
                    .upload_from_path(&real)
                    .await
                    .map_err(|e| {
                        anyhow!("Failed to upload output file {}: {}", real.display(), e)
                    })?;
                info!(
                    file = %file_path,
                    hash = %hash,
                    "Collected output file"
                );
                collected.insert(file_path.clone(), hash);
            } else {
                warn!(path = %src.display(), "Collect target not found, skipping");
            }
        }
        Ok(collected)
    }

    /// Remove every environment's sandbox, logging failures.
    pub(super) async fn cleanup_environments(
        &self,
        environments: &HashMap<String, EnvironmentList>,
    ) {
        for env in environments.values() {
            if let Err(e) = self
                .sandbox_manager
                .remove_sandbox(env.box_id.as_str())
                .await
            {
                error!(env_id = %env.id, error = %e, "Failed to cleanup sandbox");
            }
        }
    }
}
