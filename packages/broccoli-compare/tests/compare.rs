use std::io::{Read, Write};
use std::process::{Command, Stdio};

/// First-N-bytes preview cap, mirroring `INLINE_OUTPUT_PREVIEW_BYTES` in
/// `packages/worker/src/models/operation/sandbox/isolate.rs`.
const PREVIEW_BYTES: usize = 64 * 1024;

fn run(mode: &str, stdin: &[u8], answer: &str) -> i32 {
    run_args(mode, stdin, answer, &[])
}

fn run_args(mode: &str, stdin: &[u8], answer: &str, extra: &[&str]) -> i32 {
    let dir = tempfile::tempdir().unwrap();
    let ans = dir.path().join("ans");
    std::fs::write(&ans, answer).unwrap();
    let mut args = vec!["--mode", mode, "--answer", ans.to_str().unwrap()];
    args.extend_from_slice(extra);
    let mut child = Command::new(env!("CARGO_BIN_EXE_broccoli-compare"))
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    child.wait().unwrap().code().unwrap()
}

#[test]
fn exact_match_is_ac() {
    assert_eq!(run("exact", b"hello\n", "hello\n"), 0);
}

#[test]
fn exact_mismatch_is_wa() {
    assert_eq!(run("exact", b"hello\n", "world\n"), 1);
}

// Edge cases capturing compare_exact's real semantics: exact mode is a strict
// byte-for-byte comparison with NO trailing-whitespace/newline normalization.
// A trailing newline present in one side but not the other is a WrongAnswer.

#[test]
fn exact_trailing_newline_only_in_output_is_wa() {
    // output has the trailing "\n", answer does not -> bytes differ -> WA
    assert_eq!(run("exact", b"42\n", "42"), 1);
}

#[test]
fn exact_trailing_newline_only_in_answer_is_wa() {
    // answer has the trailing "\n", output does not -> bytes differ -> WA
    assert_eq!(run("exact", b"42", "42\n"), 1);
}

#[test]
fn exact_trailing_space_is_significant_wa() {
    // a trailing space is a real byte; exact mode does not strip it -> WA
    assert_eq!(run("exact", b"42 \n", "42\n"), 1);
}

#[test]
fn exact_both_empty_is_ac() {
    assert_eq!(run("exact", b"", ""), 0);
}

// --- tokens (whitespace-insensitive token-sequence equality) ---------------

#[test]
fn tokens_different_whitespace_is_ac() {
    assert_eq!(run("tokens", b"  1   2  3  \n", "1 2 3"), 0);
}

#[test]
fn tokens_trailing_newline_is_ac() {
    // Trailing newline is whitespace -> same tokens -> AC (unlike exact mode).
    assert_eq!(run("tokens", b"42\n", "42"), 0);
}

#[test]
fn tokens_value_mismatch_is_wa() {
    assert_eq!(run("tokens", b"1 X 3", "1 2 3"), 1);
}

#[test]
fn tokens_count_mismatch_is_wa() {
    assert_eq!(run("tokens", b"1 2", "1 2 3"), 1);
}

// --- tokens-case-insensitive ----------------------------------------------

#[test]
fn tokens_ci_case_folded_is_ac() {
    assert_eq!(run("tokens-case-insensitive", b"YES\n", "yes"), 0);
}

#[test]
fn tokens_ci_different_tokens_is_wa() {
    assert_eq!(run("tokens-case-insensitive", b"abc\n", "xyz"), 1);
}

// --- lines (per-line, trailing whitespace/newline tolerant) ----------------

#[test]
fn lines_trailing_whitespace_is_ac() {
    assert_eq!(run("lines", b"hello  \nworld  \n", "hello\nworld\n"), 0);
}

#[test]
fn lines_trailing_empty_lines_is_ac() {
    assert_eq!(run("lines", b"hello\n\n\n", "hello\n"), 0);
}

#[test]
fn lines_crlf_normalized_is_ac() {
    assert_eq!(run("lines", b"a\r\nb\r\n", "a\nb\n"), 0);
}

#[test]
fn lines_internal_whitespace_is_wa() {
    assert_eq!(run("lines", b"hello  world\n", "hello world\n"), 1);
}

// --- tokens-float (numeric tokens within --epsilon, else exact) ------------

#[test]
fn float_within_default_tolerance_is_ac() {
    assert_eq!(run("tokens-float", b"1.0000000001", "1.0000000000"), 0);
}

#[test]
fn float_outside_default_tolerance_is_wa() {
    assert_eq!(run("tokens-float", b"1.0", "2.0"), 1);
}

#[test]
fn float_within_epsilon_is_ac() {
    // --epsilon maps to the absolute tolerance: a 0.5 diff is AC at eps 1.0.
    assert_eq!(
        run_args("tokens-float", b"1.5", "1.0", &["--epsilon", "1.0"]),
        0
    );
}

