//! Static-source regression guards for the server crate.
//!
//! UP#14g - `tokio::task::block_in_place` was removed from every host
//! function in UP#13 (see roadmap entry at
//! `docs/profiling/run-2/roadmap.md`). Re-parking a tokio worker thread
//! from a `spawn_blocking` task is harmful and was found to starve the
//! API runtime under load. This file holds a structural invariant: no
//! `block_in_place(` may reappear in `packages/server/src/host_funcs/`.
//!
//! The original UP#14g acceptance asked for a dynamic counter
//! (`host_fn_block_in_place_total`) check at the end of a 60s stress
//! wave. That formulation is weaker than a static-source guard: the
//! counter only increments if a future host function explicitly opts
//! in via `record_block_in_place_regression`, so a careless
//! re-introduction would silently bypass the check. A static scan is
//! the invariant we actually want.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use futures::future::join_all;
use serde_json::json;

use crate::common::{TestApp, routes};

/// Returns the path to `packages/server/src/host_funcs/` from the
/// crate manifest dir of the `server` package.
fn host_funcs_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at packages/server when this test runs.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("src").join("host_funcs")
}

/// Recursively collect `.rs` files under `dir`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) => panic!("failed to read {}: {err}", dir.display()),
    };
    for entry in entries {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        let file_type = entry.file_type().expect("file_type");
        if file_type.is_dir() {
            collect_rs_files(&path, out);
        } else if file_type.is_file() && path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// True iff the character immediately preceding the match position is
/// part of an identifier (alphanumeric or `_`). In that case
/// `block_in_place(` is the tail of a longer identifier such as
/// `record_block_in_place_regression(` and is not a real toxic call.
fn match_is_inside_identifier(line: &str, match_start: usize) -> bool {
    if match_start == 0 {
        return false;
    }
    // Walk back to the previous char boundary.
    let bytes = line.as_bytes();
    let mut i = match_start;
    while i > 0 && !line.is_char_boundary(i - 1) {
        i -= 1;
    }
    if i == 0 {
        return false;
    }
    let prev_byte = bytes[i - 1];
    prev_byte.is_ascii_alphanumeric() || prev_byte == b'_'
}

/// True iff this line is a `//` comment (single-line). We use this to
/// allow-list the documentation around the regression sentinel.
fn line_is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

#[derive(Debug)]
struct Offender {
    path: PathBuf,
    line_no: usize,
    line: String,
}

/// UP#14g: scan `packages/server/src/host_funcs/` for the literal
/// `block_in_place(` token outside identifier and comment context.
/// Fail loudly with file/line offenders if any are found.
///
/// See also UP#13 (the collapse that removed the toxic pattern) at
/// `docs/profiling/run-2/roadmap.md`.
#[test]
fn host_funcs_must_not_use_tokio_block_in_place() {
    const NEEDLE: &str = "block_in_place(";

    let root = host_funcs_root();
    assert!(
        root.is_dir(),
        "expected host_funcs directory at {}",
        root.display()
    );

    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no .rs files found under {}; the regression guard would silently pass",
        root.display()
    );

    let mut offenders: Vec<Offender> = Vec::new();

    for path in &files {
        let contents =
            fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (idx, line) in contents.lines().enumerate() {
            // Allowlist: comment-only lines (the regression-guard doc
            // comments in host_funcs/mod.rs reference the token).
            if line_is_comment(line) {
                continue;
            }
            // Scan all occurrences on the line (very unlikely to be
            // more than one, but cheap to be exhaustive).
            let mut search_from = 0usize;
            while let Some(rel) = line[search_from..].find(NEEDLE) {
                let abs = search_from + rel;
                if !match_is_inside_identifier(line, abs) {
                    offenders.push(Offender {
                        path: path.clone(),
                        line_no: idx + 1,
                        line: line.to_string(),
                    });
                    break;
                }
                search_from = abs + NEEDLE.len();
            }
        }
    }

    if !offenders.is_empty() {
        let mut msg = String::from(
            "UP#14g regression: tokio::task::block_in_place reintroduced in host_funcs/. \
             This re-parks tokio worker threads and was removed in UP#13. \
             Offenders:\n",
        );
        for o in &offenders {
            msg.push_str(&format!(
                "  {}:{}  {}\n",
                o.path.display(),
                o.line_no,
                o.line.trim_end()
            ));
        }
        msg.push_str("If this is legitimate, document the rationale and remove this test.");
        panic!("{msg}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "60s runtime stress-wave guard; run explicitly for pre-release regression checks"]
async fn host_fn_stress_wave_does_not_spawn_thread_storm() {
    let duration = env_duration_secs("BROCCOLI_STRESS_WAVE_SECS", 60);
    let concurrency = env_usize("BROCCOLI_STRESS_WAVE_CONCURRENCY", 48);
    let allowed_thread_growth = env_usize(
        "BROCCOLI_STRESS_WAVE_MAX_THREAD_GROWTH",
        concurrency.saturating_add(32),
    );

    let app = TestApp::spawn_with_plugins().await;
    let route = routes::plugin_proxy("server-plugin", "kv/runtime-cascade-warmup");
    let warmup = app
        .post_without_token(&route, &json!({ "value": "warmup" }))
        .await;
    assert_eq!(warmup.status, 200, "warmup plugin host-fn call failed");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let baseline_metrics = app.get_without_token("/metrics").await;
    assert_eq!(baseline_metrics.status, 200);
    let baseline_host_fn_calls =
        prometheus_counter_sum(&baseline_metrics.text, "broccoli_host_fn_calls_total");
    let baseline_block_in_place = prometheus_counter_sum(
        &baseline_metrics.text,
        "broccoli_host_fn_block_in_place_total",
    );
    let baseline_plugin_call_failures = prometheus_counter_sum(
        &baseline_metrics.text,
        "broccoli_plugin_call_failures_total",
    );
    let baseline_threads = process_thread_count();
    let max_threads_seen = Arc::new(AtomicUsize::new(baseline_threads.unwrap_or(0)));

    let request_count = Arc::new(AtomicUsize::new(0));
    let failure_count = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let kv_url_prefix = format!(
        "http://{}{}",
        app.addr,
        routes::plugin_proxy("server-plugin", "kv/runtime-cascade")
    );
    let sql_url = format!(
        "http://{}{}",
        app.addr,
        routes::plugin_proxy("server-plugin", "sql/params")
    );
    let client = app.client.clone();
    let sampler_max_threads = max_threads_seen.clone();
    let sampler = tokio::spawn(async move {
        while started.elapsed() < duration {
            if let Some(count) = process_thread_count() {
                update_max(&sampler_max_threads, count);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if let Some(count) = process_thread_count() {
            update_max(&sampler_max_threads, count);
        }
    });

    let tasks = (0..concurrency)
        .map(|worker_idx| {
            let client = client.clone();
            let request_count = request_count.clone();
            let failure_count = failure_count.clone();
            let kv_url_prefix = kv_url_prefix.clone();
            let sql_url = sql_url.clone();
            tokio::spawn(async move {
                let mut sequence = 0usize;
                while started.elapsed() < duration {
                    let response = if sequence.is_multiple_of(5) {
                        client
                            .post(&sql_url)
                            .json(&json!({ "name": format!("stress-{worker_idx}-{sequence}") }))
                            .send()
                            .await
                    } else {
                        let url = format!("{kv_url_prefix}-{worker_idx}-{sequence}");
                        client
                            .post(url)
                            .json(&json!({ "value": format!("{worker_idx}-{sequence}") }))
                            .send()
                            .await
                    };
                    match response {
                        Ok(resp) if resp.status().is_success() => {
                            request_count.fetch_add(1, Ordering::Relaxed);
                        }
                        Ok(resp) => {
                            failure_count.fetch_add(1, Ordering::Relaxed);
                            let _ = resp.text().await;
                        }
                        Err(_) => {
                            failure_count.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    sequence += 1;
                }
            })
        })
        .collect::<Vec<_>>();

    for task in join_all(tasks).await {
        task.expect("stress-wave task panicked");
    }
    sampler.await.expect("thread-count sampler panicked");

    let completed = request_count.load(Ordering::Relaxed);
    let failures = failure_count.load(Ordering::Relaxed);
    assert_eq!(failures, 0, "stress wave had {failures} failed requests");
    assert!(
        completed >= concurrency,
        "stress wave did not exercise enough plugin calls: completed={completed}, concurrency={concurrency}"
    );

    let metrics = app.get_without_token("/metrics").await;
    assert_eq!(metrics.status, 200);
    let host_fn_calls = prometheus_counter_sum(&metrics.text, "broccoli_host_fn_calls_total");
    let host_fn_calls_delta = host_fn_calls - baseline_host_fn_calls;
    assert!(
        host_fn_calls_delta >= completed as f64,
        "host_fn call metric did not observe the wave: before={baseline_host_fn_calls}, after={host_fn_calls}, completed={completed}"
    );
    let block_in_place_total =
        prometheus_counter_sum(&metrics.text, "broccoli_host_fn_block_in_place_total");
    assert_eq!(
        block_in_place_total, baseline_block_in_place,
        "host_fn block_in_place regression counter changed during stress wave"
    );
    let plugin_call_failures =
        prometheus_counter_sum(&metrics.text, "broccoli_plugin_call_failures_total");
    assert_eq!(
        plugin_call_failures, baseline_plugin_call_failures,
        "plugin call failures were recorded during stress wave"
    );

    if let Some(before) = baseline_threads {
        let max_seen = max_threads_seen.load(Ordering::Relaxed);
        let growth = max_seen.saturating_sub(before);
        assert!(
            growth <= allowed_thread_growth,
            "thread-count blow-up during host-fn wave: before={before}, max_seen={max_seen}, growth={growth}, allowed={allowed_thread_growth}, completed={completed}"
        );
    }
}

fn update_max(max: &AtomicUsize, candidate: usize) {
    let mut current = max.load(Ordering::Relaxed);
    while candidate > current {
        match max.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(next_current) => current = next_current,
        }
    }
}

fn env_duration_secs(name: &str, default_secs: u64) -> Duration {
    Duration::from_secs(
        std::env::var(name)
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default_secs),
    )
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn prometheus_counter_sum(metrics_text: &str, metric_name: &str) -> f64 {
    metrics_text
        .lines()
        .filter(|line| line.starts_with(metric_name))
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_, value)| value.parse::<f64>().ok())
        .sum()
}

#[cfg(target_os = "linux")]
fn process_thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task").ok().map(|entries| {
        entries
            .filter(|entry| entry.as_ref().is_ok_and(|entry| entry.path().is_dir()))
            .count()
    })
}

#[cfg(target_os = "macos")]
fn process_thread_count() -> Option<usize> {
    let output = std::process::Command::new("ps")
        .args(["-M", "-p", &std::process::id().to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let count = stdout
        .lines()
        .skip(1)
        .filter(|line| !line.trim().is_empty())
        .count();
    (count > 0).then_some(count)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_thread_count() -> Option<usize> {
    None
}

/// Returns the path to `packages/server/src/handlers/` from the crate
/// manifest dir of the `server` package.
fn handlers_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("src").join("handlers")
}

/// Handlers must reach entities only through the visibility kernel. A direct
/// `crate::entity::` import in a handler module is a bypass: it can read a row
/// the kernel would have denied. Kept as a static guard because the failure
/// this prevents — a new read path that skips the kernel — is invisible at
/// runtime until someone reads a problem they should not have.
#[test]
fn handlers_do_not_import_entities_directly() {
    let root = handlers_root();
    assert!(
        root.is_dir(),
        "expected handlers directory at {}",
        root.display()
    );

    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no .rs files found under {}; the regression guard would silently pass",
        root.display()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        if src.contains("use crate::entity::") && !src.contains("visibility-bypass-audited:") {
            offenders.push(file.display().to_string());
        }
    }
    assert!(
        offenders.is_empty(),
        "handlers importing entities directly: {offenders:#?}"
    );
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn identifier_context_is_recognized() {
        // Hypothetical user-defined wrapper `my_block_in_place(` - the char
        // immediately before `block_in_place(` is `_`, so the guard must
        // treat the match as an identifier tail and skip it.
        let line = "    my_block_in_place(|| {});";
        let pos = line.find("block_in_place(").expect("substring present");
        assert!(match_is_inside_identifier(line, pos));
    }

    #[test]
    fn path_qualified_call_is_not_identifier_context() {
        let line = "    tokio::task::block_in_place(|| { /* ... */ });";
        let pos = line.find("block_in_place(").expect("substring present");
        assert!(!match_is_inside_identifier(line, pos));
    }

    #[test]
    fn bare_call_at_line_start_is_not_identifier_context() {
        let line = "block_in_place(|| {});";
        let pos = line.find("block_in_place(").expect("substring present");
        assert!(!match_is_inside_identifier(line, pos));
    }

    #[test]
    fn comment_lines_are_skipped() {
        assert!(line_is_comment("// block_in_place(|| {})"));
        assert!(line_is_comment("    // mentions block_in_place"));
        assert!(!line_is_comment("    tokio::task::block_in_place(|| {});"));
    }
}
