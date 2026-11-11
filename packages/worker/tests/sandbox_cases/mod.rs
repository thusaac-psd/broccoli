//! Operation-execution cases shared by every sandbox backend.
//!
//! `worker_mock_sandbox.rs` and `worker_isolate_sandbox.rs` each implement
//! [`Backend`] for their `SandboxManager` and instantiate the whole case list
//! with [`sandbox_suite!`], so a case added here runs against both sandboxes.
//! Only the backend's own concerns differ: how a fresh manager is built and how
//! a step's command names a path mounted into its box. Every step runs on the
//! default run options (one process per box, which isolate enforces) unless the
//! case lifts the cap with [`multi_process`] because its shell must fork.

#![allow(dead_code)]

use common::worker::{Task, TaskResult};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use worker::models::operation::executor::OperationTaskExecutor;
use worker::models::operation::file_cacher::UnavailableFileCacher;
use worker::models::operation::handler::OperationHandler;
use worker::models::operation::models::{
    Channel, Environment, IOConfig, IOTarget, MountSource, MountSpec, OperationResult,
    OperationTask, SessionFile, Step, StepKind,
};
use worker::models::operation::sandbox::{
    DirectoryOptions, DirectoryRule, RunOptions, SandboxManager,
};
use worker::models::operation::task_cache::NoopTaskCacheStore;
use worker::models::worker::Worker;

/// A sandbox backend the shared cases run against.
pub trait Backend {
    /// A fresh sandbox manager for one operation.
    fn manager() -> Box<dyn SandboxManager + Send + Sync>;
    /// How a step's command names `path`, mounted at `/<path>` in its box.
    fn mounted(path: &str) -> String;
}

