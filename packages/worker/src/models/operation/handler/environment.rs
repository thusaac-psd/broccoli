use super::super::models::{IOConfig, IOTarget, SessionFile, Step};
use super::super::sandbox::ExecutionResult;
use super::OperationHandler;
use super::metrics::{materialization_path_kind, session_file_kind};
use super::paths::{create_fifo, safe_join, validate_pipe_name};
use anyhow::{Context, Result, anyhow};
use std::collections::{HashMap, HashSet};
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use tracing::{debug, info, instrument, warn};

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
                // resolve it inside the box.
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

                if let Err(e) = create_fifo(&pipe_path) {
                    if let Ok(meta) = tokio::fs::metadata(&pipe_path).await
                        && meta.file_type().is_fifo()
                    {
                        return Ok(Some(pipe_path));
                    }
                    return Err(anyhow!("mkfifo failed for {}: {e}", pipe_path.display()));
                }

                Ok(Some(pipe_path))
            }
        }
    }

    /// Upload each of `step.collect` and return its blob hash by name.
    ///
    /// A file that is the step's own stdout or stderr redirect, and whose
    /// exact bytes `result` already carries inline, is not uploaded and gets
    /// no hash: the inline text is its content. A larger or non-text redirect
    /// (one the inline preview truncates or rewrites) is uploaded as usual.
    pub(super) async fn collect_output(
        &self,
        working_dir: &Path,
        step: &Step,
        result: &ExecutionResult,
    ) -> Result<HashMap<String, String>> {
        let mut collected = HashMap::new();
        let real_base = tokio::fs::canonicalize(working_dir)
            .await
            .unwrap_or_else(|_| working_dir.to_path_buf());
        for file_path in &step.collect {
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
                if let Some(inline) = inline_copy(step, result, file_path)
                    && holds_exactly(&real, inline).await
                {
                    debug!(file = %file_path, "Collected output travels inline");
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
}

/// The inline text `result` carries for `file_path`, when that file is the
/// step's stdout or stderr redirect target.
fn inline_copy<'a>(step: &Step, result: &'a ExecutionResult, file_path: &str) -> Option<&'a str> {
    let redirects =
        |target: &IOTarget| matches!(target, IOTarget::File { path } if path == file_path);
    if redirects(&step.io.stdout) {
        Some(&result.stdout)
    } else if redirects(&step.io.stderr) {
        Some(&result.stderr)
    } else {
        None
    }
}

/// Whether the file at `path` holds exactly the bytes of `text`. The length is
/// compared first, so a file larger than the inline preview is never read.
async fn holds_exactly(path: &Path, text: &str) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(meta) if meta.len() == text.len() as u64 => tokio::fs::read(path)
            .await
            .is_ok_and(|bytes| bytes == text.as_bytes()),
        _ => false,
    }
}
