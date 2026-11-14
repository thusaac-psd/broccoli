//! Checker fusion: resolve a checker into a `CheckerStage` the batch-evaluator
//! splices into the run operation, and interpret the small check result.
//!
//! The pure stage builders here are host-testable (no host calls); the wasm
//! `#[plugin_fn]` entry points (bottom) wire them to the host (config load,
//! language resolve for testlib).

use broccoli_server_sdk::types::*;

#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::Host;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};

/// Checker environment id within the fused op (both built-in and testlib).
pub(crate) const CHECKER_ENV_ID: &str = "checker";
/// The check step's id; also the stage's `result_step_id`. Single-sourced from
/// the SDK so the resolver's emitted id and the interpreter's lookup id (in
/// batch-evaluator) can never diverge.
use broccoli_server_sdk::evaluator::DEFAULT_CHECK_STEP_ID as CHECK_STEP_ID;

// Hard caps for the built-in streaming comparator. The comparator reads the
// solution's stdout over a FIFO; in lines/tokens mode a contestant can emit a
// giant newline-free stream, so without limits the check step has unbounded CPU,
// wall time, and memory (worker DoS). These are generous for any real comparison
// but bound a pathological stream. See resolve_builtin below.
//
// CPU (`TIME_LIMIT`) bounds actual comparison work. WALL must be large because in
// Stream mode the checker runs CONCURRENTLY with the solution and only finishes
// when the solution's stdout stream CLOSES (which the solution's OWN wall limit
// guarantees). A tight wall here would kill the checker - and thus WA a correct
// answer - for any solution whose run legitimately exceeds it; so the wall is a
// coarse backstop well above any realistic per-test time limit, not a bound on
// the checker's own work (that is CPU).
const BUILTIN_CHECK_TIME_LIMIT_S: f64 = 30.0;
const BUILTIN_CHECK_WALL_LIMIT_S: f64 = 600.0;
const BUILTIN_CHECK_MEMORY_LIMIT_KB: u32 = 256 * 1024;
/// The solution's run step id in the fused op. batch-evaluator names it "exec";
/// a File-mode checker mounts THAT step's captured output read-only. The
/// evaluator adds the `depends_on` edge on `exec` when it splices the stage.
pub(crate) const SOLUTION_EXEC_STEP_ID: &str = "exec";

/// PlatformTool name + in-sandbox path for the native comparator binary.
const BROCCOLI_COMPARE_TOOL: &str = "broccoli-compare";
const BROCCOLI_COMPARE_PATH: &str = "/tools/broccoli-compare";

const ANSWER_FILE: &str = "answer.txt";
/// broccoli-compare writes the display preview (first 64 KiB of the solution
/// output) to **stdout** and its short verdict message to **stderr**. Both are
/// File-redirected so the worker reads them back into the check step's sandbox
/// result inline - no collected blob, nothing uploaded to the coordinator.
const PREVIEW_FILE: &str = "output_preview.txt";
const MESSAGE_FILE: &str = "check_msg.txt";

/// `tokens-float` tolerance defaults, identical to broccoli-compare's own
/// `tokens-float` defaults (`abs_tol = 1e-9`, `rel_tol = 1e-6`) so fused
/// verdicts stay byte-identical to an unfused broccoli-compare run.
const DEFAULT_ABS_TOL: f64 = 1e-9;
const DEFAULT_REL_TOL: f64 = 1e-6;

/// Map a built-in checker format to its broccoli-compare `--mode` value. The
/// native comparator's mode strings are identical to the checker format strings
/// for every comparison format, so this is a 1:1 pass-through that also gates
/// which formats are native-comparator builtins (testlib / none return `None`).
pub fn builtin_compare_mode(format: &str) -> Option<&'static str> {
    match format {
        "exact" => Some("exact"),
        "lines" => Some("lines"),
        "tokens" => Some("tokens"),
        "tokens-case-insensitive" => Some("tokens-case-insensitive"),
        "tokens-float" => Some("tokens-float"),
        _ => None,
    }
}