/// Instantiate every shared case as a `#[tokio::test]` for one backend. The
/// `all` attributes are applied to every generated test (e.g. the isolate
/// suite's `#[ignore]` and `#[serial]`); the `cpp` ones are added to the cases
/// that need a C++ compiler on the host (e.g. an `#[ignore]` keeping them
/// opt-in where the suite otherwise runs by default).
#[macro_export]
macro_rules! sandbox_suite {
    (@cases $backend:ty; $attrs:tt; $($case:ident),* $(,)?) => {
        $( $crate::sandbox_suite!(@case $backend; $attrs; $case); )*
    };
    (@case $backend:ty; [$(#[$attr:meta])*]; $case:ident) => {
        #[tokio::test]
        $(#[$attr])*
        async fn $case() {
            sandbox_cases::$case::<$backend>().await;
        }
    };
    ($backend:ty; all: [$(#[$attr:meta]),* $(,)?]; cpp: [$(#[$cpp:meta]),* $(,)?] $(,)?) => {
        $crate::sandbox_suite!(@cases $backend; [$(#[$attr])* $(#[$cpp])*];
            cpp_oi_pipeline_with_io_redirection,
            cpp_compile_error_skips_dependent_step,
        );
        $crate::sandbox_suite!(@cases $backend; [$(#[$attr])*];
            echo_succeeds,
            nonzero_exit_fails_the_step,
            large_stdout_is_returned_as_capped_preview,
            blob_input_fails_when_blob_storage_is_unavailable,
            platform_tool_mount_executes_tool_inside_box,
            platform_tool_mount_without_configured_tools_dir_fails_the_step,
            empty_pipe_name_fails_the_step,
            two_envs_share_a_mapped_directory,
            shared_channel_fifo_between_two_environments,
            channel_pipe_io_redirect_between_environments,
            non_channel_pipe_still_works_with_channels_present,
            step_output_mount_exposes_only_that_file_to_dependent_step,
            step_output_mount_without_declared_dependency_fails_the_step,
            stdin_fifo_consumer_observes_eof_when_writer_closes,
            fused_builtin_stream_accepts_matching_output,
            fused_builtin_stream_rejects_mismatched_output,
            fused_solution_env_cannot_read_answer,
        );
    };
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh, not yet created, temp path unique to this process and call.
pub fn unique_temp_dir(prefix: &str) -> PathBuf {
    let counter = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "broccoli-{prefix}-{}-{counter}-{ts}",
        std::process::id()
    ))
}

fn set_mode(path: &Path, mode: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A fresh tools dir the sandbox user can traverse.
fn tools_dir() -> PathBuf {
    let dir = unique_temp_dir("tools-dir-test");
    std::fs::create_dir_all(&dir).unwrap();
    set_mode(&dir, 0o755);
    dir
}

/// An absolute path to a C++ compiler on PATH. The C++ cases are opt-in on
/// every backend, so a missing compiler is a misconfigured run, not a skip.
pub fn cpp_compiler() -> String {
    find_cpp_compiler().expect("no C++ compiler found on PATH")
}

fn find_cpp_compiler() -> Option<String> {
    ["c++", "clang++", "g++"].into_iter().find_map(|compiler| {
        let output = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("command -v {compiler}"))
            .output()
            .ok()?;
        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (output.status.success() && !path.is_empty()).then_some(path)
    })
}

/// Copy the built `broccoli-compare` binary into a fresh tools dir for a
/// PlatformTool mount. `None` (skip the case) if the comparator isn't built:
/// build it with `cargo build -p broccoli-compare` first.
fn broccoli_compare_tools_dir() -> Option<PathBuf> {
    let manifest = env!("CARGO_MANIFEST_DIR"); // .../packages/worker
    let src = ["debug", "release"]
        .into_iter()
        .map(|p| {
            PathBuf::from(manifest)
                .join("../../target")
                .join(p)
                .join("broccoli-compare")
        })
        .find(|p| p.exists())?;
    let dir = tools_dir();
    let dst = dir.join("broccoli-compare");
    std::fs::copy(&src, &dst).unwrap();
    set_mode(&dst, 0o755);
    Some(dir)
}

fn env(id: &str) -> Environment {
    Environment {
        id: id.to_string(),
        files_in: vec![],
    }
}

/// A generic `/bin/sh -c <command>` step in `env_ref`, on the default run
/// options.
fn sh(id: &str, env_ref: &str, command: &str) -> Step {
    Step {
        id: id.to_string(),
        kind: StepKind::Generic,
        env_ref: env_ref.to_string(),
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()],
        conf: RunOptions::default(),
        io: IOConfig::default(),
        collect: vec![],
        depends_on: vec![],
        cache: None,
        mounts: vec![],
    }
}

/// Lift the one-process-per-box limit (0 means no cap) for a step whose shell
/// must fork: a pipeline, a compiler, or an external command that is not the
/// shell's last. The mock ignores the limit; isolate enforces it.
fn multi_process(mut step: Step) -> Step {
    step.conf.resource_limits.process_limit = Some(0);
    step
}

fn operation(
    environments: Vec<Environment>,
    tasks: Vec<Step>,
    channels: Vec<Channel>,
) -> OperationTask {
    OperationTask {
        environments,
        tasks,
        channels,
        priority: None,
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    }
}

/// A single `/bin/sh -c <command>` step in a single environment.
fn single_step(command: &str) -> OperationTask {
    operation(
        vec![env("env-1")],
        vec![sh("step-1", "env-1", command)],
        vec![],
    )
}

fn channel(name: &str) -> Channel {
    Channel {
        name: name.to_string(),
        buffer_size: Some(8192),
        ..Channel::default()
    }
}

fn pipe(name: &str) -> IOTarget {
    IOTarget::Pipe {
        name: name.to_string(),
    }
}

fn file(path: &str) -> IOTarget {
    IOTarget::File {
        path: path.to_string(),
    }
}

fn platform_tool(name: &str) -> MountSpec {
    MountSpec {
        inside_path: format!("/tools/{name}"),
        source: MountSource::PlatformTool {
            name: name.to_string(),
        },
    }
}

/// Run `operation` through a worker's operation executor.
async fn run_task<B: Backend>(
    task_id: &str,
    operation: OperationTask,
) -> (TaskResult, OperationResult) {
    let (metrics, _registry) = common::observability::init_metrics("broccoli-worker-test");
    let worker = Worker::with_no_executors();
    worker.register_executor(
        "operation",
        Arc::new(OperationTaskExecutor::new_with_sandbox_manager(
            B::manager(),
            metrics,
        )),
    );
    let task = Task {
        id: task_id.to_string(),
        task_type: "operation".to_string(),
        executor_name: "operation".to_string(),
        payload: serde_json::to_value(operation).unwrap(),
        result_queue: "test_results".into(),
        operation_batch_id: None,
        reply_queue: None,
        priority: None,
        trace_context: None,
        enqueued_at_unix_ms: None,
    };

    let result = worker.execute_task(task).await.unwrap();
    let operation_result: OperationResult = serde_json::from_value(result.output.clone())
        .unwrap_or_else(|e| {
            panic!(
                "Failed to deserialize OperationResult: {e}\nraw output: {}",
                result.output
            )
        });
    (result, operation_result)
}

/// An operation handler on the backend with no blob storage, optionally with a
/// platform tools dir.
fn handler<B: Backend>(tools_dir: Option<PathBuf>) -> OperationHandler {
    let (metrics, _registry) = common::observability::init_metrics("broccoli-worker-test");
    OperationHandler::new(
        B::manager(),
        Box::new(UnavailableFileCacher::new("no blobs needed")),
        Box::new(NoopTaskCacheStore),
        String::new(),
        metrics,
    )
    .with_tools_dir(tools_dir)
}

// ---------------------------------------------------------------------------
// Single-step basics
// ---------------------------------------------------------------------------

pub async fn echo_succeeds<B: Backend>() {
    let (result, operation_result) =
        run_task::<B>("task-success", single_step("echo sandbox-ok")).await;
    assert!(result.success, "task failed: {:?}", operation_result);
    assert!(
        operation_result.success,
        "operation failed: {:?}",
        operation_result
    );

    let step_result = operation_result.task_results.get("step-1").unwrap();
    assert!(step_result.success);
    assert_eq!(step_result.sandbox_result.exit_code, Some(0));
    assert!(step_result.sandbox_result.stdout.contains("sandbox-ok"));
}

pub async fn nonzero_exit_fails_the_step<B: Backend>() {
    let (result, operation_result) = run_task::<B>("task-failure", single_step("exit 17")).await;
    assert!(!result.success);
    assert!(!operation_result.success);

    let step_result = operation_result.task_results.get("step-1").unwrap();
    assert!(!step_result.success);
    assert_eq!(step_result.sandbox_result.exit_code, Some(17));
}

pub async fn large_stdout_is_returned_as_capped_preview<B: Backend>() {
    let command = "awk 'BEGIN { for (i = 0; i < 70000; i++) printf \"a\" }'";
    let (_result, operation_result) =
        run_task::<B>("task-large-stdout", single_step(command)).await;

    let step_result = operation_result.task_results.get("step-1").unwrap();
    assert!(step_result.success);
    assert!(step_result.sandbox_result.stdout.len() < 70_000);
    assert!(
        step_result
            .sandbox_result
            .stdout
            .contains("... (truncated)")
    );
}

pub async fn blob_input_fails_when_blob_storage_is_unavailable<B: Backend>() {
    let (metrics, _registry) = common::observability::init_metrics("broccoli-worker-test");
    let handler = OperationHandler::new(
        B::manager(),
        Box::new(UnavailableFileCacher::new("test storage unavailable")),
        Box::new(NoopTaskCacheStore),
        String::new(),
        metrics,
    );

    let mut operation = single_step("cat input.txt");
    operation.environments[0].files_in.push((
        "input.txt".to_string(),
        SessionFile::Blob {
            hash: "a".repeat(64),
        },
    ));

    let err = handler
        .execute(&operation)
        .await
        .expect_err("blob-backed operation input must fail when storage is unavailable");
    assert!(format!("{err:?}").contains("blob storage is unavailable"));
}

pub async fn empty_pipe_name_fails_the_step<B: Backend>() {
    let mut step = sh("pipe-invalid", "env-1", "echo hi");
    step.io.stdout = pipe("");
    let operation = operation(vec![env("env-1")], vec![step], vec![]);

    let (result, operation_result) = run_task::<B>("task-pipe-invalid", operation).await;
    assert!(!result.success);
    assert!(!operation_result.success);

    let invalid_result = operation_result.task_results.get("pipe-invalid").unwrap();
    assert!(!invalid_result.success);
    assert_eq!(invalid_result.sandbox_result.status, "UNKNOWN");
    assert_eq!(invalid_result.sandbox_result.exit_code, None);
}

// ---------------------------------------------------------------------------
// Platform tool mounts (MountSource::PlatformTool resolves to a read-only
// mount of a worker-configured tool, which the step can then execute)
// ---------------------------------------------------------------------------

pub async fn platform_tool_mount_executes_tool_inside_box<B: Backend>() {
    // A worker-local tools directory holding one executable tool.
    let tools_dir = tools_dir();
    let tool_path = tools_dir.join("mytool");
    std::fs::write(&tool_path, "#!/bin/sh\necho tool-ran\n").unwrap();
    set_mode(&tool_path, 0o755);
    let handler = handler::<B>(Some(tools_dir.clone()));

    // The step execs the tool directly via its in-box mount path; the worker
    // resolves the PlatformTool name to <tools_dir>/mytool and mounts it
    // read-only.
    let mut operation = single_step("");
    operation.tasks[0].argv = vec![B::mounted("tools/mytool")];
    operation.tasks[0].mounts = vec![platform_tool("mytool")];

    let result = handler.execute(&operation).await.unwrap();
    let step = result.task_results.get("step-1").unwrap();
    assert!(step.success, "tool step failed: {:?}", step.sandbox_result);
    assert!(
        step.sandbox_result.stdout.contains("tool-ran"),
        "tool did not run; stdout: {:?}",
        step.sandbox_result.stdout
    );

    let _ = std::fs::remove_dir_all(&tools_dir);
}

pub async fn platform_tool_mount_without_configured_tools_dir_fails_the_step<B: Backend>() {
    // No tools dir: a PlatformTool mount cannot be resolved.
    let handler = handler::<B>(None);

    let mut operation = single_step("true");
    operation.tasks[0].mounts = vec![platform_tool("mytool")];

    let result = handler.execute(&operation).await.unwrap();
    let step = result.task_results.get("step-1").unwrap();
    assert!(
        !step.success,
        "step must fail when a platform tool is requested but no tools dir is configured"
    );
}

// ---------------------------------------------------------------------------
// Compile-and-run pipelines (opt-in: need a C++ compiler on the host, and
// every step forks)
// ---------------------------------------------------------------------------

pub async fn cpp_oi_pipeline_with_io_redirection<B: Backend>() {
    let compiler = cpp_compiler();

    let setup_script = r#"
cat > main.cpp <<'CPP'
#include <iostream>
using namespace std;

int main() {
    ios::sync_with_stdio(false);
    cin.tie(nullptr);

    long long a, b;
    if (!(cin >> a >> b)) {
        cerr << "bad-input" << endl;
        return 2;
    }

    cout << (a + b) << "\n";
    cerr << "judge-log" << endl;
    return 0;
}
CPP
printf '2 40\n' > input.txt
"#;

    let mut run = sh("run", "env-1", "");
    run.argv = vec!["./main".to_string()];
    run.io = IOConfig {
        stdin: file("input.txt"),
        stdout: file("output.txt"),
        stderr: file("error.txt"),
    };
    run.depends_on = vec!["compile".to_string()];

    let mut compile = sh(
        "compile",
        "env-1",
        &format!("{compiler} -std=c++17 -O2 main.cpp -o main"),
    );
    compile.depends_on = vec!["prepare".to_string()];

    let mut verify = sh(
        "verify",
        "env-1",
        "test -x main; echo executable:$?; test -f output.txt; echo output_file:$?; \
         test -f error.txt; echo error_file:$?; grep -qx '42' output.txt; \
         echo output_content:$?; exit 0",
    );
    verify.depends_on = vec!["run".to_string()];

    let operation = operation(
        vec![env("env-1")],
        [sh("prepare", "env-1", setup_script), compile, run, verify]
            .into_iter()
            .map(multi_process)
            .collect(),
        vec![],
    );

    let (result, operation_result) = run_task::<B>("task-cpp-oi", operation).await;

    assert!(
        result.success,
        "task result failed: {:#?}",
        operation_result
    );
    assert!(
        operation_result.success,
        "operation failed: {:#?}",
        operation_result
    );

    for step_id in ["prepare", "compile", "run", "verify"] {
        let step_result = operation_result.task_results.get(step_id).unwrap();
        assert!(step_result.success, "step {step_id} should succeed");
    }

    let run_result = operation_result.task_results.get("run").unwrap();
    assert_eq!(run_result.sandbox_result.exit_code, Some(0));

    let verify_stdout = &operation_result
        .task_results
        .get("verify")
        .unwrap()
        .sandbox_result
        .stdout;
    assert!(verify_stdout.contains("executable:0"));
    assert!(verify_stdout.contains("output_file:0"));
    assert!(verify_stdout.contains("error_file:0"));
    assert!(verify_stdout.contains("output_content:0"));
}

pub async fn cpp_compile_error_skips_dependent_step<B: Backend>() {
    let compiler = cpp_compiler();

    let bad_cpp_script = r#"
cat > bad.cpp <<'CPP'
#include <iostream>
int main() {
    std::cout << "missing semicolon" << std::endl
    return 0;
}
CPP
"#;

    let mut compile = sh(
        "compile-bad",
        "env-1",
        &format!("{compiler} -std=c++17 bad.cpp -o bad"),
    );
    compile.depends_on = vec!["prepare-bad".to_string()];
    let mut skipped = sh("run-should-skip", "env-1", "echo should-not-run");
    skipped.depends_on = vec!["compile-bad".to_string()];

    let operation = operation(
        vec![env("env-1")],
        [sh("prepare-bad", "env-1", bad_cpp_script), compile, skipped]
            .into_iter()
            .map(multi_process)
            .collect(),
        vec![],
    );

    let (result, operation_result) = run_task::<B>("task-cpp-compile-error", operation).await;

    assert!(!result.success);
    assert!(!operation_result.success);

    let compile_result = operation_result.task_results.get("compile-bad").unwrap();
    assert!(!compile_result.success);
    assert_ne!(compile_result.sandbox_result.exit_code, Some(0));

    let skipped_result = operation_result
        .task_results
        .get("run-should-skip")
        .unwrap();
    assert!(!skipped_result.success);
    assert_eq!(skipped_result.sandbox_result.exit_code, None);
    assert_eq!(skipped_result.sandbox_result.status, "UNKNOWN");
}

// ---------------------------------------------------------------------------
// Cross-environment sharing: mapped directories and channel FIFOs
// ---------------------------------------------------------------------------

pub async fn two_envs_share_a_mapped_directory<B: Backend>() {
    let shared_dir = unique_temp_dir("shared-dir-test");
    std::fs::create_dir_all(&shared_dir).unwrap();
    set_mode(&shared_dir, 0o777);

    let shared_rule = DirectoryRule {
        inside_path: PathBuf::from("/shared"),
        outside_path: Some(shared_dir.clone()),
        options: DirectoryOptions {
            read_write: true,
            ..Default::default()
        },
    };
    let msg = B::mounted("shared/msg.txt");

    let mut producer = sh(
        "producer",
        "env-a",
        &format!("printf 'hello-from-env-a' > {msg}"),
    );
    producer.conf.directory_rules = vec![shared_rule.clone()];
    let mut consumer = sh(
        "consumer",
        "env-b",
        &format!("test -f {msg} && grep -qx 'hello-from-env-a' {msg}"),
    );
    consumer.conf.directory_rules = vec![shared_rule];
    consumer.depends_on = vec!["producer".to_string()];

    let operation = operation(
        vec![env("env-a"), env("env-b")],
        // `test && grep` forks grep before the shell exits.
        vec![producer, multi_process(consumer)],
        vec![],
    );

    let (result, operation_result) = run_task::<B>("task-shared-dir-two-env", operation).await;

    assert!(result.success, "task failed: {:?}", operation_result);
    assert!(operation_result.success);

    let producer = operation_result.task_results.get("producer").unwrap();
    assert!(producer.success);
    assert_eq!(producer.sandbox_result.exit_code, Some(0));

    let consumer = operation_result.task_results.get("consumer").unwrap();
    assert!(consumer.success);
    assert_eq!(consumer.sandbox_result.exit_code, Some(0));

    let _ = std::fs::remove_dir_all(&shared_dir);
}

pub async fn shared_channel_fifo_between_two_environments<B: Backend>() {
    let fifo = B::mounted("channels/pipe1");
    let operation = operation(
        vec![env("writer-env"), env("reader-env")],
        vec![
            sh(
                "writer",
                "writer-env",
                &format!("printf 'hello-via-fifo' > {fifo}"),
            ),
            sh("reader", "reader-env", &format!("cat {fifo}")),
        ]
        .into_iter()
        .map(multi_process)
        .collect(),
        vec![channel("pipe1")],
    );

    let (result, operation_result) = run_task::<B>("task-shared-channel", operation).await;

    assert!(result.success, "task failed: {:?}", operation_result);
    assert!(operation_result.success);

    let writer = operation_result.task_results.get("writer").unwrap();
    assert!(writer.success, "writer should succeed");

    let reader = operation_result.task_results.get("reader").unwrap();
    assert!(reader.success, "reader should succeed");
    assert!(
        reader.sandbox_result.stdout.contains("hello-via-fifo"),
        "reader should receive data written via channel FIFO, got: '{}'",
        reader.sandbox_result.stdout
    );
}

pub async fn channel_pipe_io_redirect_between_environments<B: Backend>() {
    let mut producer = sh("producer", "producer-env", "echo channel-data-42");
    producer.io.stdout = pipe("data_pipe");
    let mut consumer = sh("consumer", "consumer-env", "head -n1");
    consumer.io.stdin = pipe("data_pipe");

    let operation = operation(
        vec![env("producer-env"), env("consumer-env")],
        vec![multi_process(producer), multi_process(consumer)],
        vec![channel("data_pipe")],
    );

    let (result, operation_result) = run_task::<B>("task-channel-io-redirect", operation).await;

    assert!(result.success, "task failed: {:?}", operation_result);
    assert!(operation_result.success);

    let consumer = operation_result.task_results.get("consumer").unwrap();
    assert!(consumer.success, "consumer should succeed");
    assert!(
        consumer.sandbox_result.stdout.contains("channel-data-42"),
        "consumer should receive data via channel IO redirect, got: '{}'",
        consumer.sandbox_result.stdout
    );
}

pub async fn non_channel_pipe_still_works_with_channels_present<B: Backend>() {
    let mut writer = sh("writer", "env-1", "echo local-pipe-data");
    writer.io.stdout = pipe("local_pipe");
    let mut reader = sh("reader", "env-1", "head -n1");
    reader.io.stdin = pipe("local_pipe");

    let operation = operation(
        vec![env("env-1")],
        vec![multi_process(writer), multi_process(reader)],
        vec![channel("some_other_channel")],
    );

    let (result, operation_result) =
        run_task::<B>("task-local-pipe-with-channels", operation).await;

    assert!(result.success, "task failed: {:?}", operation_result);
    assert!(operation_result.success);

    let reader = operation_result.task_results.get("reader").unwrap();
    assert!(reader.success, "reader should succeed");
    assert!(
        reader.sandbox_result.stdout.contains("local-pipe-data"),
        "non-channel pipe should still work, got: '{}'",
        reader.sandbox_result.stdout
    );
}

// A streamed checker (broccoli-compare, testlib) reads the solution output on
// stdin and drains it to EOF. EOF on a FIFO requires every write end to close,
// so the consumer's OWN stdin fd must be read-only - a read+write stdin keeps a
// writer open and the consumer hangs forever. (`head -n1` consumers elsewhere
// exit early and never exercise EOF, so they don't catch this.)
pub async fn stdin_fifo_consumer_observes_eof_when_writer_closes<B: Backend>() {
    // Fail fast (kill -> success=false) instead of hanging the suite forever if
    // the consumer never sees EOF.
    let guard = |step: Step| {
        let mut step = multi_process(step);
        step.conf.resource_limits.wall_time_limit = Some(15.0);
        step
    };
    let mut producer = sh("producer", "producer-env", "printf 'streamed-to-eof\\n'");
    producer.io.stdout = pipe("s");
    // `cat` reads stdin to EOF: it only terminates once the writer closes AND
    // the consumer holds no write end of its own.
    let mut consumer = sh("consumer", "consumer-env", "cat");
    consumer.io.stdin = pipe("s");
    consumer.io.stdout = file("got.txt");

    let operation = operation(
        vec![env("producer-env"), env("consumer-env")],
        vec![guard(producer), guard(consumer)],
        vec![channel("s")],
    );

    let (_result, operation_result) = run_task::<B>("task-stdin-fifo-eof", operation).await;

    let consumer = operation_result.task_results.get("consumer").unwrap();
    assert!(
        consumer.success,
        "an EOF-draining stdin consumer must terminate at EOF, not hang: {:?}",
        consumer.sandbox_result
    );
    assert!(
        consumer.sandbox_result.stdout.contains("streamed-to-eof"),
        "consumer should have read the streamed bytes; got: {:?}",
        consumer.sandbox_result.stdout
    );
}

// ---------------------------------------------------------------------------
// Step output mounts: MountSource::StepOutput is the intra-op handoff. A
// dependent step reads a prior step's captured output file directly from that
// step's working dir, with NO blob round-trip, and sees ONLY that file
// (read-only), never the producer's whole environment.
// ---------------------------------------------------------------------------

fn step_output_op<B: Backend>(
    producer_cmd: &str,
    consumer_cmd: &str,
    consumer_deps: Vec<String>,
) -> OperationTask {
    // out.txt is NOT collected -> it is never uploaded as a blob.
    let producer = sh("producer", "a", producer_cmd);
    let mut consumer = sh("consumer", "b", consumer_cmd);
    consumer.depends_on = consumer_deps;
    // A step output is staged as a copy in the consumer's working dir, so its
    // mount point is box-relative on every backend.
    consumer.mounts = vec![MountSpec {
        inside_path: "in.txt".to_string(),
        source: MountSource::StepOutput {
            from_step: "producer".to_string(),
            file: "out.txt".to_string(),
        },
    }];
    operation(vec![env("a"), env("b")], vec![producer, consumer], vec![])
}

pub async fn step_output_mount_exposes_only_that_file_to_dependent_step<B: Backend>() {
    let handler = handler::<B>(None);
    let operation = step_output_op::<B>(
        "printf payload-xyz > out.txt; printf SECRET > other.txt",
        // Read the mounted file, then prove the producer's OTHER files are absent.
        // Shell builtins only, so the step needs no second process.
        "IFS= read -r got < in.txt; printf %s \"$got\"; \
         if [ -e other.txt ] || [ -e out.txt ]; then printf ' LEAK'; fi",
        vec!["producer".to_string()],
    );

    let result = handler.execute(&operation).await.unwrap();

    let producer = result.task_results.get("producer").unwrap();
    assert!(
        producer.success,
        "producer failed: {:?}",
        producer.sandbox_result
    );
    let consumer = result.task_results.get("consumer").unwrap();
    assert!(
        consumer.success,
        "consumer failed: {:?}",
        consumer.sandbox_result
    );
    // The dependent step reads the producer's output through the mount...
    assert!(
        consumer.sandbox_result.stdout.contains("payload-xyz"),
        "consumer did not see the mounted output; stdout: {:?}",
        consumer.sandbox_result.stdout
    );
    // ...but sees ONLY that file - not the producer's other files.
    assert!(
        !consumer.sandbox_result.stdout.contains("LEAK"),
        "consumer leaked producer files: {:?}",
        consumer.sandbox_result.stdout
    );
}

pub async fn step_output_mount_without_declared_dependency_fails_the_step<B: Backend>() {
    let handler = handler::<B>(None);
    // Consumer mounts the producer's output but omits it from depends_on.
    let operation = step_output_op::<B>(
        "printf payload > out.txt",
        "cat in.txt",
        vec![], // <-- no dependency declared
    );

    let result = handler.execute(&operation).await.unwrap();
    let consumer = result.task_results.get("consumer").unwrap();
    assert!(
        !consumer.success,
        "a StepOutput mount without a declared dependency must fail the step"
    );
}

// ---------------------------------------------------------------------------
// Checker fusion: a REAL fused operation runs the REAL broccoli-compare
// comparator. Exercises the three primitives fusion depends on: a FIFO
// streaming the solution output -> comparator (Stream mode), broccoli-compare
// provided via a PlatformTool mount, and the answer living ONLY in the checker
// env. The exit-code -> Verdict mapping is unit-tested separately (SDK
// interpret_fused_result + standard-checkers interpret_builtin); here we prove
// the comparator receives the streamed output and emits the right exit code,
// and that the display preview is captured inline.
// ---------------------------------------------------------------------------

/// A Stream-mode fused op mirroring the resolver's stage: a `solution` step
/// runs `solution_cmd` and pipes its stdout to a `check` step that runs
/// broccoli-compare (mounted from the tools dir) against `answer` (a file in
/// the checker env ONLY). `exec_stderr_file`, when set, captures the solution
/// step's stderr (for the answer-isolation probe, since its stdout goes to the
/// FIFO and isn't captured).
fn fused_stream_op<B: Backend>(
    solution_cmd: &str,
    answer: &str,
    mode: &str,
    exec_stderr_file: Option<&str>,
) -> OperationTask {
    // Fail-fast guard: a wall-time limit turns any future FIFO deadlock into a
    // quick timeout (killed -> exit_code None -> AC assertion fails) instead of
    // an infinite hang that wedges the whole suite.
    let mut guarded = RunOptions::default();
    guarded.resource_limits.wall_time_limit = Some(20.0);

    let mut exec = sh("exec", "solution", solution_cmd);
    exec.conf = guarded.clone();
    // Solution output streams to the checker via a FIFO; the full output is
    // never written to a file or collected.
    exec.io.stdout = pipe("sol_out");
    if let Some(path) = exec_stderr_file {
        exec.io.stderr = file(path);
    }

    let check = Step {
        id: "check".to_string(),
        kind: StepKind::Checker,
        env_ref: "checker".to_string(),
        argv: vec![
            B::mounted("tools/broccoli-compare"),
            "--mode".to_string(),
            mode.to_string(),
            "--answer".to_string(),
            "answer.txt".to_string(),
        ],
        conf: guarded,
        io: IOConfig {
            stdin: pipe("sol_out"),
            // broccoli-compare: stdout = preview, stderr = message.
            stdout: file("preview.txt"),
            stderr: file("msg.txt"),
        },
        collect: vec![],
        // Concurrent with exec (Stream): no dependency on it.
        depends_on: vec![],
        cache: None,
        mounts: vec![platform_tool("broccoli-compare")],
    };

    let mut checker_env = env("checker");
    checker_env.files_in = vec![(
        "answer.txt".to_string(),
        SessionFile::Content {
            content: answer.to_string(),
        },
    )];

    operation(
        vec![env("solution"), checker_env],
        vec![exec, check],
        vec![channel("sol_out")],
    )
}

pub async fn fused_builtin_stream_accepts_matching_output<B: Backend>() {
    let Some(tools_dir) = broccoli_compare_tools_dir() else {
        eprintln!("skipping: broccoli-compare not built (cargo build -p broccoli-compare)");
        return;
    };
    let handler = handler::<B>(Some(tools_dir.clone()));

    let op = fused_stream_op::<B>("printf '3 1 4\\n'", "3 1 4\n", "tokens", None);
    let result = handler.execute(&op).await.unwrap();

    let check = result.task_results.get("check").unwrap();
    assert!(
        check.success,
        "check step failed: {:?}",
        check.sandbox_result
    );
    // Matching output -> broccoli-compare exits 0 (Accepted).
    assert_eq!(
        check.sandbox_result.exit_code,
        Some(0),
        "expected AC (exit 0); msg: {:?}",
        check.sandbox_result.stderr
    );
    // The solution output streamed through the FIFO into the comparator, which
    // tee'd it to stdout -> preview.txt -> read back inline.
    assert!(
        check.sandbox_result.stdout.contains("3 1 4"),
        "preview should carry the streamed solution output; got: {:?}",
        check.sandbox_result.stdout
    );
    // The full solution output is never collected (no blob upload).
    let exec = result.task_results.get("exec").unwrap();
    assert!(
        exec.collected_outputs.is_empty(),
        "solution output must not be collected/uploaded: {:?}",
        exec.collected_outputs
    );

    let _ = std::fs::remove_dir_all(&tools_dir);
}

pub async fn fused_builtin_stream_rejects_mismatched_output<B: Backend>() {
    let Some(tools_dir) = broccoli_compare_tools_dir() else {
        eprintln!("skipping: broccoli-compare not built (cargo build -p broccoli-compare)");
        return;
    };
    let handler = handler::<B>(Some(tools_dir.clone()));

    let op = fused_stream_op::<B>("printf '9 9 9\\n'", "3 1 4\n", "tokens", None);
    let result = handler.execute(&op).await.unwrap();

    let check = result.task_results.get("check").unwrap();
    // Mismatch -> exit 1 (WrongAnswer).
    assert_eq!(
        check.sandbox_result.exit_code,
        Some(1),
        "expected WA (exit 1); msg: {:?}",
        check.sandbox_result.stderr
    );
    // Preview is still captured even when the verdict is decided early.
    assert!(
        check.sandbox_result.stdout.contains("9 9 9"),
        "preview should be captured even on WA; got: {:?}",
        check.sandbox_result.stdout
    );

    let _ = std::fs::remove_dir_all(&tools_dir);
}

pub async fn fused_solution_env_cannot_read_answer<B: Backend>() {
    let Some(tools_dir) = broccoli_compare_tools_dir() else {
        eprintln!("skipping: broccoli-compare not built (cargo build -p broccoli-compare)");
        return;
    };
    let handler = handler::<B>(Some(tools_dir.clone()));

    // A "cheating" solution probes for the answer in its own env. The answer is
    // ONLY in the checker env, so every probe must miss. Its real output (to the
    // FIFO) still matches, so the verdict is AC - proving isolation does not
    // depend on the solution failing.
    let cheat = "for f in answer.txt expected expected_output.txt ../answer.txt /answer.txt; do \
                 [ -e \"$f\" ] && echo LEAK >&2; done; printf '3 1 4\\n'";
    let op = fused_stream_op::<B>(cheat, "3 1 4\n", "tokens", Some("exec_err.txt"));
    let result = handler.execute(&op).await.unwrap();

    let exec = result.task_results.get("exec").unwrap();
    assert!(
        !exec.sandbox_result.stderr.contains("LEAK"),
        "solution env must NOT contain the answer file; stderr: {:?}",
        exec.sandbox_result.stderr
    );
    assert!(
        exec.collected_outputs.is_empty(),
        "solution output must not be collected: {:?}",
        exec.collected_outputs
    );
    let check = result.task_results.get("check").unwrap();
    assert_eq!(
        check.sandbox_result.exit_code,
        Some(0),
        "verdict should still be AC; msg: {:?}",
        check.sandbox_result.stderr
    );

    let _ = std::fs::remove_dir_all(&tools_dir);
}