#[test]
fn float_outside_epsilon_is_wa() {
    assert_eq!(
        run_args("tokens-float", b"3.0", "1.0", &["--epsilon", "1.0"]),
        1
    );
}

#[test]
fn float_mixed_int_float_string_is_ac() {
    assert_eq!(run("tokens-float", b"YES 3.14", "YES 3.14"), 0);
}

// Float mode tunes BOTH abs_tol AND rel_tol per problem: `--epsilon` sets the
// absolute tolerance, `--rel-epsilon` the relative one. The fused resolver maps
// a problem's config.abs_tol -> --epsilon and config.rel_tol -> --rel-epsilon.

#[test]
fn float_default_rel_tol_rejects_large_relative_diff_wa() {
    // diff 0.5 at magnitude ~1000; default rel_tol 1e-6 -> tol ~1.0e-3 -> WA.
    assert_eq!(run("tokens-float", b"1000.5", "1000.0"), 1);
}

#[test]
fn float_custom_rel_epsilon_accepts_large_relative_diff_ac() {
    // Same inputs, but rel_tol 1e-3 -> tol ~1.0005 -> AC. Pins that the CLI
    // reaches the rel_tol knob.
    assert_eq!(
        run_args(
            "tokens-float",
            b"1000.5",
            "1000.0",
            &["--rel-epsilon", "1e-3"]
        ),
        0
    );
}

#[test]
fn float_rel_epsilon_and_epsilon_combine_via_max() {
    // tolerance = max(abs_tol, rel_tol*max(|a|,|b|)); with tiny magnitudes the
    // absolute term dominates. abs_tol 0.6 accepts a 0.5 diff at magnitude 1.
    assert_eq!(
        run_args(
            "tokens-float",
            b"1.5",
            "1.0",
            &["--epsilon", "0.6", "--rel-epsilon", "1e-9"]
        ),
        0
    );
}

// ===========================================================================
// The first 64 KiB of stdin is tee'd to stdout (the display preview).
// ===========================================================================

