use super::capture::{INLINE_OUTPUT_PREVIEW_BYTES, read_text_preview, text_preview_from_bytes};
use super::error::SandboxError;
use super::{
    DirectoryOptions, DirectoryRule, EnvRule, ExecutionResult, ResourceLimits, RunOptions,
    SandboxManager, SandboxStatus,
};
use async_trait::async_trait;
use common::metrics::Metrics;
use opentelemetry::KeyValue;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
};
use tokio::fs;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::RwLock;

pub struct IsolateSandboxManager {
    isolate_bin: String,
    enable_cgroups: bool,
    sandboxes: Arc<RwLock<HashMap<String, PathBuf>>>,
    metrics: Option<Metrics>,
    /// Per-box cumulative CPU seconds consumed by steps that actually ran in the
    /// box (cache-hit steps never reach `execute`, so they do not contribute).
    ///
    /// isolate runs every step of an environment in ONE box (one `--init`), and
    /// with `--cg` the reported `time` and the `--time` limit are the box
    /// cgroup's CPU usage cumulative *since `--init`*. So a later step (e.g.
    /// `exec`) inherits an earlier step's CPU (e.g. `compile`) - charging compile
    /// time against the run-time limit and false-TLEing legitimate solutions.
    /// We offset each step's `--time` by this prior cumulative and report only
    /// the step's own delta. Reset on `create_sandbox`, dropped on
    /// `remove_sandbox`.
    box_cpu_secs: Arc<Mutex<HashMap<String, f64>>>,
}

impl std::fmt::Debug for IsolateSandboxManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IsolateSandboxManager")
            .field("isolate_bin", &self.isolate_bin)
            .field("enable_cgroups", &self.enable_cgroups)
            .finish_non_exhaustive()
    }
}

impl IsolateSandboxManager {
    pub fn new(isolate_bin: String, enable_cgroups: bool) -> Self {
        Self::new_with_metrics(isolate_bin, enable_cgroups, None)
    }

