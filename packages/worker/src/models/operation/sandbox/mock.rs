use super::capture::{INLINE_OUTPUT_PREVIEW_BYTES, read_text_preview, text_preview_from_bytes};
use super::error::SandboxError;
use super::{DirectoryRule, ExecutionResult, RunOptions, SandboxManager, SandboxStatus};
use async_trait::async_trait;
use std::collections::HashMap;
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::RwLock;
use tokio::time::Instant;
use tracing::{debug, warn};

/// Drain a child pipe to EOF while keeping only the capped preview. Reading to
/// the end (rather than stopping at the cap) keeps the child from taking SIGPIPE
/// when it writes more than the preview limit.
async fn read_capped_drain<R>(mut reader: R) -> Vec<u8>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut preview = Vec::with_capacity(INLINE_OUTPUT_PREVIEW_BYTES + 1);
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if preview.len() <= INLINE_OUTPUT_PREVIEW_BYTES {
                    let remaining = (INLINE_OUTPUT_PREVIEW_BYTES + 1).saturating_sub(preview.len());
                    preview.extend_from_slice(&chunk[..remaining.min(read)]);
                }
            }
        }
    }
    preview
}

#[derive(Debug, Clone)]
pub struct MockSandboxManager {
    base_dir: PathBuf,
    sandboxes: Arc<RwLock<HashMap<String, PathBuf>>>,
}

impl Default for MockSandboxManager {
    fn default() -> Self {
        // Scope the base directory by PID so concurrent worker processes don't
        // race on the same `box_id` directory under a shared temp root.
        let pid = std::process::id();
        Self::new(std::env::temp_dir().join(format!("broccoli-mock-sandbox-{pid}")))
    }
}

impl MockSandboxManager {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            sandboxes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn get_sandbox_path(&self, box_id: &str) -> Result<PathBuf, SandboxError> {
        if let Some(path) = self.sandboxes.read().await.get(box_id).cloned() {
            return Ok(path);
        }

        let fallback = self.base_dir.join(box_id);
        if tokio::fs::try_exists(&fallback).await.map_err(|err| {
            SandboxError::Execution(format!("failed to inspect sandbox path: {err}"))
        })? {
            return Ok(fallback);
        }

        Err(SandboxError::Execution(format!(
            "mock sandbox not found for box id: {box_id}"
        )))
    }

    fn open_stdin(path: &PathBuf) -> Result<std::fs::File, SandboxError> {
        if Self::is_fifo(path) {
            // The child's stdin must be READ-ONLY: a FIFO delivers EOF only once
            // every write end closes, so if the consumer held a write end of its
            // own stdin (O_RDWR) it would never see EOF and an EOF-draining
            // checker would hang forever. We can't use a plain blocking O_RDONLY
            // open here - opening a FIFO read-only blocks until a writer appears,
            // which would stall this async task before the producer step runs.
            // Instead open O_RDONLY | O_NONBLOCK (returns immediately even with no
            // writer yet) and then clear O_NONBLOCK so the child's reads block
            // normally and observe EOF when the producer closes the write end.
            // This mirrors the real sandbox, whose children also get read-only
            // stdin.
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                use std::os::unix::io::AsRawFd;

                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(path)
                    .map_err(|err| {
                        SandboxError::Execution(format!(
                            "failed to open stdin pipe {}: {err}",
                            path.display()
                        ))
                    })?;

                let fd = file.as_raw_fd();
                // SAFETY: `fd` is a valid open descriptor owned by `file` for the
                // duration of these calls.
                let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
                if flags < 0 {
                    return Err(SandboxError::Execution(format!(
                        "failed to read flags for stdin pipe {}: {}",
                        path.display(),
                        std::io::Error::last_os_error()
                    )));
                }
                let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
                if rc < 0 {
                    return Err(SandboxError::Execution(format!(
                        "failed to clear O_NONBLOCK for stdin pipe {}: {}",
                        path.display(),
                        std::io::Error::last_os_error()
                    )));
                }
                return Ok(file);
            }
            #[cfg(not(unix))]
            {
                return OpenOptions::new().read(true).open(path).map_err(|err| {
                    SandboxError::Execution(format!(
                        "failed to open stdin pipe {}: {err}",
                        path.display()
                    ))
                });
            }
        }

