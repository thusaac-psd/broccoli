//! Operation execution on the real isolate sandbox: the shared cases in
//! `sandbox_cases`, plus isolate-only probes. Every test is `#[ignore]`d (run
//! them with `cargo test -p worker --test worker_isolate_sandbox -- --ignored`
//! on a host with isolate installed) and `#[serial]`, since isolate box ids are
//! a host-wide resource.

mod sandbox_cases;

use sandbox_cases::Backend;
use serial_test::serial;
use worker::WorkerAppConfig;
use worker::models::operation::sandbox::SandboxManager;
use worker::models::operation::sandbox::isolate::IsolateSandboxManager;

fn isolate_bin() -> String {
    WorkerAppConfig::load()
        .map(|cfg| cfg.worker.isolate_bin)
        .unwrap_or_else(|_| "isolate".to_string())
}

fn isolate_available() -> bool {
    std::process::Command::new(isolate_bin())
        .arg("--version")
        .output()
        .is_ok()
}

struct Isolate;

impl Backend for Isolate {
    fn manager() -> Box<dyn SandboxManager + Send + Sync> {
        assert!(isolate_available(), "isolate is not available");
        Box::new(IsolateSandboxManager::default())
    }

    // isolate binds mounts at their absolute in-box path.
    fn mounted(path: &str) -> String {
        format!("/{path}")
    }
}

sandbox_suite!(
    Isolate;
    all: [
        #[ignore = "requires Linux isolate sandbox installed and configured"],
        #[serial],
    ];
    cpp: [],
);

// Task 3 - live boot self-test: an over-limit allocation inside isolate MUST
// surface as `cg-oom-killed` so the batch evaluator can render MemoryLimitExceeded
// instead of a silent RuntimeError. Unlike the other tests here (which use
// `IsolateSandboxManager::default()`, cgroups OFF), this constructs the manager
// with cgroups ENABLED, since `cg-oom-killed` is only emitted under `--cg`. If
// isolate + cgroup v2 delegation exist the probe must report `Ok`; otherwise the
// test self-skips. A present-but-non-`Ok` outcome is a real environment defect,
// not a reason to weaken the assertion.
#[tokio::test]
#[ignore = "requires Linux isolate sandbox with cgroup v2 memory delegation"]
#[serial]
async fn boot_probe_reports_oom_when_cgroups_enabled() {
    if !isolate_available() {
        eprintln!("skipping: isolate not available");
        return;
    }
    let mgr = IsolateSandboxManager::new(isolate_bin(), true);
    let outcome = worker::sandbox_preflight::probe_isolate_oom(&mgr).await;
    assert!(
        matches!(outcome, worker::sandbox_preflight::ProbeOutcome::Ok),
        "expected Ok, got {outcome:?}"
    );
}
