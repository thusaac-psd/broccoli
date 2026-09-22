//! Test-only judging fixture for the afternoon-bracket integration test
//! (`packages/server/tests/integration/afternoon_bracket.rs`).
//!
//! The afternoon-bracket plugin itself never runs code: `AfternoonBracketJudge`
//! (`plugins/afternoon-bracket/src/judge.rs`) drives `DetachedEval`, which asks
//! the HOST to run one `evaluate` call per test case for the problem's
//! registered `problem_type`. In production that's `batch-evaluator`, which
//! actually compiles/runs the submission in a sandboxed worker. This fixture
//! stands in for that sandbox so the integration test can drive real
//! submissions through the real `DetachedEval`/host dispatch machinery
//! without needing a real compiler toolchain: it decides Accepted/WrongAnswer
//! purely by reading the submitted source verbatim (`"ACCEPT"` -> AC, anything
//! else -> WA), matching the exact `BuildEvalOpsInput` -> `TestCaseVerdict`
//! contract `batch-evaluator::evaluate_batch` implements for real.
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::types::{BuildEvalOpsInput, ResolveLanguageOutput, RunSpec, TestCaseVerdict};
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();
    host.registry
        .register_evaluator("afternoon-judge-fixture", "evaluate")?;
    host.registry.register_language_resolver(
        "cpp",
        "resolve_language",
        "C++ (fixture)",
        "main.cpp",
        &["cpp", "cc", "cxx"],
        "",
    )?;
    host.log.info("afternoon-judge-fixture registered")?;
    Ok("ok".to_string())
}

/// Registered evaluator handler for problem_type "afternoon-judge-fixture".
/// Contract: input is `BuildEvalOpsInput`, output is a `TestCaseVerdict` for
/// exactly the one test case named in the input - identical to
/// `batch-evaluator::evaluate_batch`'s contract, minus the real sandboxing.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn evaluate(input: String) -> FnResult<String> {
    let req: BuildEvalOpsInput = serde_json::from_str(&input)?;
    let submitted = req
        .solution_source
        .first()
        .map(|f| f.content.trim())
        .unwrap_or_default();

    let verdict = if submitted == "ACCEPT" {
        TestCaseVerdict::accepted(req.test_case_id)
    } else {
        TestCaseVerdict::wrong_answer(req.test_case_id)
    };

    Ok(serde_json::to_string(&verdict)?)
}

/// Registered language resolver handler for "cpp". Never actually invoked by
/// this test's flow - `evaluate` above never calls `host.language.resolve` -
/// but a language must be registered under this exact `language_id` for
/// `known_languages` (`packages/server/src/utils/judging.rs`) to accept
/// "cpp" as a valid submission language at all.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn resolve_language(_input: String) -> FnResult<String> {
    let output = ResolveLanguageOutput {
        compile: None,
        run: RunSpec {
            command: vec!["true".to_string()],
            extra_files: vec![],
            min_process_limit: None,
        },
    };
    Ok(serde_json::to_string(&output)?)
}