    pub fn new_with_metrics(
        isolate_bin: String,
        enable_cgroups: bool,
        metrics: Option<Metrics>,
    ) -> Self {
        Self {
            isolate_bin,
            enable_cgroups,
            sandboxes: Arc::new(RwLock::new(HashMap::new())),
            metrics,
            box_cpu_secs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn metric_attrs(&self, outcome: &'static str) -> [KeyValue; 2] {
        [
            KeyValue::new("enable_cgroups", self.enable_cgroups.to_string()),
            KeyValue::new("outcome", outcome),
        ]
    }
}

impl Default for IsolateSandboxManager {
    /// Config-free defaults (`isolate` binary, cgroups off) for tests. Production
    /// injects real settings via `sandbox_manager_from_config`. This deliberately
    /// does NOT read `WorkerAppConfig::load()` - the sandbox layer must not reach
    /// up to global app config.
    fn default() -> Self {
        Self::new("isolate".to_string(), false)
    }
}

fn is_fifo(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
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

/// Whether a declared `--stdout`/`--stderr` redirect target is a box-local file
/// whose data the worker reads back from the host after the run.
///
/// isolate creates box-local redirect files (a box-relative path such as
/// `output.txt`) inside `box_dir` before exec, so after a clean exit their
/// absence is a genuine infra fault -> retryable [`SandboxStatus::InternalError`].
///
/// An ABSOLUTE redirect target is an in-box path into a bind-mounted directory -
/// e.g. a channel FIFO at `/channels/<name>` (see `prepare_io_target`). Its data
/// is streamed to the consumer (the fused check step reads the FIFO directly);
/// there is nothing for the worker to read back. Crucially, `Path::join` drops
/// the base for an absolute argument, so `box_dir.join("/channels/x")` resolves
/// to the HOST path `/channels/x`, which never exists on the host - reading it
/// back would ALWAYS "fail". Keying on box-locality (the resolved path staying
/// under `box_dir`) rather than a hardcoded `/channels` prefix stays correct for
/// any bind-mounted redirect target, and treats streamed targets as empty (never
/// a fault). Without this guard every channel-redirected step is misflagged as an
/// infra failure.
///
/// The `starts_with` predicate is component-wise, not canonicalizing, so it would
/// misjudge a `..`-escaping box-relative path. That input is unreachable here:
/// `prepare_io_target` runs `safe_join` on every `File` redirect target and
/// rejects any absolute or `..` component before it becomes a redirect path, and
/// pipe/channel targets are absolute. Were such a path to slip through it still
/// fails safe - it resolves off-box, the read-back finds nothing, and the step
/// retries as a SystemError rather than silently scoring empty output.
fn is_box_local_redirect(box_dir: &Path, declared: &Path) -> bool {
    box_dir.join(declared).starts_with(box_dir)
}

async fn read_capped_child_pipe<R>(mut reader: R) -> Result<Vec<u8>, std::io::Error>
where
    R: AsyncRead + Unpin,
{
    let mut preview = Vec::with_capacity(INLINE_OUTPUT_PREVIEW_BYTES + 1);
    let mut chunk = [0_u8; 8192];

    loop {
        let read = reader.read(&mut chunk).await?;
        if read == 0 {
            break;
        }

        if preview.len() <= INLINE_OUTPUT_PREVIEW_BYTES {
            let remaining = (INLINE_OUTPUT_PREVIEW_BYTES + 1).saturating_sub(preview.len());
            let keep = remaining.min(read);
            preview.extend_from_slice(&chunk[..keep]);
        }
    }

    Ok(preview)
}

async fn join_pipe_capture(
    handle: tokio::task::JoinHandle<Result<Vec<u8>, std::io::Error>>,
    stream_name: &str,
) -> Result<Vec<u8>, SandboxError> {
    handle
        .await
        .map_err(|err| {
            SandboxError::Execution(format!(
                "failed to join isolate {stream_name} reader: {err}"
            ))
        })?
        .map_err(|err| {
            SandboxError::Execution(format!("failed to read isolate {stream_name}: {err}"))
        })
}

/// Read a declared, non-FIFO redirect file back after a run.
///
/// isolate creates a `--stdout`/`--stderr` redirect file before exec, so after a
/// clean (exit 0/1) run the file MUST exist. The two outcomes the caller treats
/// differently:
/// - `Ok(text)` - file present (possibly empty: a legitimate empty program
///   output, which must stay a normal verdict, never a retry).
/// - `Err(())` - file absent/unreadable: the box was clobbered or torn down
///   under us (a concurrency/infra fault), which the caller surfaces as an
///   internal sandbox error (-> retryable SystemError), NOT as empty output.
///
/// FIFO redirects are handled by the caller (streamed; nothing to read back).
async fn read_declared_redirect(resolved: &Path, stream: &str) -> Result<String, ()> {
    match read_text_preview(resolved).await {
        Ok(s) => Ok(s),
        Err(e) => {
            tracing::warn!(
                path = %resolved.display(),
                error = %e,
                stream,
                "Declared output file missing after execution; treating as sandbox internal error"
            );
            Err(())
        }
    }
}

fn parse_box_id(id: Option<&str>) -> Result<String, SandboxError> {
    let raw = id.unwrap_or("0");
    raw.parse::<u32>()
        .map(|n| n.to_string())
        .map_err(|_| SandboxError::Initialization(format!("invalid isolate box id: {raw}")))
}

fn add_directory_rule_args(command: &mut Command, rule: &DirectoryRule) {
    let inside = rule.inside_path.to_string_lossy();
    let mut option_parts = Vec::new();

    if rule.options.read_write {
        option_parts.push("rw");
    }
    if rule.options.allow_devices {
        option_parts.push("dev");
    }
    if rule.options.no_exec {
        option_parts.push("noexec");
    }
    if rule.options.is_filesystem {
        option_parts.push("fs");
    }
    if rule.options.is_tmp {
        option_parts.push("tmp");
    }
    if rule.options.no_recursive {
        option_parts.push("norec");
    }

    let options = if option_parts.is_empty() {
        String::new()
    } else {
        format!(":{}", option_parts.join(","))
    };

    let rule_value = match &rule.outside_path {
        Some(outside) => format!("{}={}{}", inside, outside.to_string_lossy(), options),
        None => {
            if option_parts.is_empty() {
                format!("{}=", inside)
            } else {
                format!("{}{}", inside, options)
            }
        }
    };

    command.arg(format!("--dir={rule_value}"));
}

fn add_env_rule_args(command: &mut Command, rule: &EnvRule) {
    match rule {
        EnvRule::Inherit(var) => {
            command.arg(format!("--env={var}"));
        }
        EnvRule::Set(var, value) => {
            command.arg(format!("--env={var}={value}"));
        }
        EnvRule::FullEnv => {
            command.arg("--full-env");
        }
    }
}

fn add_resource_limit_args(
    command: &mut Command,
    limits: &ResourceLimits,
    cgroups_enabled: bool,
) -> Result<(), SandboxError> {
    if let Some(time_limit) = limits.time_limit {
        command.arg(format!("--time={time_limit}"));
    }
    if let Some(wall_time_limit) = limits.wall_time_limit {
        command.arg(format!("--wall-time={wall_time_limit}"));
    }
    if let Some(extra_time) = limits.extra_time {
        command.arg(format!("--extra-time={extra_time}"));
    }
    if let Some(memory_limit) = limits.memory_limit {
        if cgroups_enabled {
            command.arg(format!("--cg-mem={memory_limit}"));
        } else {
            command.arg(format!("--mem={memory_limit}"));
        }
    }
    if let Some(stack_limit) = limits.stack_limit {
        command.arg(format!("--stack={stack_limit}"));
    }
    if let Some(open_files_limit) = limits.open_files_limit {
        command.arg(format!("--open-files={open_files_limit}"));
    }
    if let Some(file_size_limit) = limits.file_size_limit {
        command.arg(format!("--fsize={file_size_limit}"));
    }
    if let Some(process_limit) = limits.process_limit {
        if process_limit == 0 {
            command.arg("--processes");
        } else {
            command.arg(format!("--processes={process_limit}"));
        }
    }

    Ok(())
}

async fn parse_meta_file(meta_path: &Path) -> Result<ExecutionResult, SandboxError> {
    let content = fs::read_to_string(meta_path).await.map_err(|err| {
        SandboxError::Execution(format!("failed to read isolate meta file: {err}"))
    })?;

    let mut raw = HashMap::<String, String>::new();

    for line in content.lines() {
        if let Some((key, value)) = line.split_once(':') {
            raw.insert(key.trim().to_string(), value.trim().to_string());
        }
    }

    let parse_i32 = |key: &str| raw.get(key).and_then(|v| v.parse::<i32>().ok());
    let parse_u32 = |key: &str| raw.get(key).and_then(|v| v.parse::<u32>().ok());
    let parse_f64 = |key: &str| {
        raw.get(key)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };

    // isolate omits the status line on a clean exit-0 run; treat that as "OK".
    let status = raw
        .get("status")
        .cloned()
        .unwrap_or_else(|| "OK".to_string());

    Ok(ExecutionResult {
        exit_code: parse_i32("exitcode"),
        signal: parse_i32("exitsig"),
        time_used: parse_f64("time"),
        wall_time_used: parse_f64("time-wall"),
        // Prefer per-step `max-rss` over `cg-mem`. With `--cg`, `cg-mem` is the box
        // cgroup's memory PEAK since `--init`, SHARED by every step in the box, so
        // a step running after a heavier one (e.g. `run` after `compile`) inherits
        // the earlier step's peak - reporting hundreds of MB for a 4 MB run. Unlike
        // CPU (cumulative -> per-step delta recoverable by subtraction, see
        // `box_cpu_secs`), a peak is not additive, so there is no arithmetic
        // reconciliation. `max-rss` comes from wait4/getrusage of THIS `--run`'s
        // child, so it is genuinely per-step. It is also consistent with the
        // `cg-oom-killed`-gated MLE verdict (OOM is driven by live usage crossing
        // the limit, which max-rss tracks) and is the conventional "memory used"
        // metric. `cg-mem` remains the fallback if a build omits max-rss. This is a
        // REPORTED-value change only; the MLE verdict is gated on `cg-oom-killed`.
        memory_used: parse_u32("max-rss").or(parse_u32("cg-mem")),
        cg_oom_killed: parse_i32("cg-oom-killed").map(|v| v != 0).unwrap_or(false),
        killed: parse_i32("killed").map(|v| v != 0).unwrap_or(false),
        sandbox_status: SandboxStatus::from_isolate(&status),
        status,
        message: raw.get("message").cloned().unwrap_or_default(),
        stdout: String::new(),
        stderr: String::new(),
    })
}

#[async_trait]
impl SandboxManager for IsolateSandboxManager {
    #[tracing::instrument(
        name = "isolate_init",
        skip(self),
        fields(box_id = id.unwrap_or("0"), enable_cgroups = self.enable_cgroups)
    )]
    async fn create_sandbox(&self, id: Option<&str>) -> Result<PathBuf, SandboxError> {
        let start = std::time::Instant::now();
        let mut command = Command::new(&self.isolate_bin);
        if let Some(box_id) = id {
            command.arg(format!("--box-id={box_id}"));
        }
        if self.enable_cgroups {
            command.arg("--cg");
        }
        command.arg("--init");

        let output = match command.output().await {
            Ok(output) => output,
            Err(err) => {
                if let Some(metrics) = &self.metrics {
                    metrics
                        .sandbox_init_duration
                        .record(start.elapsed().as_secs_f64(), &self.metric_attrs("error"));
                }
                return Err(SandboxError::Initialization(format!(
                    "failed to execute isolate --init: {err}"
                )));
            }
        };

        if !output.status.success() {
            if let Some(metrics) = &self.metrics {
                metrics
                    .sandbox_init_duration
                    .record(start.elapsed().as_secs_f64(), &self.metric_attrs("error"));
            }
            return Err(SandboxError::Initialization(format!(
                "isolate --init failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        let path_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path_text.is_empty() {
            if let Some(metrics) = &self.metrics {
                metrics
                    .sandbox_init_duration
                    .record(start.elapsed().as_secs_f64(), &self.metric_attrs("error"));
            }
            return Err(SandboxError::Initialization(
                "isolate --init did not return sandbox path".to_string(),
            ));
        }

        let working_dir = PathBuf::from(&path_text).join("box");
        let box_id = id.unwrap_or("0").to_string();
        self.sandboxes
            .write()
            .await
            .insert(box_id.clone(), working_dir.clone());
        // Fresh box (--init resets the cgroup) -> zero prior CPU for this box.
        if let Ok(mut m) = self.box_cpu_secs.lock() {
            m.insert(box_id, 0.0);
        }

        if let Some(metrics) = &self.metrics {
            metrics
                .sandbox_init_duration
                .record(start.elapsed().as_secs_f64(), &self.metric_attrs("success"));
        }

        Ok(working_dir)
    }

    #[tracing::instrument(
        name = "isolate_cleanup",
        skip(self),
        fields(box_id = %id, enable_cgroups = self.enable_cgroups)
    )]
    async fn remove_sandbox(&self, id: &str) -> Result<(), SandboxError> {
        let start = std::time::Instant::now();
        let box_id = parse_box_id(Some(id))?;
        self.sandboxes.write().await.remove(&box_id);
        if let Ok(mut m) = self.box_cpu_secs.lock() {
            m.remove(&box_id);
        }

        let mut command = Command::new(&self.isolate_bin);
        command.arg(format!("--box-id={box_id}"));
        if self.enable_cgroups {
            command.arg("--cg");
        }
        command.arg("--cleanup");

        let output = match command.output().await {
            Ok(output) => output,
            Err(err) => {
                if let Some(metrics) = &self.metrics {
                    metrics
                        .sandbox_cleanup_duration
                        .record(start.elapsed().as_secs_f64(), &self.metric_attrs("error"));
                }
                return Err(SandboxError::Execution(format!(
                    "failed to execute isolate --cleanup: {err}"
                )));
            }
        };

        if !output.status.success() {
            if let Some(metrics) = &self.metrics {
                metrics
                    .sandbox_cleanup_duration
                    .record(start.elapsed().as_secs_f64(), &self.metric_attrs("error"));
            }
            return Err(SandboxError::Execution(format!(
                "isolate --cleanup failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }

        if let Some(metrics) = &self.metrics {
            metrics
                .sandbox_cleanup_duration
                .record(start.elapsed().as_secs_f64(), &self.metric_attrs("success"));
        }

        Ok(())
    }

    #[tracing::instrument(
        name = "isolate_execute",
        skip(self, argv, run_options),
        fields(
            box_id = %box_id,
            argv_len = argv.len(),
            wait = run_options.wait,
            env_rule_count = run_options.env_rules.len(),
            directory_rule_count = run_options.directory_rules.len(),
            enable_cgroups = self.enable_cgroups
        )
    )]
    async fn execute(
        &self,
        box_id: &str,
        argv: Vec<String>,
        run_options: &RunOptions,
    ) -> Result<ExecutionResult, SandboxError> {
        if argv.is_empty() {
            return Err(SandboxError::Execution(
                "isolate --run requires at least one program argument".to_string(),
            ));
        }

        // Per-box cumulative-CPU offset (see `box_cpu_secs`). With `--cg`,
        // isolate's `time` and `--time` limit are the box cgroup's CPU usage
        // cumulative since `--init`, so a step inherits prior same-box steps'
        // CPU. Add the prior cumulative to this step's CPU `--time` so isolate
        // enforces the step's OWN limit, then report only the step's own delta.
        // WITHOUT cgroups isolate reports per-`--run` CPU already, so there is no
        // cumulative to reconcile and the offset is zero (see `cpu_time_offset`).
        // `time_limit` is CPU seconds; wall time (`--wall-time`) is measured per
        // `--run` and is not cumulative, so it is left untouched.
        let box_key = parse_box_id(Some(box_id))?;
        let prior_cpu = self
            .box_cpu_secs
            .lock()
            .ok()
            .and_then(|m| m.get(&box_key).copied())
            .unwrap_or(0.0);
        let offset = cpu_time_offset(self.enable_cgroups, prior_cpu);
        let adjusted_opts = if offset > 0.0 && run_options.resource_limits.time_limit.is_some() {
            let mut ro = run_options.clone();
            ro.resource_limits.time_limit = ro.resource_limits.time_limit.map(|t| t + offset);
            Some(ro)
        } else {
            None
        };
        let effective_opts = adjusted_opts.as_ref().unwrap_or(run_options);

        const MAX_TRANSIENT_RETRIES: usize = 3;
        for attempt in 0..=MAX_TRANSIENT_RETRIES {
            let result = self
                .execute_once(box_id, argv.clone(), effective_opts)
                .await;
            match result {
                // Retryable isolate setup error: under concurrent judging a
                // box's input file can momentarily carry restrictive perms
                // (shared cache inode being re-chmodded by another op), making
                // isolate's open() of stdin/input fail. Re-running picks up the
                // now-readable file. Bounded by MAX_TRANSIENT_RETRIES.
                Err(SandboxError::Unknown(msg))
                    if attempt < MAX_TRANSIENT_RETRIES && msg.contains("Permission denied") =>
                {
                    let backoff_ms = 25u64 << attempt;
                    tracing::warn!(
                        box_id = %box_id,
                        attempt = attempt + 1,
                        backoff_ms,
                        error = %msg,
                        "Transient isolate setup failure (EACCES), retrying after backoff",
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(e) => return Err(e),
                Ok(mut exec) => {
                    if is_transient_exec_failure(&exec) {
                        if attempt < MAX_TRANSIENT_RETRIES {
                            let backoff_ms = 25u64 << attempt;
                            tracing::warn!(
                                box_id = %box_id,
                                attempt = attempt + 1,
                                backoff_ms,
                                stderr_preview =
                                    %exec.stderr.chars().take(120).collect::<String>(),
                                "Transient exec failure (EAGAIN), retrying after backoff",
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                            continue;
                        }
                        // Retries exhausted but the guest still cannot exec: the
                        // host is momentarily out of PID/thread headroom (execve
                        // EAGAIN, exit 127). That is a worker-saturation fault,
                        // never the contestant's program, so tag it as an internal
                        // sandbox error. The evaluator maps InternalError to a
                        // retryable SystemError (bounded by max_system_error_retries,
                        // self-healing) instead of a terminal RuntimeError that
                        // permanently blames correct code. In-sandbox backoff cannot
                        // outlast sustained saturation; a SystemError requeue re-runs
                        // the submission later, once pressure eases. Mirrors the
                        // redirect-vanished `infra_failure` path below.
                        tracing::warn!(
                            box_id = %box_id,
                            max_retries = MAX_TRANSIENT_RETRIES,
                            stderr_preview = %exec.stderr.chars().take(120).collect::<String>(),
                            "Exec failure (EAGAIN) persisted past retries; tagging as internal \
                             sandbox error for SystemError retry",
                        );
                        exec.sandbox_status = SandboxStatus::InternalError;
                        exec.status = "XX".to_string();
                        exec.message = format!(
                            "guest exec failed with EAGAIN after {MAX_TRANSIENT_RETRIES} retries \
                             (worker saturated; internal error)"
                        );
                        return Ok(exec);
                    }
                    // With `--cg`, `exec.time_used` is the box cgroup's cumulative
                    // CPU since --init: persist it as the new prior, then report
                    // only this step's own delta (`cumulative - offset`). Without
                    // cgroups the value is already this run's own per-`--run` CPU
                    // and `offset` is zero, so it is reported unchanged and nothing
                    // is persisted (the map holds no meaningful cumulative there).
                    let cumulative = exec.time_used;
                    if self.enable_cgroups
                        && let Ok(mut m) = self.box_cpu_secs.lock()
                    {
                        m.insert(box_key.clone(), cumulative.max(prior_cpu));
                    }
                    exec.time_used = (cumulative - offset).max(0.0);
                    return Ok(exec);
                }
            }
        }
        unreachable!("retry loop exits via return")
    }
}

fn is_transient_exec_failure(result: &ExecutionResult) -> bool {
    result.exit_code == Some(127)
        && result.stderr.contains("execve(")
        && result.stderr.contains("Resource temporarily unavailable")
}

/// True when a `Command::spawn` failure is a transient fork EAGAIN.
///
/// A `fork()`/`clone()` that fails because the worker is momentarily out of
/// PID/thread headroom sets errno to EAGAIN. It is distinct from the guest's
/// own execve EAGAIN (`is_transient_exec_failure`, which inspects a completed
/// run): this fires before isolate exists, on the raw spawn error. Matching on
/// the OS errno rather than the message keeps it locale-independent.
fn is_fork_eagain(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(libc::EAGAIN)
}

/// Worker-side hard-timeout backstop for a single `isolate --run`.
///
/// isolate's `--wall-time`/`--extra-time` bound the guest program's own run,
/// but not a child wedged BEFORE it execs (a blocking `open()` on a channel
/// FIFO with no writer never lets isolate's wall clock start). So the backstop
/// is the declared wall limit plus grace plus a fixed margin (isolate startup +
/// our own kill/cleanup slack). A step with no wall limit (e.g. compile) falls
/// back to a generous absolute default so a long-but-legitimate step is never
/// killed. This is a backstop, not a verdict clock: it must sit safely ABOVE
/// any time isolate itself would enforce.
fn worker_hard_timeout(limits: &ResourceLimits) -> std::time::Duration {
    const MARGIN_SECS: f64 = 30.0;
    const DEFAULT_SECS: f64 = 300.0;
    match limits.wall_time_limit {
        Some(wall) if wall > 0.0 => {
            let extra = limits.extra_time.unwrap_or(0.0).max(0.0);
            std::time::Duration::from_secs_f64(wall + extra + MARGIN_SECS)
        }
        _ => std::time::Duration::from_secs_f64(DEFAULT_SECS),
    }
}

/// CPU-seconds offset used to reconcile isolate's reported `time` (and `--time`
/// limit) into a single step's own CPU.
///
/// With `--cg`, isolate's `time`/`--time` are the box cgroup's CPU cumulative
/// since `--init`, so `prior_cpu` (earlier same-box steps' CPU) is the offset:
/// add it to the step's `--time` limit, subtract it from the reported time.
/// WITHOUT cgroups isolate already reports per-`--run` CPU, so there is no
/// cumulative to reconcile and the offset is zero - the reported value is used
/// as-is. Gating on `enable_cgroups` here is what keeps `time_used`/TLE correct
/// when cgroups is off.
fn cpu_time_offset(enable_cgroups: bool, prior_cpu: f64) -> f64 {
    if enable_cgroups { prior_cpu } else { 0.0 }
}

impl IsolateSandboxManager {
    #[tracing::instrument(
        name = "isolate_execute_once",
        skip(self, argv, run_options),
        fields(
            box_id = %box_id,
            argv_len = argv.len(),
            wait = run_options.wait,
            env_rule_count = run_options.env_rules.len(),
            directory_rule_count = run_options.directory_rules.len(),
            enable_cgroups = self.enable_cgroups
        )
    )]
    async fn execute_once(
        &self,
        box_id: &str,
        argv: Vec<String>,
        run_options: &RunOptions,
    ) -> Result<ExecutionResult, SandboxError> {
        let box_id = parse_box_id(Some(box_id))?;
        let meta_path = std::env::temp_dir().join(format!(
            "broccoli-isolate-{box_id}-{}.meta",
            uuid::Uuid::new_v4()
        ));
        // Remove the --meta temp file on EVERY return path. isolate writes it on
        // exit, and any early `?` after that (JoinError on the pipe readers, an
        // unreadable/short meta file, a failed wait) would otherwise leak it into
        // /tmp, which nothing sweeps - unbounded accumulation over a long-lived
        // worker. A sync unlink in Drop is fine for one temp file.
        struct MetaGuard(std::path::PathBuf);
        impl Drop for MetaGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _meta_guard = MetaGuard(meta_path.clone());

        let mut command = Command::new(&self.isolate_bin);
        command.arg(format!("--box-id={box_id}"));
        if self.enable_cgroups {
            command.arg("--cg");
        }
        command.arg(format!("--meta={}", meta_path.to_string_lossy()));

        if run_options.wait {
            command.arg("--wait");
        }
        if let Some(uid) = run_options.as_uid {
            command.arg(format!("--as-uid={uid}"));
        }
        if let Some(gid) = run_options.as_gid {
            command.arg(format!("--as-gid={gid}"));
        }

        add_resource_limit_args(
            &mut command,
            &run_options.resource_limits,
            self.enable_cgroups,
        )?;

        if let Some(stdin) = &run_options.stdin {
            command.arg(format!("--stdin={}", stdin.to_string_lossy()));
        }
        if let Some(stdout) = &run_options.stdout {
            command.arg(format!("--stdout={}", stdout.to_string_lossy()));
        }
        if let Some(stderr) = &run_options.stderr {
            command.arg(format!("--stderr={}", stderr.to_string_lossy()));
        }
        if run_options.env_rules.is_empty() {
            // Never inherit the worker's full environment into a sandboxed
            // (potentially contestant-controlled) program: `--full-env` would
            // leak the DB/Redis/S3 credentials, the JWT secret, and every other
            // BROCCOLI__* value the worker process holds. Provide a minimal,
            // secret-free default environment instead. Callers that need extra
            // variables must request them explicitly via `env_rules`.
            command.arg("--env=PATH=/usr/local/bin:/usr/bin:/bin");
        } else {
            for rule in &run_options.env_rules {
                add_env_rule_args(&mut command, rule);
            }
        }
        for rule in &run_options.directory_rules {
            add_directory_rule_args(&mut command, rule);
        }

        // Debian's openjdk toolchain resolves through /etc: `javac`/`java` are
        // symlinks into /etc/alternatives, and the JVM reads its security policy
        // (java.security, cacerts) from /etc/java-*-openjdk and /etc/ssl/certs/java.
        // isolate's compiled-in default dir rules (box,bin,dev,lib,lib64,proc,
        // tmp,usr) omit /etc, so Java compile and run fail with "No such file or
        // directory" / "Error loading java.security file" without it. Mount /etc
        // read-only for every step so Java works alongside c/cpp/python3, whose
        // relative symlinks stay under /usr. This exposes /etc to contestant exec
        // steps: /etc/passwd is world-readable (not secret) and /etc/shadow (0640)
        // stays unreadable to the box uid, matching standard isolate-based judges.
        // Skip if a caller already supplied an explicit /etc rule.
        let has_etc_rule = run_options.directory_rules.iter().any(|rule| {
            let inside = rule.inside_path.to_string_lossy();
            inside == "/etc" || inside == "etc"
        });
        if !has_etc_rule {
            add_directory_rule_args(
                &mut command,
                &DirectoryRule {
                    inside_path: PathBuf::from("/etc"),
                    outside_path: Some(PathBuf::from("/etc")),
                    options: DirectoryOptions::default(),
                },
            );
        }

        let rewritten_argv: Vec<String> = argv
            .into_iter()
            .map(|arg| {
                for rule in &run_options.directory_rules {
                    let inside = rule.inside_path.to_string_lossy();
                    if let Some(rel) = inside.strip_prefix('/')
                        && (arg == rel || arg.starts_with(&format!("{rel}/")))
                    {
                        return format!("/{arg}");
                    }
                }
                arg
            })
            .collect();

        command.arg("--run").arg("--").args(&rewritten_argv);

        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        // Spawning `isolate --run` is itself a fork() in the worker process.
        // Under worker-level PID/thread exhaustion (many concurrent judged
        // boxes on a container with a tight pids/nproc limit) that fork can
        // fail with EAGAIN ("Resource temporarily unavailable") BEFORE isolate
        // ever runs - distinct from a *guest* execve EAGAIN, which surfaces as
        // a completed run (exit 127) caught by `is_transient_exec_failure`.
        // Here there is no ExecutionResult, only an io::Error, so the guest
        // path can never see it; without a retry the fork EAGAIN maps straight
        // to a terminal error and (via the SystemError retry) burns a stuck
        // attempt on what is a momentary, self-clearing resource spike. A
        // failed spawn touches no box state, so retrying is side-effect-free:
        // back off briefly to let peer boxes drain and free fork headroom.
        // Bounded by MAX_SPAWN_RETRIES; past the cap the error propagates and
        // still self-heals via the outer SystemError retry.
        const MAX_SPAWN_RETRIES: usize = 3;
        let mut spawn_attempt = 0;
        let mut child = loop {
            match command.spawn() {
                Ok(child) => break child,
                Err(err) if spawn_attempt < MAX_SPAWN_RETRIES && is_fork_eagain(&err) => {
                    let backoff_ms = 25u64 << spawn_attempt;
                    tracing::warn!(
                        box_id = %box_id,
                        attempt = spawn_attempt + 1,
                        backoff_ms,
                        "Transient fork failure spawning isolate --run (EAGAIN); retrying after backoff",
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                    spawn_attempt += 1;
                    continue;
                }
                Err(err) => {
                    return Err(SandboxError::Execution(format!(
                        "failed to spawn isolate --run: {err}"
                    )));
                }
            }
        };
        let stdout = child.stdout.take().ok_or_else(|| {
            SandboxError::Execution("failed to capture isolate stdout".to_string())
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            SandboxError::Execution("failed to capture isolate stderr".to_string())
        })?;
        let stdout_task = tokio::spawn(read_capped_child_pipe(stdout));
        let stderr_task = tokio::spawn(read_capped_child_pipe(stderr));

        // Worker-side hard backstop around `isolate --wait`. isolate's own
        // `--wall-time` only advances once the guest program is RUNNING, so it
        // does NOT reap a child wedged in a PRE-exec syscall - e.g. a fused
        // check step's `open()` of the channel FIFO `/channels/sol_out`, which
        // blocks in the kernel waiting for a writer that a failed/EAGAIN'd exec
        // step never opened. Such a child hangs forever and `child.wait()` never
        // returns; with no bound here the whole task future parks, the MQ
        // message is never acked, and the submission is orphaned permanently
        // (only a worker restart frees it). Bound the wait at the step's wall
        // limit + margin (or an absolute default when no wall limit is set);
        // anything past that is definitely wedged. On elapse: SIGKILL the
        // `isolate --run` we spawned AND `isolate --cleanup` the box (the keeper
        // + sandboxed child survive the parent's death via reparenting, so
        // killing our handle alone would leave them holding the FIFO), then
        // return an InternalError-tagged result so the evaluator maps it to a
        // retryable SystemError (self-heals via the bounded stuck/system-error
        // retry) rather than a terminal misverdict.
        let hard_timeout = worker_hard_timeout(&run_options.resource_limits);
        let status = match tokio::time::timeout(hard_timeout, child.wait()).await {
            Ok(wait_res) => wait_res.map_err(|err| {
                SandboxError::Execution(format!("failed to wait for isolate --run: {err}"))
            })?,
            Err(_elapsed) => {
                tracing::error!(
                    box_id = %box_id,
                    timeout_secs = hard_timeout.as_secs_f64(),
                    "isolate --run exceeded the worker hard timeout; killing and cleaning the box"
                );
                let _ = child.start_kill();
                let _ = child.wait().await;
                let mut cleanup = Command::new(&self.isolate_bin);
                cleanup.arg(format!("--box-id={box_id}"));
                if self.enable_cgroups {
                    cleanup.arg("--cg");
                }
                cleanup.arg("--cleanup");
                if let Err(err) = cleanup.output().await {
                    tracing::warn!(
                        box_id = %box_id,
                        error = %err,
                        "isolate --cleanup after hard-timeout failed"
                    );
                }
                stdout_task.abort();
                stderr_task.abort();
                return Ok(ExecutionResult {
                    killed: true,
                    sandbox_status: SandboxStatus::InternalError,
                    status: "XX".to_string(),
                    message: format!(
                        "isolate --run exceeded the worker hard timeout of {:.0}s (sandbox wedged; internal error)",
                        hard_timeout.as_secs_f64()
                    ),
                    ..Default::default()
                });
            }
        };
        let output_stdout = join_pipe_capture(stdout_task, "stdout").await?;
        let output_stderr = join_pipe_capture(stderr_task, "stderr").await?;

        match status.code() {
            Some(0) | Some(1) => {
                let mut result = parse_meta_file(&meta_path).await?;
                let box_dir = self
                    .sandboxes
                    .read()
                    .await
                    .get(&box_id)
                    .cloned()
                    .ok_or_else(|| {
                        SandboxError::Execution(format!(
                            "sandbox working directory not found for box id: {box_id}"
                        ))
                    })?;
                let mut infra_failure = false;
                result.stdout = if let Some(stdout_path) = &run_options.stdout {
                    let resolved = box_dir.join(stdout_path);
                    if is_fifo(&resolved) || !is_box_local_redirect(&box_dir, stdout_path) {
                        String::new()
                    } else {
                        read_declared_redirect(&resolved, "stdout")
                            .await
                            .unwrap_or_else(|()| {
                                infra_failure = true;
                                String::new()
                            })
                    }
                } else {
                    text_preview_from_bytes(output_stdout, false)
                };
                result.stderr = if let Some(stderr_path) = &run_options.stderr {
                    let resolved = box_dir.join(stderr_path);
                    if is_fifo(&resolved) || !is_box_local_redirect(&box_dir, stderr_path) {
                        String::new()
                    } else {
                        read_declared_redirect(&resolved, "stderr")
                            .await
                            .unwrap_or_else(|()| {
                                infra_failure = true;
                                String::new()
                            })
                    }
                } else {
                    text_preview_from_bytes(output_stderr, false)
                };

                if infra_failure {
                    // A declared redirect vanished after a clean exit: the box was
                    // clobbered or torn down concurrently. Tag it as an internal
                    // sandbox error so the evaluator maps it to a retryable
                    // SystemError (which self-heals) instead of scoring the missing
                    // output as a terminal WrongAnswer/TLE that never recovers.
                    result.sandbox_status = SandboxStatus::InternalError;
                    result.status = "XX".to_string();
                    if result.message.is_empty() {
                        result.message =
                            "sandbox output file missing after execution (internal error)"
                                .to_string();
                    }
                }
                Ok(result)
            }
            _ => Err(SandboxError::Unknown(format!(
                "isolate internal error: {}",
                String::from_utf8_lossy(&output_stderr).trim()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_relative_redirect_is_box_local() {
        // A box-relative redirect file is created by isolate inside box_dir, so
        // it is read back and its absence after a clean exit is a real fault.
        let box_dir = Path::new("/var/local/lib/isolate/0/box");
        assert!(is_box_local_redirect(box_dir, Path::new("output.txt")));
        assert!(is_box_local_redirect(box_dir, Path::new("sub/out.txt")));
    }

    #[test]
    fn absolute_channel_redirect_is_not_box_local() {
        // Regression guard: a fused exec redirects stdout to the in-box channel
        // FIFO `/channels/<name>`. `Path::join` drops box_dir for an absolute
        // arg, so box_dir.join("/channels/sol_out") == "/channels/sol_out" (a
        // host path that never exists). It MUST be treated as streamed (skip the
        // read-back existence check), NOT misflagged as a missing-file infra
        // fault - otherwise every fused submission fails with SystemError.
        let box_dir = Path::new("/var/local/lib/isolate/0/box");
        assert!(!is_box_local_redirect(
            box_dir,
            Path::new("/channels/sol_out")
        ));
        assert!(!is_box_local_redirect(
            box_dir,
            Path::new("/channels/stderr")
        ));
    }

    #[test]
    fn fork_eagain_is_classified_by_errno() {
        // A spawn that fails with EAGAIN is the retryable worker-fork case; any
        // other errno (EACCES, ENOENT) or a non-OS error is NOT and must
        // propagate immediately rather than burn the bounded spawn retries.
        assert!(is_fork_eagain(&std::io::Error::from_raw_os_error(
            libc::EAGAIN
        )));
        assert!(!is_fork_eagain(&std::io::Error::from_raw_os_error(
            libc::EACCES
        )));
        assert!(!is_fork_eagain(&std::io::Error::from_raw_os_error(
            libc::ENOENT
        )));
        assert!(!is_fork_eagain(&std::io::Error::other("not an os error")));
    }

    #[test]
    fn transient_exec_failure_matches_execve_eagain_only() {
        // The exact signature isolate emits when the guest cannot execve because
        // the host is out of PID/thread headroom: exit 127 + `execve(...)` +
        // `Resource temporarily unavailable` (EAGAIN). This is the observed
        // saturation artifact that must be reclassified to a retryable
        // SystemError, never a terminal RuntimeError.
        let eagain = ExecutionResult {
            exit_code: Some(127),
            stderr: "execve(\"./main\"): Resource temporarily unavailable".to_string(),
            ..Default::default()
        };
        assert!(is_transient_exec_failure(&eagain));

        // Near-misses must NOT match: a genuine exit-127 from the guest without
        // the EAGAIN marker is a real RuntimeError and must stay one.
        let real_127 = ExecutionResult {
            exit_code: Some(127),
            stderr: "bash: ./main: No such file or directory".to_string(),
            ..Default::default()
        };
        assert!(!is_transient_exec_failure(&real_127));

        // EAGAIN text but a clean exit code is not an exec failure.
        let clean_exit = ExecutionResult {
            exit_code: Some(0),
            stderr: "Resource temporarily unavailable".to_string(),
            ..Default::default()
        };
        assert!(!is_transient_exec_failure(&clean_exit));

        // A different nonzero exit with the marker is likewise not the exec path.
        let other_exit = ExecutionResult {
            exit_code: Some(1),
            stderr: "execve(\"./main\"): Resource temporarily unavailable".to_string(),
            ..Default::default()
        };
        assert!(!is_transient_exec_failure(&other_exit));
    }

    #[test]
    fn cpu_offset_is_zero_without_cgroups() {
        // Without cgroups isolate reports per-`--run` CPU, so no prior-step
        // offset is ever applied, regardless of what a prior step consumed.
        assert_eq!(cpu_time_offset(false, 0.0), 0.0);
        assert_eq!(cpu_time_offset(false, 1.5), 0.0);
    }

    #[test]
    fn worker_hard_timeout_sits_above_the_wall_limit() {
        // Backstop must exceed the wall limit (+ extra grace) so it never fires
        // on a run that isolate would itself enforce, but is finite so a wedged
        // pre-exec child is always reaped.
        let limits = ResourceLimits {
            wall_time_limit: Some(10.0),
            extra_time: Some(2.0),
            ..Default::default()
        };
        let d = worker_hard_timeout(&limits).as_secs_f64();
        assert!(d > 12.0, "backstop {d}s must exceed wall+extra=12s");
        assert!(
            d < 60.0,
            "backstop {d}s should be a tight margin, not open-ended"
        );
    }

    #[test]
    fn worker_hard_timeout_falls_back_without_wall_limit() {
        // A step with no wall limit (e.g. compile) gets a generous absolute
        // default, never a tiny/zero bound that could kill a legitimate step.
        let limits = ResourceLimits {
            wall_time_limit: None,
            ..Default::default()
        };
        let d = worker_hard_timeout(&limits).as_secs_f64();
        assert!(d >= 300.0, "no-wall-limit fallback {d}s must be generous");

        // A zero/nonsensical wall limit also takes the safe fallback, not 30s.
        let zero = ResourceLimits {
            wall_time_limit: Some(0.0),
            ..Default::default()
        };
        assert!(worker_hard_timeout(&zero).as_secs_f64() >= 300.0);
    }

    #[test]
    fn cpu_offset_is_prior_cumulative_with_cgroups() {
        // With cgroups isolate reports cumulative CPU, so the prior cumulative is
        // the offset to add to the limit / subtract from the reported time.
        assert_eq!(cpu_time_offset(true, 0.0), 0.0);
        assert_eq!(cpu_time_offset(true, 1.5), 1.5);
    }

    #[test]
    fn reported_time_is_isolate_per_run_without_cgroups() {
        // A prior step recorded 2.0s; this run's isolate-reported per-`--run` CPU
        // is 0.4s. With cgroups off the offset is 0, so the reported time is
        // isolate's raw value (0.4) - NOT 0.4 - 2.0 clamped to 0.
        let prior_cpu = 2.0;
        let isolate_reported = 0.4;
        let offset = cpu_time_offset(false, prior_cpu);
        let reported = (isolate_reported - offset).max(0.0);
        assert_eq!(reported, isolate_reported);
    }

    #[test]
    fn reported_time_subtracts_offset_with_cgroups() {
        // With cgroups, isolate reports cumulative CPU (2.0 prior + 0.4 this step
        // = 2.4); subtracting the offset (2.0) yields this step's own 0.4s.
        let prior_cpu = 2.0;
        let isolate_cumulative = 2.4;
        let offset = cpu_time_offset(true, prior_cpu);
        let reported = (isolate_cumulative - offset).max(0.0);
        assert!((reported - 0.4).abs() < 1e-9);
    }
}