/// Resolve a built-in comparison format into a single-step Stream checker stage
/// driven by the native broccoli-compare binary. The solution output arrives on
/// the checker's **stdin** via a FIFO channel; the answer lives only in the
/// checker env. Tolerances for `tokens-float` map to `--epsilon`/`--rel-epsilon`.
pub fn resolve_builtin(format: &str, input: &ResolveCheckerInput) -> Result<CheckerStage, String> {
    let mode = builtin_compare_mode(format)
        .ok_or_else(|| format!("'{format}' is not a built-in comparison format"))?;

    // Built-in checkers consume the solution output on stdin via a FIFO. The
    // evaluator picked the channel name and passed it as the output binding.
    let channel = match &input.output_binding {
        OutputMode::Stream { channel } => channel.clone(),
        OutputMode::File { name } => {
            return Err(format!(
                "built-in checker '{format}' requires a Stream output binding, got File '{name}'"
            ));
        }
    };

    let mut argv = vec![
        BROCCOLI_COMPARE_PATH.to_string(),
        "--mode".to_string(),
        mode.to_string(),
        "--answer".to_string(),
        ANSWER_FILE.to_string(),
    ];
    // Float identity: map config tolerances to BOTH flags so a custom rel_tol
    // never flips AC<->WA once fused (broccoli-compare's combined rule needs both).
    if format == "tokens-float" {
        let (abs_tol, rel_tol) = float_tolerances(input.config.as_ref())?;
        argv.push("--epsilon".to_string());
        argv.push(format_tol(abs_tol));
        argv.push("--rel-epsilon".to_string());
        argv.push(format_tol(rel_tol));
    }

    let check = Step {
        id: CHECK_STEP_ID.to_string(),
        kind: StepKind::Checker,
        env_ref: CHECKER_ENV_ID.to_string(),
        argv,
        conf: RunOptions {
            resource_limits: ResourceLimits {
                time_limit: Some(BUILTIN_CHECK_TIME_LIMIT_S),
                wall_time_limit: Some(BUILTIN_CHECK_WALL_LIMIT_S),
                memory_limit: Some(BUILTIN_CHECK_MEMORY_LIMIT_KB),
                process_limit: Some(1),
                ..ResourceLimits::default()
            },
            ..RunOptions::default()
        },
        io: IOConfig {
            stdin: IOTarget::Pipe {
                name: channel.clone(),
            },
            // stdout = preview, stderr = message (broccoli-compare's channels).
            stdout: IOTarget::File {
                path: PREVIEW_FILE.to_string(),
            },
            stderr: IOTarget::File {
                path: MESSAGE_FILE.to_string(),
            },
        },
        // Nothing is collected: the message + preview return inline in the check
        // step's sandbox result (<=64 KiB each); the full output is consumed
        // worker-side and never leaves the worker.
        collect: vec![],
        // The evaluator adds the dependency on the solution's run step when it
        // wires the FIFO; the resolver leaves intra-stage deps empty.
        depends_on: vec![],
        cache: None,
        mounts: vec![MountSpec {
            inside_path: BROCCOLI_COMPARE_PATH.to_string(),
            source: MountSource::PlatformTool {
                name: BROCCOLI_COMPARE_TOOL.to_string(),
            },
        }],
    };

    let checker_env = Environment {
        id: CHECKER_ENV_ID.to_string(),
        files_in: vec![(
            ANSWER_FILE.to_string(),
            judge_file_to_session_file(&input.answer),
        )],
    };

    Ok(CheckerStage {
        checker_env,
        steps: vec![check],
        output_mode: OutputMode::Stream { channel },
        result_step_id: CHECK_STEP_ID.to_string(),
    })
}

