//! What the real isolate sandbox costs per judging step, measured through the
//! worker's own sandbox and handler code.
//!
//! isolate needs cgroup v2 delegation and a setuid binary, so this cannot run
//! in a development container; the `isolate-bench` workflow runs it on a
//! GitHub runner. Every test is `#[ignore]`d and `#[serial]` (box ids are a
//! host-wide resource), and prints one JSON object per shape, so a run's own
//! shapes and a before/after pair are comparable within one run. Runners are
//! shared, noisy VMs: read the spread, not the absolute numbers.

mod sandbox_cases;

use sandbox_cases::{Backend, cpp_compiler, unique_temp_dir};
use serial_test::serial;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use worker::WorkerAppConfig;
use worker::models::operation::handler::OperationHandler;
use worker::models::operation::models::{
    Channel, Environment, IOConfig, IOTarget, OperationTask, RunOptions, SessionFile, Step,
    StepKind,
};
use worker::models::operation::sandbox::SandboxManager;
use worker::models::operation::sandbox::isolate::IsolateSandboxManager;

/// Iterations per shape. Enough for a stable p95 on a runner without making
/// the job long: one whole test case is a few hundred milliseconds.
const ITERATIONS: usize = 200;
/// Iterations for shapes that compile or run many boxes, which cost far more
/// per iteration.
const HEAVY_ITERATIONS: usize = 20;

fn isolate_bin() -> String {
    WorkerAppConfig::load()
        .map(|cfg| cfg.worker.isolate_bin)
        .unwrap_or_else(|_| "isolate".to_string())
}

struct Isolate;

impl Backend for Isolate {
    fn manager() -> Box<dyn SandboxManager + Send + Sync> {
        Box::new(IsolateSandboxManager::new(isolate_bin(), true))
    }

    fn mounted(path: &str) -> String {
        format!("/{path}")
    }
}

/// Print one shape's timings as a JSON line the workflow renders as a table.
fn report(shape: &str, mut samples: Vec<Duration>) {
    assert!(!samples.is_empty(), "{shape} produced no samples");
    samples.sort_unstable();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    let at = |q: f64| ms(samples[((samples.len() - 1) as f64 * q).round() as usize]);
    let total: f64 = samples.iter().copied().map(ms).sum();
    println!(
        "ISOLATE_BENCH {{\"shape\":\"{shape}\",\"n\":{},\"mean_ms\":{:.3},\"p50_ms\":{:.3},\
         \"p95_ms\":{:.3},\"p99_ms\":{:.3},\"min_ms\":{:.3},\"max_ms\":{:.3}}}",
        samples.len(),
        total / samples.len() as f64,
        at(0.50),
        at(0.95),
        at(0.99),
        ms(samples[0]),
        ms(samples[samples.len() - 1]),
    );
}

async fn time_each<F, Fut>(iterations: usize, mut once: F) -> Vec<Duration>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let start = Instant::now();
        once(i).await;
        samples.push(start.elapsed());
    }
    samples
}

fn handler(tools_dir: Option<PathBuf>) -> OperationHandler {
    let (metrics, _registry) = common::observability::init_metrics("broccoli-worker-bench");
    OperationHandler::new(
        Isolate::manager(),
        Box::new(worker::models::operation::file_cacher::NoopFileCacher),
        Box::new(worker::models::operation::task_cache::NoopTaskCacheStore),
        worker::toolchain_fingerprint::ToolchainFingerprint::Known("bench".into()),
        metrics,
    )
    .with_tools_dir(tools_dir)
}

fn env_of(id: &str, files: Vec<(String, SessionFile)>) -> Environment {
    Environment {
        id: id.to_string(),
        files_in: files,
    }
}

fn step(id: &str, env_ref: &str, argv: Vec<String>) -> Step {
    Step {
        id: id.to_string(),
        kind: StepKind::Generic,
        env_ref: env_ref.to_string(),
        argv,
        conf: RunOptions::default(),
        io: IOConfig::default(),
        collect: vec![],
        depends_on: vec![],
        cache: None,
        mounts: vec![],
    }
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
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    }
}

/// `isolate --init` and `isolate --cleanup`, each on its own, so the two sides
/// of a box's life are separate numbers.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn box_init_and_cleanup() {
    let manager = Isolate::manager();
    let mut inits = Vec::with_capacity(ITERATIONS);
    let mut cleanups = Vec::with_capacity(ITERATIONS);
    for _ in 0..ITERATIONS {
        let start = Instant::now();
        manager.create_sandbox(Some("0")).await.expect("init");
        inits.push(start.elapsed());
        let start = Instant::now();
        manager.remove_sandbox("0").await.expect("cleanup");
        cleanups.push(start.elapsed());
    }
    report("box_init", inits);
    report("box_cleanup", cleanups);
}

