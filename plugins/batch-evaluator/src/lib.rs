#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::types::{
    BuildEvalOpsInput, EvaluateOperationResultsInput, OperationResult, OutputMode,
    PreparedEvaluateCase, ResolveCheckerInput, ResolveLanguageInput, TestCaseVerdict, Verdict,
};
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};

pub mod batch;

#[cfg(target_arch = "wasm32")]
fn load_sandbox_config(host: &Host) -> Result<batch::SandboxConfig, SdkError> {
    host.config.get_global("sandbox")?.parse()
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();
    host.registry.register_staged_evaluator(
        "batch",
        "prepare_evaluate_case",
        "on_operation_results",
    )?;
    host.log.info("Batch evaluator registered")?;
    Ok("ok".to_string())
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn prepare_evaluate_case(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: BuildEvalOpsInput = serde_json::from_str(&input)?;
    let prepared = prepare_case(&host, &req)?;
    Ok(serde_json::to_string(&prepared)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn on_operation_results(input: String) -> FnResult<String> {
    let host = Host::new();
    let input: EvaluateOperationResultsInput = serde_json::from_str(&input)?;
    let verdicts = input
        .results
        .into_iter()
        .map(|item| match item.operation_results.first() {
            Some(result) => interpret_case_result(&host, &item.case, result)
                .unwrap_or_else(|e| system_error(item.case.test_case_id, e.to_string())),
            None => system_error(item.case.test_case_id, "Operation produced no result"),
        })
        .collect::<Vec<_>>();

    Ok(serde_json::to_string(&verdicts)?)
}

#[cfg(target_arch = "wasm32")]
fn prepare_case(
    host: &Host,
    req: &BuildEvalOpsInput,
) -> Result<PreparedEvaluateCase, extism_pdk::Error> {
    let sandbox_config = load_sandbox_config(host)?;

    let additional_filenames: std::collections::HashSet<&str> = req
        .additional_file_refs
        .iter()
        .map(|f| f.filename.as_str())
        .collect();
    let submitted_files: Vec<String> = req
        .solution_source
        .iter()
        .filter(|f| !additional_filenames.contains(f.filename.as_str()))
        .map(|f| f.filename.clone())
        .collect();
    let resolved = host
        .language
        .resolve(&ResolveLanguageInput {
            language_id: req.solution_language.clone(),
            submitted_files,
            additional_files: req.additional_file_refs.clone(),
            problem_id: Some(req.problem_id),
            contest_id: req.contest_id,
            overrides: None,
        })
        .map_err(|e| extism_pdk::Error::msg(format!("{e}")))?;

    // Checker fusion: resolve a checker stage to splice into the run op so the
    // solution output is checked worker-side. `none` schedules NO checker stage
    // (no comparison; the solution output is captured normally for display) and
    // is interpreted inline by the fused interpret path.
    let checker_format = req.checker_format.as_str();
    let checker_stage = if checker_format == "none" {
        None
    } else {
        let stage = host
            .checker
            .resolve(&ResolveCheckerInput {
                format: checker_format.to_string(),
                answer: req.expected_output.clone(),
                test_input: req.test_input.clone(),
                problem_id: Some(req.problem_id),
                output_binding: checker_output_binding(checker_format),
            })
            .map_err(|e| extism_pdk::Error::msg(format!("checker resolve failed: {e}")))?;
        Some(stage)
    };

    let operations =
        batch::build_operation(req, &resolved, &sandbox_config, checker_stage.as_ref())
            .map_err(|e| extism_pdk::Error::msg(format!("{e}")))?;
    // Base budget covers compile + exec; add the checker stage's wall budget so a
    // sequential (File-mode) checker - e.g. a cold testlib compile - doesn't trip
    // the result-wait timeout.
    let result_timeout_ms = sandbox_config
        .result_timeout_ms_for(req.time_limit_ms, u32::from(resolved.compile.is_some()))
        + checker_stage
            .as_ref()
            .map(batch::checker_stage_timeout_ms)
            .unwrap_or(0);

    Ok(PreparedEvaluateCase {
        operations,
        result_timeout_ms,
    })
}

/// The proposed output binding for a checker format. Mirrors standard-checkers'
/// modes: testlib reads the solution output as a FILE; every built-in comparator
/// reads it on STDIN (Stream). The evaluator wires exec's output side from the
/// RESOLVED stage's `output_mode`, so this only names the channel/file the
/// resolver wires its checker input to.
#[cfg(target_arch = "wasm32")]
fn checker_output_binding(format: &str) -> OutputMode {
    match format {
        "testlib" => OutputMode::File {
            name: "output.txt".to_string(),
        },
        _ => OutputMode::Stream {
            channel: "sol_out".to_string(),
        },
    }
}

#[cfg(target_arch = "wasm32")]
fn interpret_case_result(
    host: &Host,
    req: &BuildEvalOpsInput,
    result: &OperationResult,
) -> Result<TestCaseVerdict, extism_pdk::Error> {
    let tc_id = req.test_case_id;
    let checker_format = req.checker_format.as_str();

    // Every format interprets via the fused path. Comparison formats read the
    // worker-side `check` step's small result; `none` schedules no check step and
    // is handled inline by interpret_fused_result (precheck wins, else Accepted).
    let verdict = evaluator::interpret_fused_result(
        &host.checker,
        tc_id,
        result,
        checker_format,
        evaluator::DEFAULT_CHECK_STEP_ID,
    )
    .map_err(|e| extism_pdk::Error::msg(format!("{e}")))?;
    // A SystemError is a judge/system fault, never the contestant's code. Log each
    // step's raw sandbox result so the cause is diagnosable straight from the log
    // (exit/signal/status/oom/memory) instead of needing a repro.
    if verdict.verdict == Verdict::SystemError {
        let steps: Vec<String> = result
            .task_results
            .iter()
            .map(|(k, t)| {
                let s = &t.sandbox_result;
                format!(
                    "{k}{{ok={},exit={:?},sig={:?},status={:?},oom={},mem={:?}KB,t={:.2},out={}B,msg={:?},err={:?}}}",
                    t.success,
                    s.exit_code,
                    s.signal,
                    s.status,
                    s.cg_oom_killed,
                    s.memory_used,
                    s.time_used,
                    s.stdout.len(),
                    s.message.chars().take(120).collect::<String>(),
                    s.stderr.chars().take(200).collect::<String>()
                )
            })
            .collect();
        let _ = host.log.info(&format!(
            "SystemError on tc={tc_id}: op_success={} op_err={:?} steps=[{}]",
            result.success,
            result.error,
            steps.join(", ")
        ));
    }
    Ok(verdict)
}

#[cfg(target_arch = "wasm32")]
fn system_error(test_case_id: i32, message: impl Into<String>) -> TestCaseVerdict {
    TestCaseVerdict {
        test_case_id,
        verdict: Verdict::SystemError,
        score: 0.0,
        time_used_ms: None,
        memory_used_kb: None,
        message: Some(message.into()),
        stdout: None,
        stderr: None,
    }
}
