use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn require_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|_| panic!("{key} must be set; see packages/stress-test/README.md"))
}

#[test]
#[ignore = "needs a running stack: STRESS_TEST_E2E_URL=<url> cargo test -p stress-test --test e2e_real -- --ignored"]
fn stress_test_passes_against_real_server() {
    let url = require_env("STRESS_TEST_E2E_URL");
    let username =
        std::env::var("STRESS_TEST_E2E_USERNAME").unwrap_or_else(|_| "admin".to_string());
    let password = require_env("STRESS_TEST_E2E_PASSWORD");

    let bin = env!("CARGO_BIN_EXE_broccoli-stress-test");
    let output = Command::new(bin)
        .args([
            "--url",
            &url,
            "--admin-username",
            &username,
            "--admin-password",
            &password,
            "--skip-load",
            "--json",
        ])
        .output()
        .expect("failed to spawn broccoli-stress-test");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stress-test exited {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code()
    );

    let payload: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("non-JSON stdout: {e}\nstdout:\n{stdout}"));
    assert_eq!(payload["result"], "pass", "payload:\n{stdout}");
    assert_eq!(payload["exit_code"], 0, "payload:\n{stdout}");
}

#[test]
#[ignore = "needs a running stack with /metrics and a no-semaphore baseline: STRESS_TEST_E2E_URL=<url> cargo test -p stress-test --test e2e_real -- --ignored"]
fn burst_1000_submissions_keeps_plugin_admission_queued_below_baseline() {
    let url = require_env("STRESS_TEST_E2E_URL");
    let username =
        std::env::var("STRESS_TEST_E2E_USERNAME").unwrap_or_else(|_| "admin".to_string());
    let password = require_env("STRESS_TEST_E2E_PASSWORD");
    let metrics_url = require_env("STRESS_TEST_E2E_METRICS_URL");
    let baseline = require_env("STRESS_TEST_E2E_BASELINE_PLUGIN_ADMISSION_QUEUED_DELTA");
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before UNIX_EPOCH")
        .as_nanos();
    let transcript_path = std::env::temp_dir().join(format!("broccoli-burst-1000-{suffix}.json"));

    let bin = env!("CARGO_BIN_EXE_broccoli-stress-test");
    let output = Command::new(bin)
        .args([
            "fault",
            "burst",
            "--server-url",
            &url,
            "--admin-username",
            &username,
            "--admin-password",
            &password,
            "--submission-count",
            "1000",
            "--concurrency",
            "64",
            "--terminal-deadline-secs",
            "300",
            "--fanout-test-cases-per-problem",
            "20",
            "--metrics-url",
            &metrics_url,
            "--baseline-plugin-admission-queued-delta",
            &baseline,
            "--max-plugin-admission-queued-baseline-ratio",
            "0.2",
            "--require-fanout-saturation",
            "--out",
            transcript_path.to_str().expect("temp path should be UTF-8"),
        ])
        .output()
        .expect("failed to spawn broccoli-stress-test burst scenario");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let transcript = std::fs::read_to_string(&transcript_path).unwrap_or_default();
    assert!(
        output.status.success(),
        "burst scenario exited {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}\ntranscript:\n{transcript}",
        output.status.code()
    );
    assert!(
        transcript.contains("plugin_admission_queued_total delta"),
        "burst transcript should include the contention assertion:\n{transcript}"
    );
    assert!(
        transcript.contains("metrics delta captured"),
        "burst transcript should include before/after metrics evidence:\n{transcript}"
    );
}
