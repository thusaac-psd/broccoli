use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

pub const DEFAULT_OPERATION_RESULT_TIMEOUT_MS: u64 = 60 * 60 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SessionFile {
    #[serde(rename = "path")]
    Path { path: String },
    #[serde(rename = "content")]
    Content { content: String },
    #[serde(rename = "blob")]
    Blob { hash: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub files_in: Vec<(String, SessionFile)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum IOTarget {
    #[serde(rename = "null")]
    Null,
    #[serde(rename = "inherit")]
    #[default]
    Inherit,
    #[serde(rename = "file")]
    File { path: String },
    #[serde(rename = "pipe")]
    Pipe { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct IOConfig {
    pub stdin: IOTarget,
    pub stdout: IOTarget,
    pub stderr: IOTarget,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DirectoryOptions {
    pub read_write: bool,
    pub allow_devices: bool,
    pub no_exec: bool,
    pub is_filesystem: bool,
    pub is_tmp: bool,
    pub no_recursive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryRule {
    pub inside_path: PathBuf,
    pub outside_path: Option<PathBuf>,
    pub options: DirectoryOptions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EnvRule {
    Inherit(String),
    Set(String, String),
    FullEnv,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResourceLimits {
    pub time_limit: Option<f64>,
    pub wall_time_limit: Option<f64>,
    pub extra_time: Option<f64>,
    pub memory_limit: Option<u32>,
    pub stack_limit: Option<u32>,
    pub open_files_limit: Option<u32>,
    pub file_size_limit: Option<u32>,
    pub process_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RunOptions {
    pub resource_limits: ResourceLimits,
    pub wait: bool,
    pub as_uid: Option<u32>,
    pub as_gid: Option<u32>,
    pub stdin: Option<PathBuf>,
    pub stdout: Option<PathBuf>,
    pub stderr: Option<PathBuf>,
    pub env_rules: Vec<EnvRule>,
    pub directory_rules: Vec<DirectoryRule>,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            resource_limits: ResourceLimits {
                time_limit: None,
                wall_time_limit: None,
                extra_time: None,
                memory_limit: None,
                stack_limit: None,
                open_files_limit: None,
                file_size_limit: None,
                process_limit: Some(1),
            },
            wait: true,
            as_uid: None,
            as_gid: None,
            stdin: None,
            stdout: None,
            stderr: None,
            // Default to NO inherited environment. The worker translates an empty
            // env_rules list into a minimal, secret-free environment (a fixed
            // PATH). Inheriting the worker's full environment by default would
            // leak DB/Redis/S3 credentials and the JWT secret into every sandboxed
            // process (contestant code, checkers, comparators). A step that
            // genuinely needs a variable must request it explicitly.
            env_rules: vec![],
            directory_rules: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepCacheConfig {
    pub key_inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    #[default]
    Generic,
    Compile,
    Testcase,
    CheckerCompile,
    Checker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum MountSource {
    StepOutput { from_step: String, file: String },
    PlatformTool { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSpec {
    pub inside_path: String,
    pub source: MountSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    #[serde(default)]
    pub kind: StepKind,
    pub env_ref: String,
    pub argv: Vec<String>,
    pub conf: RunOptions,
    pub io: IOConfig,
    /// Files the worker uploads to the blob store after the step runs; their
    /// hashes come back in [`TaskExecutionResult::collected_outputs`]. A file
    /// that is the step's own stdout or stderr redirect, and whose exact bytes
    /// [`ExecutionResult::stdout`] or [`ExecutionResult::stderr`] already
    /// carries, is not uploaded and has no hash.
    pub collect: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub cache: Option<StepCacheConfig>,
    #[serde(default)]
    pub mounts: Vec<MountSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Channel {
    pub name: String,
    pub buffer_size: Option<usize>,
    /// Step id that WRITES this channel FIFO when the writer opens it via a raw
    /// argv path instead of an `IOTarget::Pipe` on its stdio. The worker's
    /// keep-alive machinery infers a channel's producer/consumer from
    /// `IOTarget::Pipe` targets (see `step_produced_channels`); a step that hands
    /// the FIFO path to its program as an argument -- the communication-evaluator
    /// manager opens 2 FIFOs per contestant this way -- is invisible to that scan,
    /// so it MUST name itself here or the channel gets no keep-alive and a peer
    /// that never opens its end wedges until the wall-time. `None` keeps the
    /// pure `IOTarget::Pipe` detection (e.g. the batch-evaluator fused-checker
    /// pipe, where both ends are stdio pipes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub producer_step: Option<String>,
    /// Step id that READS this channel FIFO via a raw argv path. Mirror of
    /// `producer_step`: lets the keep-alive release routine wait for this
    /// consumer's open before delivering EOF (preserving buffered bytes) even
    /// though the read end is argv-opened, not an `IOTarget::Pipe` on stdin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer_step: Option<String>,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            name: String::new(),
            buffer_size: Some(8192),
            producer_step: None,
            consumer_step: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationTask {
    pub environments: Vec<Environment>,
    pub tasks: Vec<Step>,
    #[serde(default)]
    pub channels: Vec<Channel>,
    /// When set and that worker is live, the host routes this operation to
    /// the worker's private stream instead of the shared one. Used by admin
    /// probe / pinned-rejudge flows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_worker_id: Option<String>,
    /// Evaluator-owned correlation: the evaluate batch this operation belongs
    /// to. When set together with `test_case_id`, the host records the
    /// dispatched task in its cancel registry so cancel_test_cases can later
    /// short-circuit pending work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluate_batch_id: Option<String>,
    /// Test case this operation evaluates. Paired with `evaluate_batch_id`
    /// to scope cancellation to a subset of in-flight work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_case_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartDetachedWindowedOperationInput {
    pub operations: Vec<OperationTask>,
    pub concurrency: usize,
    pub result_timeout_ms: u64,
    pub callback_fn: String,
    #[serde(default)]
    pub state: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedOperationSession {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedOperationCallbackInput {
    pub session_id: String,
    #[serde(default)]
    pub state: serde_json::Value,
    pub event: DetachedOperationCallbackEvent,
    pub completed: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DetachedOperationCallbackEvent {
    Result {
        operation_index: usize,
        result: OperationResult,
    },
    Timeout {
        message: String,
    },
    Exhausted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetachedOperationCallbackOutput {
    #[serde(default)]
    pub state: serde_json::Value,
    #[serde(default)]
    pub action: DetachedOperationCallbackAction,
    #[serde(default = "default_refill")]
    pub refill: bool,
    #[serde(default)]
    pub cancel_operation_indices: Vec<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DetachedOperationCallbackAction {
    #[default]
    Continue,
    Finish,
    Cancel,
}

impl DetachedOperationCallbackInput {
    pub fn result(&self) -> Option<(usize, &OperationResult)> {
        match &self.event {
            DetachedOperationCallbackEvent::Result {
                operation_index,
                result,
            } => Some((*operation_index, result)),
            DetachedOperationCallbackEvent::Timeout { .. }
            | DetachedOperationCallbackEvent::Exhausted => None,
        }
    }
}

impl DetachedOperationCallbackOutput {
    pub fn continue_with(state: serde_json::Value) -> Self {
        Self {
            state,
            action: DetachedOperationCallbackAction::Continue,
            refill: true,
            cancel_operation_indices: Vec::new(),
        }
    }

    pub fn finish(state: serde_json::Value) -> Self {
        Self {
            state,
            action: DetachedOperationCallbackAction::Finish,
            refill: false,
            cancel_operation_indices: Vec::new(),
        }
    }

    pub fn cancel(state: serde_json::Value) -> Self {
        Self {
            state,
            action: DetachedOperationCallbackAction::Cancel,
            refill: false,
            cancel_operation_indices: Vec::new(),
        }
    }

    pub fn refill(mut self, refill: bool) -> Self {
        self.refill = refill;
        self
    }

    pub fn refill_while(
        mut self,
        input: &DetachedOperationCallbackInput,
        predicate: impl FnOnce(usize, &OperationResult) -> bool,
    ) -> Self {
        self.refill = input
            .result()
            .is_some_and(|(operation_index, result)| predicate(operation_index, result));
        self
    }

    pub fn cancel_operation_indices(mut self, indices: impl IntoIterator<Item = usize>) -> Self {
        self.cancel_operation_indices = indices.into_iter().collect();
        self
    }
}

fn default_refill() -> bool {
    true
}

#[cfg(test)]
mod detached_tests {
    use super::*;

    #[test]
    fn detached_operation_output_refill_while_uses_result_predicate() {
        let input = DetachedOperationCallbackInput {
            session_id: "session".to_string(),
            state: serde_json::json!({}),
            event: DetachedOperationCallbackEvent::Result {
                operation_index: 2,
                result: OperationResult {
                    success: true,
                    task_results: HashMap::new(),
                    error: None,
                },
            },
            completed: 1,
            total: 3,
        };

        let output = DetachedOperationCallbackOutput::continue_with(serde_json::json!({}))
            .refill_while(&input, |operation_index, result| {
                operation_index == 2 && result.success
            });

        assert!(output.refill);
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct OperationResult {
    pub success: bool,
    pub task_results: HashMap<String, TaskExecutionResult>,
    pub error: Option<String>,
}

impl OperationResult {
    pub const CANCELLED_BY_HOST: &'static str = "Cancelled by host";

    pub fn cancelled_by_host() -> Self {
        Self {
            success: false,
            task_results: HashMap::new(),
            error: Some(Self::CANCELLED_BY_HOST.to_string()),
        }
    }

    pub fn is_cancelled_by_host(&self) -> bool {
        !self.success
            && self.task_results.is_empty()
            && self.error.as_deref() == Some(Self::CANCELLED_BY_HOST)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskExecutionResult {
    pub task_id: String,
    pub success: bool,
    pub sandbox_result: ExecutionResult,
    /// Blob hash of each uploaded [`Step::collect`] file, by name.
    #[serde(default)]
    pub collected_outputs: HashMap<String, String>,
}

/// Normalized sandbox termination status, mapped once at the worker sandbox
/// boundary from backend-specific signals (e.g. isolate's `meta` status codes).
/// Consumers match on this instead of the raw [`ExecutionResult::status`]
/// string, which is retained for diagnostics only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SandboxStatus {
    /// The program ran to completion and exited with code 0.
    Ok,
    /// The program exceeded its CPU or wall-clock time limit.
    TimedOut,
    /// The program was terminated by a signal.
    Signaled,
    /// The program exited with a non-zero exit code.
    NonZeroExit,
    /// The sandbox itself failed internally (isolate `XX`).
    InternalError,
    /// No status was recorded: the step never ran.
    #[default]
    Unknown,
}

impl SandboxStatus {
    /// Map an isolate `meta` status code (or the empty/`OK`/`UNKNOWN` sentinels)
    /// to a normalized status. This is the single place raw isolate status
    /// strings are interpreted.
    pub fn from_isolate(status: &str) -> Self {
        match status {
            "OK" => Self::Ok,
            "TO" => Self::TimedOut,
            "SG" => Self::Signaled,
            "RE" => Self::NonZeroExit,
            "XX" => Self::InternalError,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub signal: Option<i32>,
    #[serde(default)]
    pub time_used: f64,
    #[serde(default)]
    pub wall_time_used: f64,
    #[serde(default)]
    pub memory_used: Option<u32>,
    #[serde(default)]
    pub killed: bool,
    #[serde(default)]
    pub cg_oom_killed: bool,
    /// Raw backend status string (isolate `meta` code), kept for diagnostics.
    /// Match on [`ExecutionResult::sandbox_status`] for control flow instead.
    #[serde(default)]
    pub status: String,
    /// Normalized sandbox status, mapped once at the sandbox backend;
    /// [`SandboxStatus::Unknown`] for a step that never ran.
    #[serde(default)]
    pub sandbox_status: SandboxStatus,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
}

impl Default for ExecutionResult {
    fn default() -> Self {
        Self {
            exit_code: None,
            signal: None,
            time_used: 0.0,
            wall_time_used: 0.0,
            memory_used: None,
            killed: false,
            cg_oom_killed: false,
            status: "UNKNOWN".to_string(),
            sandbox_status: SandboxStatus::Unknown,
            message: String::new(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

pub type SandboxResult = ExecutionResult;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_status_maps_isolate_codes() {
        assert_eq!(SandboxStatus::from_isolate("OK"), SandboxStatus::Ok);
        assert_eq!(SandboxStatus::from_isolate("TO"), SandboxStatus::TimedOut);
        assert_eq!(SandboxStatus::from_isolate("SG"), SandboxStatus::Signaled);
        assert_eq!(
            SandboxStatus::from_isolate("RE"),
            SandboxStatus::NonZeroExit
        );
        assert_eq!(
            SandboxStatus::from_isolate("XX"),
            SandboxStatus::InternalError
        );
        assert_eq!(SandboxStatus::from_isolate(""), SandboxStatus::Unknown);
        assert_eq!(
            SandboxStatus::from_isolate("UNKNOWN"),
            SandboxStatus::Unknown
        );
    }

    #[test]
    fn step_kind_defaults_to_generic_when_omitted() {
        let step: Step = serde_json::from_value(serde_json::json!({
            "id": "plain",
            "env_ref": "sandbox",
            "argv": ["echo", "ok"],
            "conf": {},
            "io": {
                "stdin": {"type": "null"},
                "stdout": {"type": "null"},
                "stderr": {"type": "null"}
            },
            "collect": []
        }))
        .unwrap();

        assert_eq!(step.kind, StepKind::Generic);
    }

    #[test]
    fn step_kind_uses_snake_case_wire_values() {
        let value = serde_json::to_value(StepKind::CheckerCompile).unwrap();
        assert_eq!(value, serde_json::json!("checker_compile"));
    }

    #[test]
    fn mount_source_step_output_round_trips_with_tag() {
        let source = MountSource::StepOutput {
            from_step: "compile".to_string(),
            file: "checker".to_string(),
        };

        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["source"], "step_output");
        assert_eq!(json["from_step"], "compile");
        assert_eq!(json["file"], "checker");

        let back: MountSource = serde_json::from_value(json).unwrap();
        match back {
            MountSource::StepOutput { from_step, file } => {
                assert_eq!(from_step, "compile");
                assert_eq!(file, "checker");
            }
            _ => panic!("expected step_output variant"),
        }
    }

    #[test]
    fn mount_source_platform_tool_round_trips_with_tag() {
        let source = MountSource::PlatformTool {
            name: "testlib".to_string(),
        };

        let json = serde_json::to_value(&source).unwrap();
        assert_eq!(json["source"], "platform_tool");
        assert_eq!(json["name"], "testlib");

        let back: MountSource = serde_json::from_value(json).unwrap();
        match back {
            MountSource::PlatformTool { name } => assert_eq!(name, "testlib"),
            _ => panic!("expected platform_tool variant"),
        }
    }

    #[test]
    fn mount_spec_round_trips() {
        let spec = MountSpec {
            inside_path: "/sandbox/checker".to_string(),
            source: MountSource::StepOutput {
                from_step: "checker_compile".to_string(),
                file: "checker.bin".to_string(),
            },
        };

        let json = serde_json::to_value(&spec).unwrap();
        assert_eq!(json["inside_path"], "/sandbox/checker");
        assert_eq!(json["source"]["source"], "step_output");

        let back: MountSpec = serde_json::from_value(json).unwrap();
        assert_eq!(back.inside_path, "/sandbox/checker");
        match back.source {
            MountSource::StepOutput { from_step, file } => {
                assert_eq!(from_step, "checker_compile");
                assert_eq!(file, "checker.bin");
            }
            _ => panic!("expected step_output variant"),
        }
    }

    #[test]
    fn step_round_trips_with_mounts() {
        let step = Step {
            id: "checker".to_string(),
            kind: StepKind::Checker,
            env_ref: "sandbox".to_string(),
            argv: vec!["checker".to_string()],
            conf: RunOptions::default(),
            io: IOConfig::default(),
            collect: vec![],
            depends_on: vec![],
            cache: None,
            mounts: vec![MountSpec {
                inside_path: "/tool".to_string(),
                source: MountSource::PlatformTool {
                    name: "testlib".to_string(),
                },
            }],
        };

        let json = serde_json::to_value(&step).unwrap();
        let back: Step = serde_json::from_value(json).unwrap();
        assert_eq!(back.mounts.len(), 1);
        assert_eq!(back.mounts[0].inside_path, "/tool");
    }

    #[test]
    fn step_mounts_defaults_to_empty_when_omitted() {
        let step: Step = serde_json::from_value(serde_json::json!({
            "id": "plain",
            "env_ref": "sandbox",
            "argv": ["echo", "ok"],
            "conf": {},
            "io": {
                "stdin": {"type": "null"},
                "stdout": {"type": "null"},
                "stderr": {"type": "null"}
            },
            "collect": []
        }))
        .unwrap();

        assert!(step.mounts.is_empty());
    }
}