/// Resolve a testlib checker into a File-mode stage: `[compile_checker (cached),
/// check]`. The solution output is handed to the checker as a worker-local
/// read-only file via a `StepOutput` mount (no blob round-trip). The answer and
/// test input live only in the checker env. `resolved` is the testlib checker's
/// compiled language spec; `config` carries the testlib resource limits.
pub fn build_testlib_checker_stage(
    checker_source: &[SourceFile],
    test_input: &JudgeFile,
    answer: &JudgeFile,
    resolved: &ResolveLanguageOutput,
    config: &crate::checkers::testlib::TestlibConfig,
    output_binding: &OutputMode,
) -> Result<CheckerStage, String> {
    // testlib checkers read the solution output as a FILE handed over from the
    // run step; a Stream binding makes no sense for them.
    let output_file = match output_binding {
        OutputMode::File { name } => name.clone(),
        OutputMode::Stream { channel } => {
            return Err(format!(
                "testlib checker requires a File output binding, got Stream '{channel}'"
            ));
        }
    };

    // Checker env: checker sources + input.txt + answer.txt. The solution output
    // (output.txt) is NOT a static env file - it is mounted from the run step.
    // The answer never appears in the solution env.
    let mut files_in: Vec<(String, SessionFile)> = checker_source
        .iter()
        .map(|s| {
            (
                s.filename.clone(),
                SessionFile::Content {
                    content: s.content.clone(),
                },
            )
        })
        .collect();
    files_in.push((
        "input.txt".to_string(),
        judge_file_to_session_file(test_input),
    ));
    files_in.push(("answer.txt".to_string(), judge_file_to_session_file(answer)));

    let mut steps = Vec::new();
    let compiled = resolved.compile.is_some();
    if let Some(compile_step) =
        crate::checkers::testlib::build_compile_checker_step(resolved, config, CHECKER_ENV_ID)
    {
        steps.push(compile_step);
    }

    let checker_binary = crate::checkers::testlib::testlib_checker_binary(resolved);

    steps.push(Step {
        id: CHECK_STEP_ID.to_string(),
        kind: StepKind::Checker,
        env_ref: CHECKER_ENV_ID.to_string(),
        argv: vec![
            checker_binary,
            "input.txt".to_string(),
            "output.txt".to_string(),
            "answer.txt".to_string(),
        ],
        conf: RunOptions {
            resource_limits: ResourceLimits {
                time_limit: Some(config.run_time_limit_s),
                memory_limit: Some(config.run_memory_limit_kb),
                ..Default::default()
            },
            ..Default::default()
        },
        io: IOConfig {
            stdin: IOTarget::Null,
            stdout: IOTarget::File {
                path: "checker_out.txt".to_string(),
            },
            stderr: IOTarget::File {
                path: "checker_err.txt".to_string(),
            },
        },
        collect: vec!["checker_out.txt".to_string(), "checker_err.txt".to_string()],
        // Resolver-level intra-stage dep only; the evaluator adds the dep on the
        // solution's `exec` step when it splices the stage (which also satisfies
        // the StepOutput mount's from_step requirement).
        depends_on: if compiled {
            vec!["compile_checker".to_string()]
        } else {
            vec![]
        },
        cache: None,
        mounts: vec![MountSpec {
            inside_path: "output.txt".to_string(),
            source: MountSource::StepOutput {
                from_step: SOLUTION_EXEC_STEP_ID.to_string(),
                file: output_file.clone(),
            },
        }],
    });

    Ok(CheckerStage {
        checker_env: Environment {
            id: CHECKER_ENV_ID.to_string(),
            files_in,
        },
        steps,
        output_mode: OutputMode::File { name: output_file },
        result_step_id: CHECK_STEP_ID.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Interpret: turn the check step's small result into a verdict.
// Runs server-side on tiny data; the 10 MB output was consumed worker-side.
// ---------------------------------------------------------------------------

/// Dispatch interpretation by format: built-in comparison formats use the
/// broccoli-compare exit-code mapping; `testlib` reuses the testlib mapping.
pub fn interpret_checker(format: &str, result: &CheckerRunOutcome) -> CheckerVerdict {
    if builtin_compare_mode(format).is_some() {
        interpret_builtin(result)
    } else if format == "testlib" {
        interpret_testlib(result)
    } else if format == "none" {
        // `none` performs no output comparison. The evaluator interprets it
        // inline (no check step is scheduled), so this is reached only by a
        // direct caller; keep it coherent - a reached interpretation is Accepted.
        CheckerVerdict {
            verdict: Verdict::Accepted,
            score: 1.0,
            message: None,
        }
    } else {
        system_error(format!(
            "No interpreter registered for checker format '{format}'"
        ))
    }
}

/// broccoli-compare exit codes: 0 = AC, 1 = WA, 2 = presentation error
/// (defensive - the native comparator does not emit it), anything else (incl.
/// 64 usage/IO error) = SystemError. No exit code = the checker step itself
/// failed in the sandbox.
fn interpret_builtin(result: &CheckerRunOutcome) -> CheckerVerdict {
    match result.exit_code {
        Some(0) => CheckerVerdict {
            verdict: Verdict::Accepted,
            score: 1.0,
            message: message_from(&result.stderr),
        },
        Some(1) => CheckerVerdict {
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            message: message_from(&result.stderr),
        },
        Some(2) => CheckerVerdict {
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            message: message_from(&result.stderr).or_else(|| Some("Presentation error".into())),
        },
        Some(other) => system_error(format!("Comparator exited with unexpected code {other}")),
        None => sandbox_failure_verdict("Comparator", result),
    }
}

/// testlib reuses the existing exit-code mapping verbatim (0/1/2/3/4/7/8/other);
/// only the sandbox-failure path differs, working off the reduced small result.
fn interpret_testlib(result: &CheckerRunOutcome) -> CheckerVerdict {
    match result.exit_code {
        Some(code) => crate::checkers::testlib::interpret_testlib_exit_code(code, &result.stderr),
        None => sandbox_failure_verdict("Checker", result),
    }
}

/// Map a sandbox failure (no exit code) to a SystemError verdict, over the
/// `CheckerRunOutcome` returned by the fused checker stage (which carries only
/// the status string + stderr, not the full ExecutionResult).
fn sandbox_failure_verdict(label: &str, result: &CheckerRunOutcome) -> CheckerVerdict {
    let status = result.sandbox_status.as_deref().unwrap_or("");
    let detail = result.stderr.trim();
    let message = match status {
        "TO" => format!("{label} timed out"),
        "SG" => format!("{label} was terminated by a signal"),
        "UNKNOWN" => format!("{label} step did not run; dependency may have failed"),
        s if !s.is_empty() && detail.is_empty() => {
            format!("{label} failed with sandbox status {s}")
        }
        s if !s.is_empty() => format!("{label} failed with sandbox status {s}: {detail}"),
        _ if detail.is_empty() => format!("{label} failed without an exit code"),
        _ => format!("{label} failed without an exit code: {detail}"),
    };
    system_error(crate::util::truncate(&message, 1024))
}

/// Trim + truncate a checker message; `None` when empty.
fn message_from(stderr: &str) -> Option<String> {
    let m = stderr.trim();
    if m.is_empty() {
        None
    } else {
        Some(crate::util::truncate(m, 1024))
    }
}

fn system_error(message: impl Into<String>) -> CheckerVerdict {
    CheckerVerdict {
        verdict: Verdict::SystemError,
        score: 0.0,
        message: Some(message.into()),
    }
}

/// Convert a `JudgeFile` into the `SessionFile` an `Environment` carries.
pub(crate) fn judge_file_to_session_file(file: &JudgeFile) -> SessionFile {
    match file {
        JudgeFile::Blob { file } => SessionFile::Blob {
            hash: file.blob_hash.clone(),
        },
        JudgeFile::Inline { text } => SessionFile::Content {
            content: text.clone(),
        },
        JudgeFile::Missing => SessionFile::Content {
            content: String::new(),
        },
    }
}

/// Parse `{ abs_tol, rel_tol }` from the checker config. A missing (or null)
/// field falls back to the WASM default; a field that is present but not a
/// number is a configuration error rather than a silent fallback to the tight
/// default, which would otherwise turn a mistyped tolerance (e.g. the string
/// `"1e-3"`) into spurious WrongAnswers with no error surfaced.
fn float_tolerances(config: Option<&serde_json::Value>) -> Result<(f64, f64), String> {
    let one = |key: &str, default: f64| -> Result<f64, String> {
        match config.and_then(|c| c.get(key)) {
            None | Some(serde_json::Value::Null) => Ok(default),
            Some(v) => v
                .as_f64()
                .ok_or_else(|| format!("checker config `{key}` must be a number, got {v}")),
        }
    };
    Ok((
        one("abs_tol", DEFAULT_ABS_TOL)?,
        one("rel_tol", DEFAULT_REL_TOL)?,
    ))
}

/// Render an f64 tolerance for the broccoli-compare argv. Rust's f64 `Display`
/// never uses exponent notation, so the value round-trips through the binary's
/// `f64::from_str`.
fn format_tol(tol: f64) -> String {
    format!("{tol}")
}

// ---------------------------------------------------------------------------
// WASM entry points (checker fusion). Registered in `init` via
// `register_checker_resolver`; the host dispatches to them by format. The host
// validates the returned CheckerStage / CheckerVerdict shapes.
// ---------------------------------------------------------------------------

/// Load the testlib checker source for a problem from this plugin's own
/// per-problem config (`standard-checkers:checker_source`), set by the problem
/// editor. The checker source is owned by the checker plugin, not the core
/// problem model nor the evaluator; the host does not inline it.
#[cfg(target_arch = "wasm32")]
fn load_checker_source(host: &Host, problem_id: Option<i32>) -> Result<Vec<SourceFile>, String> {
    let problem_id = problem_id
        .ok_or_else(|| "Testlib checker requires a problem_id to load its source".to_string())?;
    let result = host
        .config
        .get_problem(problem_id, "checker_source")
        .map_err(|e| format!("failed to load checker source config: {e}"))?;
    if result.is_default {
        return Err("Testlib checker requires checker_source".to_string());
    }
    let files: Vec<SourceFile> = serde_json::from_value(result.config)
        .map_err(|e| format!("invalid checker_source config: {e}"))?;
    if files.is_empty() {
        return Err("Testlib checker requires checker_source".to_string());
    }
    Ok(files)
}

/// Dispatch a resolve request to the right stage builder. testlib additionally
/// loads its checker source (problem-scoped config) and asks the language plugin
/// to resolve it (both host calls).
#[cfg(target_arch = "wasm32")]
fn resolve_stage(host: &Host, req: &ResolveCheckerInput) -> Result<CheckerStage, String> {
    if builtin_compare_mode(&req.format).is_some() {
        resolve_builtin(&req.format, req)
    } else if req.format == "testlib" {
        let checker_source = load_checker_source(host, req.problem_id)?;
        let (resolved, config) =
            crate::checkers::testlib::resolve_testlib_compile(host, &checker_source)?;
        build_testlib_checker_stage(
            &checker_source,
            &req.test_input,
            &req.answer,
            &resolved,
            &config,
            &req.output_binding,
        )
    } else if req.format == "none" {
        // `none` performs no comparison; the evaluator schedules no checker step
        // and interprets it inline. Resolving it as a stage is a contract bug -
        // fail loudly rather than splice an empty/meaningless stage.
        Err(
            "checker format 'none' performs no comparison and must be interpreted \
             inline, not resolved as a checker stage"
                .to_string(),
        )
    } else {
        Err(format!(
            "No checker resolver for format '{}' in standard-checkers",
            req.format
        ))
    }
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn resolve_standard_checker(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: ResolveCheckerInput = serde_json::from_str(&input)?;
    let stage = resolve_stage(&host, &req).map_err(extism_pdk::Error::msg)?;
    Ok(serde_json::to_string(&stage)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn interpret_standard_checker_result(input: String) -> FnResult<String> {
    let req: InterpretCheckerInput = serde_json::from_str(&input)?;
    let verdict = interpret_checker(&req.format, &req.result);
    Ok(serde_json::to_string(&verdict)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens_resolve_input(channel: &str) -> ResolveCheckerInput {
        ResolveCheckerInput {
            format: "tokens".to_string(),
            answer: JudgeFile::inline("3 1 4\n"),
            test_input: JudgeFile::inline("3\n"),
            problem_id: None,
            config: None,
            output_binding: OutputMode::Stream {
                channel: channel.to_string(),
            },
        }
    }

    #[test]
    fn resolve_builtin_tokens_builds_stream_stage() {
        let input = tokens_resolve_input("sol_out");
        let stage = resolve_builtin("tokens", &input).expect("tokens should resolve");

        // Single check step, Stream output, result points at it.
        assert_eq!(stage.steps.len(), 1);
        assert_eq!(stage.result_step_id, CHECK_STEP_ID);
        match &stage.output_mode {
            OutputMode::Stream { channel } => assert_eq!(channel, "sol_out"),
            other => panic!("expected Stream output_mode, got {other:?}"),
        }

        let check = &stage.steps[0];
        assert_eq!(check.id, CHECK_STEP_ID);
        assert_eq!(check.kind, StepKind::Checker);
        assert_eq!(check.env_ref, CHECKER_ENV_ID);

        // argv: broccoli-compare --mode tokens --answer answer.txt
        assert_eq!(
            check.argv,
            vec![
                BROCCOLI_COMPARE_PATH.to_string(),
                "--mode".to_string(),
                "tokens".to_string(),
                "--answer".to_string(),
                ANSWER_FILE.to_string(),
            ]
        );

        // Solution output on stdin via the FIFO channel.
        match &check.io.stdin {
            IOTarget::Pipe { name } => assert_eq!(name, "sol_out"),
            other => panic!("expected stdin Pipe, got {other:?}"),
        }
        // stdout = preview, stderr = message (both returned inline).
        match &check.io.stdout {
            IOTarget::File { path } => assert_eq!(path, PREVIEW_FILE),
            other => panic!("expected stdout File, got {other:?}"),
        }
        match &check.io.stderr {
            IOTarget::File { path } => assert_eq!(path, MESSAGE_FILE),
            other => panic!("expected stderr File, got {other:?}"),
        }

        // Comparator provided by the worker as a platform tool.
        assert_eq!(check.mounts.len(), 1);
        assert_eq!(check.mounts[0].inside_path, BROCCOLI_COMPARE_PATH);
        match &check.mounts[0].source {
            MountSource::PlatformTool { name } => assert_eq!(name, BROCCOLI_COMPARE_TOOL),
            other => panic!("expected PlatformTool mount, got {other:?}"),
        }

        // Nothing collected - message + preview return inline in the result.
        assert!(check.collect.is_empty());

        // The answer is ONLY in the checker env (never the solution env, which
        // this stage does not even describe).
        assert_eq!(stage.checker_env.id, CHECKER_ENV_ID);
        let answer_entry = stage
            .checker_env
            .files_in
            .iter()
            .find(|(name, _)| name == ANSWER_FILE)
            .expect("answer.txt must be in the checker env");
        match &answer_entry.1 {
            SessionFile::Content { content } => assert_eq!(content, "3 1 4\n"),
            other => panic!("expected inline answer content, got {other:?}"),
        }
    }

    #[test]
    fn resolve_builtin_tokens_float_maps_both_tolerances() {
        let mut input = tokens_resolve_input("sol_out");
        input.format = "tokens-float".to_string();
        input.config = Some(serde_json::json!({ "abs_tol": 1e-4, "rel_tol": 1e-3 }));

        let stage = resolve_builtin("tokens-float", &input).expect("tokens-float resolves");
        let argv = &stage.steps[0].argv;

        // --epsilon <abs_tol> and --rel-epsilon <rel_tol> must BOTH be present.
        let eps = argv
            .iter()
            .position(|a| a == "--epsilon")
            .expect("--epsilon");
        assert_eq!(argv[eps + 1], "0.0001");
        let rel = argv
            .iter()
            .position(|a| a == "--rel-epsilon")
            .expect("--rel-epsilon");
        assert_eq!(argv[rel + 1], "0.001");
    }

    #[test]
    fn resolve_builtin_tokens_float_defaults_when_no_config() {
        let mut input = tokens_resolve_input("sol_out");
        input.format = "tokens-float".to_string();
        input.config = None;

        let stage = resolve_builtin("tokens-float", &input).unwrap();
        let argv = &stage.steps[0].argv;
        let eps = argv.iter().position(|a| a == "--epsilon").unwrap();
        assert_eq!(argv[eps + 1], "0.000000001"); // 1e-9, no exponent notation
        let rel = argv.iter().position(|a| a == "--rel-epsilon").unwrap();
        assert_eq!(argv[rel + 1], "0.000001"); // 1e-6
    }

    #[test]
    fn resolve_builtin_rejects_non_builtin_format() {
        let input = tokens_resolve_input("sol_out");
        assert!(resolve_builtin("testlib", &input).is_err());
    }

    #[test]
    fn resolve_builtin_rejects_file_binding() {
        let mut input = tokens_resolve_input("sol_out");
        input.output_binding = OutputMode::File {
            name: "output.txt".to_string(),
        };
        assert!(resolve_builtin("tokens", &input).is_err());
    }

    fn small(exit_code: Option<i32>, stderr: &str, status: Option<&str>) -> CheckerRunOutcome {
        CheckerRunOutcome {
            exit_code,
            stderr: stderr.to_string(),
            sandbox_status: status.map(|s| s.to_string()),
        }
    }

    #[test]
    fn interpret_builtin_exit_0_accepted() {
        let v = interpret_checker("tokens", &small(Some(0), "", Some("OK")));
        assert_eq!(v.verdict, Verdict::Accepted);
        assert_eq!(v.score, 1.0);
    }

    #[test]
    fn interpret_builtin_exit_1_wrong_answer_keeps_message() {
        let v = interpret_checker(
            "tokens",
            &small(Some(1), "token 3: expected 4, got 5", Some("OK")),
        );
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert_eq!(v.score, 0.0);
        assert_eq!(v.message.as_deref(), Some("token 3: expected 4, got 5"));
    }

    #[test]
    fn interpret_builtin_exit_2_presentation_error() {
        let v = interpret_checker("exact", &small(Some(2), "", Some("OK")));
        assert_eq!(v.verdict, Verdict::WrongAnswer);
        assert!(v.message.unwrap().contains("Presentation error"));
    }

    #[test]
    fn interpret_builtin_unexpected_exit_is_system_error() {
        // broccoli-compare emits 64 on usage/IO error.
        let v = interpret_checker("tokens", &small(Some(64), "bad args", Some("OK")));
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn interpret_builtin_sandbox_timeout_is_system_error() {
        let v = interpret_checker("tokens", &small(None, "", Some("TO")));
        assert_eq!(v.verdict, Verdict::SystemError);
        assert_eq!(v.score, 0.0);
        assert!(v.message.unwrap().to_lowercase().contains("timed out"));
    }

    #[test]
    fn interpret_testlib_routes_to_testlib_mapping() {
        // 3 -> judge failure (SystemError), 7 -> partial - testlib-specific codes
        // a built-in comparator never emits.
        let se = interpret_checker("testlib", &small(Some(3), "FAIL bug", None));
        assert_eq!(se.verdict, Verdict::SystemError);

        let partial = interpret_checker("testlib", &small(Some(7), "points 0.5 half", None));
        assert_eq!(partial.verdict, Verdict::WrongAnswer);
        assert_eq!(partial.score, 0.5);
    }

    #[test]
    fn interpret_unknown_format_is_system_error() {
        let v = interpret_checker("bogus", &small(Some(0), "", None));
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn interpret_none_is_accepted() {
        // `none` performs no comparison; the evaluator interprets it inline and
        // normally never calls this, but a direct caller must get a coherent
        // Accepted (never SystemError) regardless of the unused check result.
        let v = interpret_checker("none", &small(Some(0), "", Some("OK")));
        assert_eq!(v.verdict, Verdict::Accepted);
        assert_eq!(v.score, 1.0);
        assert!(v.message.is_none());
    }

    fn resolved_with_compile() -> ResolveLanguageOutput {
        ResolveLanguageOutput {
            compile: Some(CompileSpec {
                command: vec![
                    "/usr/bin/g++".to_string(),
                    "checker.cpp".to_string(),
                    "-o".to_string(),
                    "checker".to_string(),
                ],
                cache_inputs: vec!["checker.cpp".to_string()],
                outputs: vec![OutputSpec::File("checker".to_string())],
                resource_limits: None,
            }),
            run: RunSpec {
                command: vec!["./checker".to_string()],
                extra_files: vec![],
                min_process_limit: None,
            },
        }
    }

    fn testlib_stage() -> CheckerStage {
        let checker_source = vec![SourceFile {
            filename: "checker.cpp".to_string(),
            content: "int main(){}".to_string(),
        }];
        build_testlib_checker_stage(
            &checker_source,
            &JudgeFile::inline("3\n"),
            &JudgeFile::inline("3 1 4\n"),
            &resolved_with_compile(),
            &crate::checkers::testlib::TestlibConfig::schema_defaults(),
            &OutputMode::File {
                name: "output.txt".to_string(),
            },
        )
        .expect("testlib resolves")
    }

    #[test]
    fn testlib_stage_has_compile_then_check() {
        let stage = testlib_stage();
        assert_eq!(stage.steps.len(), 2);
        assert_eq!(stage.steps[0].id, "compile_checker");
        assert_eq!(stage.steps[0].kind, StepKind::CheckerCompile);
        assert!(
            stage.steps[0].cache.is_some(),
            "compile step must be cached"
        );
        assert_eq!(stage.steps[1].id, CHECK_STEP_ID);
        assert_eq!(stage.steps[1].kind, StepKind::Checker);
        assert_eq!(stage.result_step_id, CHECK_STEP_ID);
    }

    #[test]
    fn testlib_stage_is_file_mode() {
        match testlib_stage().output_mode {
            OutputMode::File { name } => assert_eq!(name, "output.txt"),
            other => panic!("expected File output_mode, got {other:?}"),
        }
    }

    #[test]
    fn testlib_check_argv_is_checker_input_output_answer() {
        let stage = testlib_stage();
        assert_eq!(
            stage.steps[1].argv,
            vec![
                "./checker".to_string(),
                "input.txt".to_string(),
                "output.txt".to_string(),
                "answer.txt".to_string(),
            ]
        );
    }

    #[test]
    fn testlib_check_mounts_solution_output() {
        let stage = testlib_stage();
        let check = &stage.steps[1];
        assert_eq!(check.mounts.len(), 1);
        assert_eq!(check.mounts[0].inside_path, "output.txt");
        match &check.mounts[0].source {
            MountSource::StepOutput { from_step, file } => {
                assert_eq!(from_step, SOLUTION_EXEC_STEP_ID);
                assert_eq!(file, "output.txt");
            }
            other => panic!("expected StepOutput mount, got {other:?}"),
        }
        // The resolver wires the intra-stage dep on the compile step; the
        // evaluator adds the dep on the solution's exec step when splicing.
        assert!(check.depends_on.contains(&"compile_checker".to_string()));
    }

    #[test]
    fn testlib_answer_and_input_only_in_checker_env_not_output() {
        let stage = testlib_stage();
        let names: Vec<&str> = stage
            .checker_env
            .files_in
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert!(names.contains(&"answer.txt"), "answer in checker env");
        assert!(names.contains(&"input.txt"), "input in checker env");
        assert!(
            names.contains(&"checker.cpp"),
            "checker source in checker env"
        );
        // The solution output is mounted, NOT a static env file.
        assert!(
            !names.contains(&"output.txt"),
            "output.txt must be mounted from the run step, not a static env file"
        );
    }

    #[test]
    fn testlib_rejects_stream_binding() {
        let checker_source = vec![SourceFile {
            filename: "checker.cpp".to_string(),
            content: "int main(){}".to_string(),
        }];
        let r = build_testlib_checker_stage(
            &checker_source,
            &JudgeFile::inline("3\n"),
            &JudgeFile::inline("3 1 4\n"),
            &resolved_with_compile(),
            &crate::checkers::testlib::TestlibConfig::schema_defaults(),
            &OutputMode::Stream {
                channel: "sol_out".to_string(),
            },
        );
        assert!(r.is_err());
    }
}
