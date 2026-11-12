//! Static checks over the server crate's source tree, plus an opt-in host
//! function stress wave.
//!
//! No host function calls `tokio::task::block_in_place`: no
//! `block_in_place(` token appears in code under
//! `packages/server/src/host_funcs/`. Host functions run inside
//! `spawn_blocking` tasks, and calling `block_in_place` there re-parks a
//! tokio worker thread, which starves the API runtime under load. The scan
//! is the guard: nothing counts `block_in_place` calls at runtime.
//!
//! Under a sustained wave of plugin host function calls, the process does
//! not grow a thread storm and records no plugin call failures. This test is `#[ignore]`d because it runs for 60 seconds by
//! default; run it explicitly before a release.
//!
//! Handler modules do not reference `crate::entity` directly. Each direct
//! reference (an import, a module alias, an alias use or a fully qualified
//! call) carries a `visibility-bypass-audited:` comment directly above it or
//! above the `use` line that opens its group.
//!
//! Outside the entity module, every `pub use` or `pub(crate) use` that
//! re-exports from `crate::entity` carries the same audit marker, so no
//! module becomes an unreviewed facade over the entity tables.

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
/// `avoid_block_in_place(` and is not a real toxic call.
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

/// True iff this line is a `//` comment (single-line). Comment lines are
/// exempt from the scan, so documentation may mention the token.
fn line_is_comment(line: &str) -> bool {
    line.trim_start().starts_with("//")
}

#[derive(Debug)]
struct Offender {
    path: PathBuf,
    line_no: usize,
    line: String,
}

