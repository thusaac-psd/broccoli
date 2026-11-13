//! Operation execution on the real isolate sandbox: the shared cases in
//! `sandbox_cases`, plus isolate-only probes. Every test is `#[ignore]`d,
//! because it needs isolate installed with cgroup v2 delegation; the `sandbox`
//! CI job runs them with
//! `cargo test -p worker --test worker_isolate_sandbox -- --ignored`. They are
//! `#[serial]` because isolate box ids are a host-wide resource.

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
        #[ignore = "needs isolate; the sandbox CI job runs it (see the module docs)"],
        #[serial],
    ];
);

// The boot self-test: an over-limit allocation inside isolate surfaces as
// `cg-oom-killed`, so the batch evaluator renders MemoryLimitExceeded instead of
// a silent RuntimeError. Unlike the other tests here (which use
// `IsolateSandboxManager::default()`, cgroups off), this enables cgroups, since
// isolate reports `cg-oom-killed` only under `--cg`.
#[tokio::test]
#[ignore = "needs isolate with cgroup v2 delegation; the sandbox CI job runs it"]
#[serial]
async fn boot_probe_reports_oom_when_cgroups_enabled() {
    assert!(isolate_available(), "isolate is not available");
    let mgr = IsolateSandboxManager::new(isolate_bin(), true);
    let outcome = worker::sandbox_preflight::probe_isolate_oom(&mgr).await;
    assert!(
        matches!(outcome, worker::sandbox_preflight::ProbeOutcome::Ok),
        "expected Ok, got {outcome:?}"
    );
}