/// One `isolate --run` of a trivial program in a box that already exists, so
/// the number is the run itself, without init or cleanup.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn run_in_an_existing_box() {
    let manager = Isolate::manager();
    manager.create_sandbox(Some("0")).await.expect("init");
    let argv = vec!["/bin/true".to_string()];
    let samples = time_each(ITERATIONS, |_| {
        let manager = &manager;
        let argv = argv.clone();
        async move {
            manager
                .execute("0", argv, &RunOptions::default())
                .await
                .expect("run");
        }
    })
    .await;
    manager.remove_sandbox("0").await.expect("cleanup");
    report("run_bin_true", samples);
}

/// A whole one-box operation through the handler: box init, the program's run,
/// and the box removal the next operation waits for.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn one_box_operation() {
    let handler = handler(None);
    let samples = time_each(ITERATIONS, |_| {
        let handler = &handler;
        async move {
            let result = handler
                .execute(&operation(
                    vec![env_of("env", vec![])],
                    vec![step("run", "env", vec!["/bin/true".into()])],
                    vec![],
                ))
                .await
                .expect("operation");
            assert!(result.success, "{result:?}");
        }
    })
    .await;
    report("operation_1_box", samples);
}

/// Two boxes, the shape of a program plus a separate checker box.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn two_box_operation() {
    let handler = handler(None);
    let samples = time_each(ITERATIONS, |_| {
        let handler = &handler;
        async move {
            let result = handler
                .execute(&operation(
                    vec![env_of("solution", vec![]), env_of("checker", vec![])],
                    vec![
                        step("solution", "solution", vec!["/bin/true".into()]),
                        step("checker", "checker", vec!["/bin/true".into()]),
                    ],
                    vec![],
                ))
                .await
                .expect("operation");
            assert!(result.success, "{result:?}");
        }
    })
    .await;
    report("operation_2_boxes", samples);
}

/// The communication shape: one box per contestant process, plus the manager's.
/// `processes` of 2, 8 and 64 bracket what a problem may ask for.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn communication_operation_boxes() {
    let handler = handler(None);
    for processes in [2usize, 8, 64] {
        let samples = time_each(HEAVY_ITERATIONS, |_| {
            let handler = &handler;
            async move {
                let ids: Vec<String> = std::iter::once("manager".to_string())
                    .chain((0..processes).map(|i| format!("contestant-{i}")))
                    .collect();
                let result = handler
                    .execute(&operation(
                        ids.iter().map(|id| env_of(id, vec![])).collect(),
                        ids.iter()
                            .map(|id| step(id, id, vec!["/bin/true".into()]))
                            .collect(),
                        vec![],
                    ))
                    .await
                    .expect("operation");
                assert!(result.success, "{result:?}");
            }
        })
        .await;
        report(
            &format!("operation_communication_{processes}_processes"),
            samples,
        );
    }
}

/// A compiled program's own run, the thing a contestant's time limit measures,
/// so a change in platform overhead can be read against it.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn compiled_program_run() {
    let dir = unique_temp_dir("bench-cpp");
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("main.cpp");
    std::fs::write(&source, "int main() { return 0; }\n").unwrap();
    let binary = dir.join("main");
    let compiled = std::process::Command::new(cpp_compiler())
        .args(["-O2", "-std=c++17", "-o"])
        .arg(&binary)
        .arg(&source)
        .status()
        .expect("the compiler runs");
    assert!(compiled.success(), "the bench program must compile");

    let handler = handler(None);
    let samples = time_each(ITERATIONS, |_| {
        let handler = &handler;
        // Copied into the box from the host, which keeps its exec bit.
        let program = SessionFile::Path {
            path: binary.to_string_lossy().into_owned(),
        };
        async move {
            let result = handler
                .execute(&operation(
                    vec![env_of("env", vec![("main".to_string(), program)])],
                    vec![step("run", "env", vec!["main".into()])],
                    vec![],
                ))
                .await
                .expect("operation");
            assert!(result.success, "{result:?}");
        }
    })
    .await;
    let _ = std::fs::remove_dir_all(&dir);
    report("operation_compiled_program", samples);
}

/// A fused test case end to end: the program's output streams to a checker in
/// its own box over a channel, the shape judging actually runs.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the isolate-bench workflow runs it"]
#[serial]
async fn fused_test_case() {
    let handler = handler(None);
    let samples = time_each(ITERATIONS, |_| {
        let handler = &handler;
        async move {
            let mut program = step("exec", "solution", vec!["/bin/echo".into(), "42".into()]);
            program.io.stdout = IOTarget::Pipe {
                name: "sol_out".into(),
            };
            let mut checker = step("check", "checker", vec!["/bin/cat".into()]);
            checker.io.stdin = IOTarget::Pipe {
                name: "sol_out".into(),
            };
            let result = handler
                .execute(&operation(
                    vec![env_of("solution", vec![]), env_of("checker", vec![])],
                    vec![program, checker],
                    vec![Channel {
                        name: "sol_out".into(),
                        buffer_size: Some(8192),
                        ..Channel::default()
                    }],
                ))
                .await
                .expect("operation");
            assert!(result.success, "{result:?}");
        }
    })
    .await;
    report("operation_fused_test_case", samples);
}