/// Scans `packages/server/src/host_funcs/` for the literal
/// `block_in_place(` token outside identifier and comment context, and
/// fails with every offending file and line.
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
        "no .rs files found under {}; the scan would pass vacuously",
        root.display()
    );

    let mut offenders: Vec<Offender> = Vec::new();

    for path in &files {
        let contents =
            fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for (idx, line) in contents.lines().enumerate() {
            // Comment-only lines are exempt (the doc comments in
            // host_funcs/mod.rs mention the token).
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
            "tokio::task::block_in_place is called in host_funcs/. \
             Host functions run inside spawn_blocking, where block_in_place \
             re-parks tokio worker threads and starves the runtime. \
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
#[ignore = "60s runtime stress wave; run explicitly before a release"]
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

/// Blanks the interior of every `"..."` string literal on `line` (minimal
/// backslash-escape handling: `\"` does not end the string), replacing each
/// character inside the quotes (including an escape backslash itself) with
/// a space. Column positions are preserved (same length in, same length
/// out). Blanking literals means a string that happens to spell
/// `"entity::..."` (an error message, a `.contains(...)` check) is never
/// mistaken for a real path reference.
fn strip_string_literals(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_string = false;
    let mut escape_next = false;
    for c in line.chars() {
        if in_string {
            if escape_next {
                out.push(' ');
                escape_next = false;
            } else if c == '\\' {
                out.push(' ');
                escape_next = true;
            } else if c == '"' {
                in_string = false;
                out.push('"');
            } else {
                out.push(' ');
            }
        } else if c == '"' {
            in_string = true;
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out
}

/// The portion of `line` that can actually be a real `entity` access: string
/// literals blanked (see `strip_string_literals`), everything from an
/// unquoted `//` onward dropped (a trailing comment is not code), and the
/// whole line blanked if, once trimmed, it is a self-contained single-line
/// `/* ... */` block comment.
///
/// Multi-line block comments are not recognized, because that needs
/// open/close state across lines and this scanner is deliberately
/// line-oriented. The cost of that gap is a possible false positive, never a
/// missed detection.
fn effective_code(line: &str) -> String {
    let trimmed = line.trim_start();
    if trimmed.starts_with("/*") && trimmed.trim_end().ends_with("*/") {
        return String::new();
    }
    let stripped = strip_string_literals(line);
    match stripped.find("//") {
        Some(idx) => stripped[..idx].to_string(),
        None => stripped,
    }
}

/// True iff the character immediately before byte offset `idx` in `code` is
/// not an identifier character (or `idx` is 0). That is, a pattern starting
/// at `idx` is a genuine word-boundary match, not the tail of a longer
/// identifier (e.g. `plugin_entity::` must not match `entity::`).
fn starts_at_word_boundary(code: &str, idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let prev = code[..idx].chars().next_back().unwrap();
    !(prev.is_alphanumeric() || prev == '_')
}

/// True iff `code` (the *effective code* portion of a line, see
/// `effective_code`) references the `entity` module as a path segment
/// (`entity::`) at a genuine word boundary. The boundary check rejects
/// `plugin_entity::` (an unrelated locally-aliased import) while still
/// accepting `crate::entity::`, `super::super::entity::` (any depth of
/// relative chain), `self::entity::`, and a grouped path fragment like
/// `entity::user` sitting on its own line inside a `use crate::{ ... }`
/// block. Also matches a bare module alias site, `entity as `, so
/// `use crate::entity as e;` requires its own audit even though it contains
/// no `entity::` token.
///
/// `known_aliases` are module aliases introduced earlier in the same file
/// via `use crate::entity as X;`. A subsequent `X::` reference is checked
/// the same way `entity::` is, so an audited alias declaration does not
/// cover later uses of the alias: each use needs its own audit.
fn line_has_entity_access(code: &str, known_aliases: &[String]) -> bool {
    let mut patterns: Vec<String> = vec!["entity::".to_string(), "entity as ".to_string()];
    patterns.extend(known_aliases.iter().map(|a| format!("{a}::")));

    for pat in &patterns {
        let mut start = 0;
        while let Some(rel) = code[start..].find(pat.as_str()) {
            let idx = start + rel;
            if starts_at_word_boundary(code, idx) {
                return true;
            }
            start = idx + 1;
        }
    }
    false
}

/// Extracts the alias name from `entity as X` in `code`, if present at a
/// genuine word boundary (see `line_has_entity_access`). Used to grow the
/// set of names `line_has_entity_access` also treats as entity accesses for
/// every following line in the file.
fn extract_entity_alias(code: &str) -> Option<String> {
    const PAT: &str = "entity as ";
    let mut start = 0;
    while let Some(rel) = code[start..].find(PAT) {
        let idx = start + rel;
        if starts_at_word_boundary(code, idx) {
            let after = &code[idx + PAT.len()..];
            let ident: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !ident.is_empty() {
                return Some(ident);
            }
        }
        start = idx + 1;
    }
    None
}

/// A multi-line `use crate::{ ... }` group can put the `entity::` token on a
/// continuation line, separate from the `use` keyword an audit comment sits
/// above. Walk backward to find the `use` line that opens the enclosing
/// group, so the audit comment above *that* line covers the whole group.
/// The walk is bounded by the start of the file, by a prior statement's end
/// (`;`), and by a blank line, not by a fixed step count, so a `use` group
/// of any depth finds its anchor. The `;` and blank-line checks are a
/// sufficient boundary: any prior statement ends with `;`, and a `use`
/// group's continuation lines are never blank (rustfmt does not put blank
/// lines inside one), so the walk never reaches unrelated code.
///
/// A `use ` line that is itself a complete statement (ends with `;` on the
/// same line) is a prior statement, not the opener of a group enclosing
/// `hit_line`. Only a still-open `use` line (no trailing `;` yet, e.g.
/// `use crate::{`) can be that opener. This keeps an unrelated single-line
/// `use foo::bar as e;` from serving as the anchor of a later statement
/// that merely has no blank line or semicolon between it and the hit line
/// (e.g. an alias-use site a few lines into a function body), which would
/// otherwise be a false negative.
fn find_use_group_anchor(lines: &[&str], hit_line: usize) -> usize {
    if lines[hit_line].trim_start().starts_with("use ") {
        return hit_line;
    }
    let mut j = hit_line;
    while j > 0 {
        j -= 1;
        let trimmed = lines[j].trim_start();
        if trimmed.starts_with("use ") {
            if trimmed.ends_with(';') {
                return hit_line;
            }
            return j;
        }
        if trimmed.ends_with(';') || trimmed.is_empty() {
            return hit_line;
        }
    }
    hit_line
}

/// Whether a contiguous block of `//` comment lines directly above
/// `anchor_line` (no blank line or code line breaks the chain) contains the
/// `visibility-bypass-audited:` marker.
fn is_audited_at(lines: &[&str], anchor_line: usize) -> bool {
    let mut j = anchor_line;
    while j > 0 {
        j -= 1;
        let trimmed = lines[j].trim_start();
        if !trimmed.starts_with("//") {
            return false;
        }
        if trimmed.contains("visibility-bypass-audited:") {
            return true;
        }
    }
    false
}

/// Returns the 1-indexed line numbers in `content` where the `entity` module
/// is referenced without an adjacent `visibility-bypass-audited:` comment.
/// Scoped per line (and per `use` group), not per file: one audited import
/// earlier in a file does not exempt any later import in that same file.
///
/// Uses of a module alias are tracked too: after `use crate::entity as e;`,
/// a later `e::user::Entity::find()` is an entity access that needs its own
/// audit (see `known_aliases` on `line_has_entity_access` and
/// `extract_entity_alias`).
///
/// Known gap, which this line-oriented scan cannot close without becoming a
/// real import and symbol resolver:
///   - a facade re-export: another (non-`entity`, non-`handlers`) module
///     doing `pub use crate::entity::submission;`, imported by a handler
///     from that facade instead of from `crate::entity` directly. Proving
///     `crate::some_module::submission` is transitively `crate::entity::submission`
///     requires resolving `pub use` chains across the whole crate, which a
///     per-line text scan cannot do. The gap is narrowed, not closed, by
///     `entity_reexports_outside_the_entity_module_are_audited` below: it
///     does not flag the *consuming* handler, but it requires the facade
///     itself, wherever it is created, to carry its own audit marker. So a
///     facade cannot appear unreviewed, but a handler that imports an
///     already-audited facade is not flagged. Precisely, this check
///     guarantees "no handler imports `crate::entity::` directly, and no
///     facade re-exports it silently", not "no handler can ever reach an
///     entity table outside the kernel".
fn find_unaudited_entity_accesses(content: &str) -> Vec<usize> {
    let lines: Vec<&str> = content.lines().collect();
    let effective: Vec<String> = lines.iter().map(|line| effective_code(line)).collect();

    let mut offenders = Vec::new();
    let mut known_aliases: Vec<String> = Vec::new();
    for (i, code) in effective.iter().enumerate() {
        if line_has_entity_access(code, &known_aliases) {
            let anchor = find_use_group_anchor(&lines, i);
            if !is_audited_at(&lines, anchor) {
                offenders.push(i + 1);
            }
        }
        // An alias becomes known starting the line after its own
        // declaration. Collected unconditionally (whether or not the
        // declaration line itself was flagged/audited) since a later
        // unaudited USE of the alias is its own separate offense either way.
        if let Some(alias) = extract_entity_alias(code) {
            known_aliases.push(alias);
        }
    }
    offenders
}

/// Returns the path to `packages/server/src/` from the crate manifest dir of
/// the `server` package.
fn server_src_root() -> PathBuf {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir).join("src")
}

/// Re-export variant of `find_unaudited_entity_accesses`: flags `pub use` /
/// `pub(crate) use` lines that reference the `entity` module without an
/// adjacent `visibility-bypass-audited:` marker. See
/// `entity_reexports_outside_the_entity_module_are_audited` for what this
/// does and does not cover.
fn find_unaudited_entity_reexports(content: &str) -> Vec<usize> {
    let lines: Vec<&str> = content.lines().collect();
    let mut offenders = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let code = effective_code(line);
        let trimmed = code.trim_start();
        if !(trimmed.starts_with("pub use") || trimmed.starts_with("pub(crate) use")) {
            continue;
        }
        if !line_has_entity_access(&code, &[]) {
            continue;
        }
        let anchor = find_use_group_anchor(&lines, i);
        if !is_audited_at(&lines, anchor) {
            offenders.push(i + 1);
        }
    }
    offenders
}

/// Handlers must reach entities only through the visibility kernel. A direct
/// `crate::entity::` import in a handler module is a bypass: it can read a row
/// the kernel would have denied. The check is static because the failure it
/// prevents (a new read path that skips the kernel) is invisible at runtime
/// until someone reads a problem they should not have.
///
/// This is a per-line and per-`use`-group check, not a per-file one: a file
/// that has one audited entity import gets no blanket exemption for its other
/// entity imports.
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
        "no .rs files found under {}; the scan would pass vacuously",
        root.display()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let src =
            fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let lines = find_unaudited_entity_accesses(&src);
        if !lines.is_empty() {
            offenders.push(format!("{}: lines {lines:?}", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "handlers importing entities directly without an adjacent audit: {offenders:#?}"
    );
}

/// Every re-export of `crate::entity` outside the entity module carries a
/// `visibility-bypass-audited:` marker. This narrows the facade gap
/// described on `find_unaudited_entity_accesses`:
/// `handlers_do_not_import_entities_directly` only scans `src/handlers/`, so
/// it cannot see a handler that imports an entity type by name from some
/// other module which itself re-exports it from `crate::entity`. This check
/// does not flag the consuming handler either (that needs real import
/// resolution), but it makes creating a facade a reviewed, explained step.
#[test]
fn entity_reexports_outside_the_entity_module_are_audited() {
    let root = server_src_root();
    assert!(
        root.is_dir(),
        "expected src directory at {}",
        root.display()
    );

    let mut files = Vec::new();
    collect_rs_files(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no .rs files found under {}; the scan would pass vacuously",
        root.display()
    );

    let mut offenders = Vec::new();
    for file in &files {
        // The entity module re-exporting its own items is not a facade.
        if file.components().any(|c| c.as_os_str() == "entity") {
            continue;
        }
        let src =
            fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let lines = find_unaudited_entity_reexports(&src);
        if !lines.is_empty() {
            offenders.push(format!("{}: lines {lines:?}", file.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "entity re-exports outside crate::entity without an adjacent audit: {offenders:#?}"
    );
}

#[cfg(test)]
mod entity_import_guard_tests {
    use super::{find_unaudited_entity_accesses, find_unaudited_entity_reexports};

    /// Audits are scoped per import, not per file: an audited import earlier
    /// in a file does not exempt an unrelated, unaudited import later in the
    /// same file. A whole-file `contains(marker)` check would pass this shape.
    #[test]
    fn earlier_audited_import_does_not_exempt_a_later_unaudited_one() {
        let src = "\
// visibility-bypass-audited: existing justified import
use crate::entity::{additional_file, problem};

use crate::entity::submission; // brand new, zero justification
";
        assert_eq!(
            find_unaudited_entity_accesses(src),
            vec![4],
            "the unaudited `submission` import on line 4 must be flagged even \
             though line 1's comment audits line 2"
        );
    }

    #[test]
    fn audited_flat_import_is_not_flagged() {
        let src = "\
// visibility-bypass-audited: some reason
use crate::entity::user;
";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    #[test]
    fn unaudited_flat_import_is_flagged() {
        let src = "use crate::entity::user;\n";
        assert_eq!(find_unaudited_entity_accesses(src), vec![1]);
    }

    /// A grouped path is detected even though it has no literal
    /// `use crate::entity::` substring.
    #[test]
    fn grouped_use_path_is_detected() {
        let src = "use crate::{entity::user, other_mod};\n";
        assert_eq!(find_unaudited_entity_accesses(src), vec![1]);
    }

    /// A relative chain has no `crate::entity::` substring and is detected at
    /// any depth of `super::`.
    #[test]
    fn relative_super_chain_is_detected_at_any_depth() {
        assert_eq!(
            find_unaudited_entity_accesses("use super::entity::user;\n"),
            vec![1]
        );
        assert_eq!(
            find_unaudited_entity_accesses("use super::super::entity::user;\n"),
            vec![1]
        );
    }

    /// A fully qualified inline call with no `use` at all is detected.
    #[test]
    fn fully_qualified_inline_call_is_detected() {
        let src = "\
fn f() {
    let x = crate::entity::submission::Entity::find_by_id(1);
}
";
        assert_eq!(find_unaudited_entity_accesses(src), vec![2]);
    }

    /// A multi-line grouped `use` puts `entity::` on a continuation line; the
    /// audit comment lives above the `use` line that opens the group, not
    /// directly above the `entity::user,` line itself.
    #[test]
    fn multiline_grouped_use_is_covered_by_comment_above_the_use_line() {
        let src = "\
// visibility-bypass-audited: reason
use crate::{
    entity::user,
    other_mod,
};
";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    /// A locally-aliased, unrelated import (`plugin_entity::`) must not
    /// false-positive: `entity` here is a suffix of a longer identifier, not
    /// the `entity` module path segment.
    #[test]
    fn aliased_unrelated_identifier_is_not_a_false_positive() {
        let src = "let x = plugin_entity::ActiveModel { ..Default::default() };\n";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    /// `use crate::entity::user as u;` (the alias is on the imported item,
    /// not the module) still contains a literal `entity::` token and is
    /// still caught.
    #[test]
    fn item_level_alias_does_not_evade() {
        let src = "use crate::entity::user as u;\n";
        assert_eq!(find_unaudited_entity_accesses(src), vec![1]);
    }

    /// `use crate::entity as e;` has no `entity::` substring but must still
    /// require an audit at the aliasing site itself.
    #[test]
    fn module_level_alias_site_is_detected() {
        let src = "use crate::entity as e;\n";
        assert_eq!(find_unaudited_entity_accesses(src), vec![1]);
    }

    /// An audited alias declaration does not exempt a later, unrelated use
    /// of the alias. This is the same rule as
    /// `earlier_audited_import_does_not_exempt_a_later_unaudited_one`,
    /// applied to an alias instead of a second `entity::` import.
    #[test]
    fn alias_used_later_in_file_is_flagged_even_if_declaration_itself_is_audited() {
        let src = "\
// visibility-bypass-audited: legitimate alias for brevity
use crate::entity as e;

fn read_it() {
    let _ = e::user::Entity::find();
}
";
        assert_eq!(
            find_unaudited_entity_accesses(src),
            vec![5],
            "a later unaudited use of an aliased entity module must be \
             flagged even though the alias declaration itself carries its \
             own audit"
        );
    }

    /// An audit on the alias use site (not just the declaration) exempts
    /// that use, the same as for any other entity access.
    #[test]
    fn audited_alias_use_is_not_flagged() {
        let src = "\
// visibility-bypass-audited: alias declaration
use crate::entity as e;

fn read_it() {
    // visibility-bypass-audited: this specific read is justified
    let _ = e::user::Entity::find();
}
";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    /// A string literal that happens to spell `entity::...` (e.g. an error
    /// message or a `.contains(...)` check) is not a path reference.
    #[test]
    fn entity_substring_inside_a_string_literal_is_not_a_false_positive() {
        let src = "let msg = \"reachable via entity::user in the audit log\";\n";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    /// A single-line `/* ... */` block comment is not code, the same as a
    /// `//` comment.
    #[test]
    fn single_line_block_comment_is_not_a_false_positive() {
        let src = "/* legacy note: entity::user used to live here */\n";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    /// A trailing `//` comment mentioning `entity::` on a line whose actual
    /// code has nothing to do with it is not flagged.
    #[test]
    fn trailing_comment_only_reference_is_not_a_false_positive() {
        let src = "let x = 1; // see entity::user for context\n";
        assert!(find_unaudited_entity_accesses(src).is_empty());
    }

    /// A deep `use crate::{ ... }` group (here, 9 lines between the opening
    /// `use` and the `entity::` continuation line) resolves back to its
    /// audited opening line: the anchor walk has no fixed depth limit.
    #[test]
    fn use_group_deeper_than_eight_lines_still_finds_its_anchor_comment() {
        let src = "\
// visibility-bypass-audited: wide facade import, see PR #123
use crate::{
    a,
    b,
    c,
    d,
    e,
    f,
    g,
    h,
    entity::user,
    other_mod,
};
";
        assert!(
            find_unaudited_entity_accesses(src).is_empty(),
            "a 9-line-deep use group must still resolve back to its audited \
             opening `use` line"
        );
    }

    #[test]
    fn unaudited_public_reexport_is_flagged() {
        let src = "pub use crate::entity::submission;\n";
        assert_eq!(find_unaudited_entity_reexports(src), vec![1]);
    }

    #[test]
    fn pub_crate_reexport_is_also_flagged() {
        let src = "pub(crate) use crate::entity::submission;\n";
        assert_eq!(find_unaudited_entity_reexports(src), vec![1]);
    }

    #[test]
    fn audited_public_reexport_is_not_flagged() {
        let src = "\
// visibility-bypass-audited: intentional read-model facade
pub use crate::entity::submission;
";
        assert!(find_unaudited_entity_reexports(src).is_empty());
    }

    /// Only `pub`/`pub(crate)` re-exports create a facade another module
    /// can import from. A private `use` is already scoped to its own module
    /// by ordinary Rust visibility and is out of scope for this check (it is
    /// still covered by
    /// `handlers_do_not_import_entities_directly` if the private `use` is
    /// itself inside `handlers/`).
    #[test]
    fn private_use_of_entity_is_not_a_reexport_concern() {
        let src = "use crate::entity::submission;\n";
        assert!(find_unaudited_entity_reexports(src).is_empty());
    }
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn identifier_context_is_recognized() {
        // Hypothetical user-defined wrapper `my_block_in_place(`: the char
        // immediately before `block_in_place(` is `_`, so the scan treats
        // the match as an identifier tail and skips it.
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