/// Run over a binary stdin/answer; returns `(exit_code, stdout_preview)`. The
/// preview is read from the child's **stdout** concurrently with writing stdin
/// (a writer thread) so a large input never deadlocks on a full stdout pipe; the
/// writer also asserts the write completes (no BrokenPipe - stdin is drained).
fn run_with_preview(mode: &str, stdin: &[u8], answer: &[u8]) -> (i32, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let ans = dir.path().join("ans");
    std::fs::write(&ans, answer).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_broccoli-compare"))
        .args(["--mode", mode, "--answer", ans.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut child_stdin = child.stdin.take().unwrap();
    let data = stdin.to_vec();
    let writer = std::thread::spawn(move || {
        child_stdin
            .write_all(&data)
            .expect("write_all to child stdin should succeed (no BrokenPipe)");
        // drop child_stdin -> EOF
    });

    let mut preview = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut preview)
        .expect("reading child stdout should succeed");
    writer.join().unwrap();
    let code = child.wait().unwrap().code().unwrap();
    (code, preview)
}

#[test]
fn preview_small_input_is_whole_input() {
    // Input shorter than the cap -> preview is the whole input.
    let stdin = b"hello world\n";
    let (code, preview) = run_with_preview("exact", stdin, b"hello world\n");
    assert_eq!(code, 0);
    assert_eq!(preview, stdin);
}

#[test]
fn preview_exactly_64k_is_whole_input() {
    // Input exactly the cap -> preview is the whole input (no truncation).
    let stdin = vec![b'a'; PREVIEW_BYTES];
    let (_code, preview) = run_with_preview("exact", &stdin, &stdin);
    assert_eq!(preview.len(), PREVIEW_BYTES);
    assert_eq!(preview, stdin);
}

#[test]
fn preview_large_input_is_capped_to_64k() {
    // 1 MiB of input; preview must be exactly the first 64 KiB.
    let stdin = vec![b'b'; 1024 * 1024];
    let (_code, preview) = run_with_preview("exact", &stdin, &stdin);
    assert_eq!(preview.len(), PREVIEW_BYTES);
    assert_eq!(preview, &stdin[..PREVIEW_BYTES]);
}

#[test]
fn preview_is_full_64k_even_when_verdict_is_wa_early() {
    // Mismatch on the very first byte (WA decided immediately), but the preview
    // must still be the full first 64 KiB -- NOT truncated at the mismatch byte.
    let stdin = vec![b'x'; 256 * 1024];
    let answer = b"y"; // differs at byte 0 -> WA decided at once
    let (code, preview) = run_with_preview("exact", &stdin, answer);
    assert_eq!(code, 1, "should be WA");
    assert_eq!(preview.len(), PREVIEW_BYTES);
    assert_eq!(preview, vec![b'x'; PREVIEW_BYTES]);
}

// ===========================================================================
// Stdin is drained to EOF even after an early-decided verdict, so a
// still-running producer never receives SIGPIPE / BrokenPipe.
// ===========================================================================

#[test]
fn drains_full_stdin_on_early_mismatch_without_broken_pipe() {
    // Verdict decided on the first byte, but the comparator must still consume
    // all of stdin. We write 8 MiB with write_all; if the child closed the read
    // end early, write_all would fail with BrokenPipe.
    let dir = tempfile::tempdir().unwrap();
    let ans = dir.path().join("ans");
    // Answer differs from the very first byte -> WA decided immediately.
    std::fs::write(&ans, b"Z").unwrap();

    let big = vec![b'q'; 8 * 1024 * 1024]; // 8 MiB

    let mut child = Command::new(env!("CARGO_BIN_EXE_broccoli-compare"))
        .args(["--mode", "exact", "--answer", ans.to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let write_result = stdin.write_all(&big);
    assert!(
        write_result.is_ok(),
        "write_all of 8 MiB should complete (no BrokenPipe): {write_result:?}"
    );
    drop(stdin); // signal EOF

    let code = child.wait().unwrap().code().unwrap();
    assert_eq!(code, 1, "verdict should be WA");
}

// ===========================================================================
// Usage / IO error contract: every malformed invocation exits 64 (mapped to a
// checker SystemError downstream), distinct from the 0/1 verdict codes. A
// regression that let an unrecognized mode fall through to a verdict, or that
// changed the error code, would silently corrupt verdicts.
// ===========================================================================

/// Run with a fully explicit argv (no implicit --mode/--answer), feeding empty
/// stdin; returns the exit code. The binary may exit before reading stdin, so a
/// BrokenPipe on the write is expected and ignored.
fn run_raw(args: &[&str]) -> i32 {
    let mut child = Command::new(env!("CARGO_BIN_EXE_broccoli-compare"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let _ = child.stdin.take().unwrap().write_all(b"");
    child.wait().unwrap().code().unwrap()
}

/// Path to a freshly-written answer file inside a tempdir kept alive by the
/// returned guard.
fn answer_file(contents: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let ans = dir.path().join("ans");
    std::fs::write(&ans, contents).unwrap();
    let path = ans.to_str().unwrap().to_string();
    (dir, path)
}

#[test]
fn error_unknown_mode_exits_64() {
    let (_d, ans) = answer_file("x");
    assert_eq!(run_raw(&["--mode", "bogus", "--answer", &ans]), 64);
}

#[test]
fn error_missing_mode_exits_64() {
    let (_d, ans) = answer_file("x");
    assert_eq!(run_raw(&["--answer", &ans]), 64);
}

#[test]
fn error_missing_answer_exits_64() {
    assert_eq!(run_raw(&["--mode", "exact"]), 64);
}

#[test]
fn error_invalid_epsilon_exits_64() {
    let (_d, ans) = answer_file("x");
    assert_eq!(
        run_raw(&[
            "--mode",
            "tokens-float",
            "--answer",
            &ans,
            "--epsilon",
            "notanumber"
        ]),
        64
    );
}

#[test]
fn error_invalid_rel_epsilon_exits_64() {
    let (_d, ans) = answer_file("x");
    assert_eq!(
        run_raw(&[
            "--mode",
            "tokens-float",
            "--answer",
            &ans,
            "--rel-epsilon",
            "notanumber"
        ]),
        64
    );
}

#[test]
fn error_unknown_flag_exits_64() {
    let (_d, ans) = answer_file("x");
    assert_eq!(
        run_raw(&["--mode", "exact", "--answer", &ans, "--bogus"]),
        64
    );
}

#[test]
fn error_flag_missing_value_exits_64() {
    // --answer with no following value.
    assert_eq!(run_raw(&["--mode", "exact", "--answer"]), 64);
}

#[test]
fn error_nonexistent_answer_file_exits_64() {
    assert_eq!(
        run_raw(&["--mode", "exact", "--answer", "/nonexistent/broccoli/xyzzy"]),
        64
    );
}

#[test]
fn drains_full_stdin_with_preview_on_early_mismatch() {
    // Verdict decided on byte 0, but the comparator must still drain all of stdin
    // (run_with_preview's writer asserts no BrokenPipe) AND the stdout preview
    // must be the first 64 KiB.
    let big = vec![b'w'; 4 * 1024 * 1024];
    let (code, preview) = run_with_preview("exact", &big, b"Z");
    assert_eq!(code, 1);
    assert_eq!(preview.len(), PREVIEW_BYTES);
    assert_eq!(preview, vec![b'w'; PREVIEW_BYTES]);
}