        std::fs::File::open(path).map_err(|err| {
            SandboxError::Execution(format!(
                "failed to open stdin file {}: {err}",
                path.display()
            ))
        })
    }

    fn open_stdout_stderr(
        path: &PathBuf,
        stream_name: &str,
    ) -> Result<std::fs::File, SandboxError> {
        if Self::is_fifo(path) {
            return OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|err| {
                    SandboxError::Execution(format!(
                        "failed to open {stream_name} pipe {}: {err}",
                        path.display()
                    ))
                });
        }

        std::fs::File::create(path).map_err(|err| {
            SandboxError::Execution(format!(
                "failed to create {stream_name} file {}: {err}",
                path.display()
            ))
        })
    }

    fn is_fifo(path: &PathBuf) -> bool {
        #[cfg(unix)]
        {
            std::fs::metadata(path)
                .map(|m| m.file_type().is_fifo())
                .unwrap_or(false)
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            false
        }
    }

    fn resolve_inside_path(base: &Path, inside: &Path) -> Result<PathBuf, SandboxError> {
        let mut resolved = base.to_path_buf();
        for component in inside.components() {
            match component {
                Component::RootDir | Component::CurDir => {}
                Component::Normal(part) => resolved.push(part),
                Component::ParentDir => {
                    return Err(SandboxError::Initialization(format!(
                        "invalid inside_path with parent traversal: {}",
                        inside.display()
                    )));
                }
                Component::Prefix(_) => {
                    return Err(SandboxError::Initialization(format!(
                        "unsupported inside_path prefix: {}",
                        inside.display()
                    )));
                }
            }
        }
        Ok(resolved)
    }

    fn resolve_runtime_path(sandbox_path: &Path, path: &Path) -> PathBuf {
        if let Ok(stripped) = path.strip_prefix("/channels") {
            sandbox_path.join("channels").join(stripped)
        } else if let Ok(stripped) = path.strip_prefix("/box") {
            sandbox_path.join(stripped)
        } else if path.is_absolute() {
            path.to_path_buf()
        } else {
            sandbox_path.join(path)
        }
    }

    async fn remove_existing_target(path: &Path) -> Result<(), SandboxError> {
        if !fs::try_exists(path).await.map_err(|err| {
            SandboxError::Initialization(format!("failed to inspect mapped path: {err}"))
        })? {
            return Ok(());
        }

        let metadata = fs::symlink_metadata(path).await.map_err(|err| {
            SandboxError::Initialization(format!("failed to inspect mapped metadata: {err}"))
        })?;

        if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(path).await.map_err(|err| {
                SandboxError::Initialization(format!("failed to remove mapped directory: {err}"))
            })
        } else {
            fs::remove_file(path).await.map_err(|err| {
                SandboxError::Initialization(format!("failed to remove mapped file: {err}"))
            })
        }
    }

    async fn apply_directory_rule(
        sandbox_path: &Path,
        rule: &DirectoryRule,
    ) -> Result<(), SandboxError> {
        let inside_path = Self::resolve_inside_path(sandbox_path, &rule.inside_path)?;

        if let Some(parent) = inside_path.parent() {
            fs::create_dir_all(parent).await.map_err(|err| {
                SandboxError::Initialization(format!(
                    "failed to create parent directory for mapping {}: {err}",
                    parent.display()
                ))
            })?;
        }

        match &rule.outside_path {
            Some(outside_path) => {
                if !fs::try_exists(outside_path).await.map_err(|err| {
                    SandboxError::Initialization(format!(
                        "failed to inspect outside_path {}: {err}",
                        outside_path.display()
                    ))
                })? {
                    fs::create_dir_all(outside_path).await.map_err(|err| {
                        SandboxError::Initialization(format!(
                            "failed to create outside_path {}: {err}",
                            outside_path.display()
                        ))
                    })?;
                }

                Self::remove_existing_target(&inside_path).await?;

                #[cfg(unix)]
                match std::os::unix::fs::symlink(outside_path, &inside_path) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                        if let Ok(target) = std::fs::read_link(&inside_path)
                            && target != *outside_path
                        {
                            return Err(SandboxError::Initialization(format!(
                                "mock directory mapping {} exists but points to {}, expected {}",
                                inside_path.display(),
                                target.display(),
                                outside_path.display()
                            )));
                        }
                    }
                    Err(err) => {
                        return Err(SandboxError::Initialization(format!(
                            "failed to create mock directory mapping {} -> {}: {err}",
                            inside_path.display(),
                            outside_path.display()
                        )));
                    }
                }
                #[cfg(not(unix))]
                std::os::windows::fs::symlink_dir(outside_path, &inside_path).map_err(|err| {
                    SandboxError::Initialization(format!(
                        "failed to create mock directory mapping {} -> {}: {err}",
                        inside_path.display(),
                        outside_path.display()
                    ))
                })?;
            }
            None => {
                fs::create_dir_all(&inside_path).await.map_err(|err| {
                    SandboxError::Initialization(format!(
                        "failed to create inside_path {}: {err}",
                        inside_path.display()
                    ))
                })?;
            }
        }

        Ok(())
    }

    async fn apply_directory_rules(
        sandbox_path: &Path,
        directory_rules: &[DirectoryRule],
    ) -> Result<(), SandboxError> {
        for rule in directory_rules {
            Self::apply_directory_rule(sandbox_path, rule).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl SandboxManager for MockSandboxManager {
    async fn create_sandbox(&self, id: Option<&str>) -> Result<PathBuf, SandboxError> {
        let box_id = id.unwrap_or("0").to_string();
        if box_id.is_empty() {
            return Err(SandboxError::Initialization(
                "mock sandbox box id cannot be empty".to_string(),
            ));
        }

        let sandbox_path = self.base_dir.join(&box_id);
        debug!(box_id = %box_id, path = %sandbox_path.display(), "Creating mock sandbox");

        if tokio::fs::try_exists(&sandbox_path).await.map_err(|err| {
            SandboxError::Initialization(format!("failed to inspect mock sandbox path: {err}"))
        })? {
            tokio::fs::remove_dir_all(&sandbox_path)
                .await
                .map_err(|err| {
                    SandboxError::Initialization(format!(
                        "failed to reset mock sandbox directory: {err}"
                    ))
                })?;
        }

        tokio::fs::create_dir_all(&sandbox_path)
            .await
            .map_err(|err| {
                SandboxError::Initialization(format!(
                    "failed to create mock sandbox directory: {err}"
                ))
            })?;

        self.sandboxes
            .write()
            .await
            .insert(box_id, sandbox_path.clone());

        Ok(sandbox_path)
    }

    async fn remove_sandbox(&self, id: &str) -> Result<(), SandboxError> {
        let known_path = self.sandboxes.write().await.remove(id);
        let sandbox_path = known_path.unwrap_or_else(|| self.base_dir.join(id));
        debug!(box_id = %id, path = %sandbox_path.display(), "Removing mock sandbox");

        if tokio::fs::try_exists(&sandbox_path).await.map_err(|err| {
            SandboxError::Execution(format!("failed to inspect mock sandbox path: {err}"))
        })? {
            tokio::fs::remove_dir_all(&sandbox_path)
                .await
                .map_err(|err| {
                    SandboxError::Execution(format!("failed to remove mock sandbox: {err}"))
                })?;
        }

        Ok(())
    }

    async fn execute(
        &self,
        box_id: &str,
        argv: Vec<String>,
        run_options: &RunOptions,
    ) -> Result<ExecutionResult, SandboxError> {
        if argv.is_empty() {
            return Err(SandboxError::Execution(
                "mock sandbox requires at least one argv element".to_string(),
            ));
        }

        let sandbox_path = self.get_sandbox_path(box_id).await?;
        debug!(box_id = %box_id, argv = ?argv, cwd = %sandbox_path.display(), "Executing command in mock sandbox");

        let rewritten_argv: Vec<String> = argv
            .iter()
            .map(|arg| {
                if let Some(rest) = arg.strip_prefix("/box/") {
                    sandbox_path.join(rest).to_string_lossy().into_owned()
                } else if arg == "/box" {
                    sandbox_path.to_string_lossy().into_owned()
                } else {
                    arg.clone()
                }
            })
            .collect();

        let mut command = Command::new(&rewritten_argv[0]);
        command.args(rewritten_argv.iter().skip(1));
        command.current_dir(&sandbox_path);

        Self::apply_directory_rules(&sandbox_path, &run_options.directory_rules).await?;

        if let Some(stdin_path) = &run_options.stdin {
            let stdin_path = Self::resolve_runtime_path(&sandbox_path, stdin_path);
            let stdin_file = Self::open_stdin(&stdin_path)?;
            command.stdin(Stdio::from(stdin_file));
        }

        if let Some(stdout_path) = &run_options.stdout {
            let stdout_path = Self::resolve_runtime_path(&sandbox_path, stdout_path);
            if let Some(parent) = stdout_path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|err| {
                    SandboxError::Execution(format!(
                        "failed to create stdout directory {}: {err}",
                        parent.display()
                    ))
                })?;
            }
            let stdout_file = Self::open_stdout_stderr(&stdout_path, "stdout")?;
            command.stdout(Stdio::from(stdout_file));
        } else {
            command.stdout(Stdio::piped());
        }

        if let Some(stderr_path) = &run_options.stderr {
            let stderr_path = Self::resolve_runtime_path(&sandbox_path, stderr_path);
            if let Some(parent) = stderr_path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|err| {
                    SandboxError::Execution(format!(
                        "failed to create stderr directory {}: {err}",
                        parent.display()
                    ))
                })?;
            }
            let stderr_file = Self::open_stdout_stderr(&stderr_path, "stderr")?;
            command.stderr(Stdio::from(stderr_file));
        } else {
            command.stderr(Stdio::piped());
        }

        let start = Instant::now();
        let mut child = command.spawn().map_err(|err| {
            SandboxError::Execution(format!("failed to spawn mock sandbox process: {err}"))
        })?;

        let piped_stdout_handle = child.stdout.take();
        let piped_stderr_handle = child.stderr.take();

        let stdout_task = piped_stdout_handle.map(|s| tokio::spawn(read_capped_drain(s)));
        let stderr_task = piped_stderr_handle.map(|s| tokio::spawn(read_capped_drain(s)));

        let time_limit_secs = run_options.resource_limits.wall_time_limit.or_else(|| {
            run_options
                .resource_limits
                .time_limit
                .map(|t| (t * 1.5).max(t + 5.0))
        });

        let (timed_out, exit_status) = if let Some(limit) = time_limit_secs {
            let timeout_dur = Duration::from_secs_f64(limit);
            tokio::select! {
                result = child.wait() => {
                    let status = result.map_err(|err| {
                        SandboxError::Execution(format!("failed to wait mock sandbox process: {err}"))
                    })?;
                    (false, status)
                }
                _ = tokio::time::sleep(timeout_dur) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    (true, std::process::ExitStatus::default())
                }
            }
        } else {
            let status = child.wait().await.map_err(|err| {
                SandboxError::Execution(format!("failed to wait mock sandbox process: {err}"))
            })?;
            (false, status)
        };

        let elapsed = start.elapsed().as_secs_f64();

        let piped_stdout_bytes = if let Some(task) = stdout_task {
            task.await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let piped_stderr_bytes = if let Some(task) = stderr_task {
            task.await.unwrap_or_default()
        } else {
            Vec::new()
        };

        let stdout = if let Some(path) = &run_options.stdout {
            let resolved = Self::resolve_runtime_path(&sandbox_path, path);
            if Self::is_fifo(&resolved) {
                String::new()
            } else {
                read_text_preview(&resolved)
                    .await
                    .unwrap_or_else(|_| String::new())
            }
        } else {
            text_preview_from_bytes(piped_stdout_bytes, false)
        };

        let stderr = if let Some(path) = &run_options.stderr {
            let resolved = Self::resolve_runtime_path(&sandbox_path, path);
            if Self::is_fifo(&resolved) {
                String::new()
            } else {
                read_text_preview(&resolved)
                    .await
                    .unwrap_or_else(|_| String::new())
            }
        } else {
            text_preview_from_bytes(piped_stderr_bytes, false)
        };

        let exit_code = if timed_out { None } else { exit_status.code() };
        #[cfg(unix)]
        let signal = if timed_out {
            None
        } else {
            exit_status.signal()
        };
        #[cfg(not(unix))]
        let signal: Option<i32> = None;
        let success = !timed_out && exit_status.success();

        if timed_out {
            warn!(box_id = %box_id, time_used = elapsed, "Mock sandbox process killed (wall-time limit exceeded)");
        } else if success {
            debug!(box_id = %box_id, exit_code = ?exit_code, time_used = elapsed, "Mock sandbox command finished");
        } else {
            warn!(box_id = %box_id, exit_code = ?exit_code, signal = ?signal, time_used = elapsed, "Mock sandbox command failed");
        }

        let status = if timed_out {
            "TO".to_string()
        } else if success {
            "OK".to_string()
        } else if signal.is_some() {
            "SG".to_string()
        } else {
            "RE".to_string()
        };

        Ok(ExecutionResult {
            exit_code,
            signal,
            time_used: elapsed,
            wall_time_used: elapsed,
            memory_used: None,
            killed: timed_out || signal.is_some(),
            cg_oom_killed: false,
            sandbox_status: SandboxStatus::from_isolate(&status),
            status,
            message: if timed_out {
                "wall-time limit exceeded".to_string()
            } else if success {
                String::new()
            } else {
                format!("mock sandbox process exited with status: {}", exit_status)
            },
            stdout,
            stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mock backend captures output through the shared preview helper,
    /// byte for byte: `printf 'a\0b'` emits a literal NUL and it is kept.
    #[cfg(unix)]
    #[tokio::test]
    async fn mock_piped_stdout_keeps_nul_bytes() {
        let manager = MockSandboxManager::new(std::env::temp_dir().join(format!(
            "broccoli-mock-sandbox-nul-test-{}",
            std::process::id()
        )));
        manager.create_sandbox(Some("nul-test")).await.unwrap();

        let result = manager
            .execute(
                "nul-test",
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "printf 'a\\0b'".to_string(),
                ],
                &RunOptions::default(),
            )
            .await
            .unwrap();

        manager.remove_sandbox("nul-test").await.unwrap();

        assert_eq!(result.stdout, "a\0b");
    }
}
