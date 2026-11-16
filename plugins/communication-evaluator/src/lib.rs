#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::types::ResolveLanguageInput;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};

pub mod config;
pub mod interpret;
pub mod operation;

#[cfg(target_arch = "wasm32")]
fn load_sandbox_config(host: &Host) -> Result<config::SandboxConfig, SdkError> {
    host.config.get_global("sandbox")?.parse()
}

#[cfg(target_arch = "wasm32")]
fn load_comm_config(host: &Host, problem_id: i32) -> Result<config::CommConfig, SdkError> {
    host.config
        .get_problem(problem_id, "communication")?
        .parse()
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();
    host.registry
        .register_evaluator("communication", "evaluate_communication")?;
    host.log.info("Communication evaluator registered")?;
    Ok("ok".to_string())
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn evaluate_communication(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: BuildEvalOpsInput = serde_json::from_str(&input)?;
    let tc_id = req.test_case_id;
    let problem_id = req.problem_id;

    let sandbox_config = load_sandbox_config(&host)?;
    let comm_config = load_comm_config(&host, problem_id)?;

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
    let contestant_lang = host
        .language
        .resolve(&ResolveLanguageInput {
            language_id: req.solution_language.clone(),
            submitted_files,
            additional_files: req.additional_file_refs.clone(),
            problem_id: Some(problem_id),
            contest_id: req.contest_id,
            overrides: None,
        })
        .map_err(|e| extism_pdk::Error::msg(format!("{e}")))?;

    if comm_config.manager_sources.is_empty() {
        return Err(extism_pdk::Error::msg(
            "Communication config has no manager_sources. \
             Upload manager source files and set them in the problem's communication config.",
        )
        .into());
    }

    let mgr_files: Vec<String> = comm_config
        .manager_sources
        .iter()
        .map(|s| s.filename.clone())
        .collect();
    let manager_lang = host
        .language
        .resolve(&ResolveLanguageInput {
            language_id: comm_config.manager_language.clone(),
            submitted_files: mgr_files,
            additional_files: vec![],
            problem_id: Some(problem_id),
            contest_id: req.contest_id,
            overrides: None,
        })
        .map_err(|e| extism_pdk::Error::msg(format!("Manager language resolve: {e}")))?;

    let operations = operation::build_operation(
        &req,
        &contestant_lang,
        &manager_lang,
        &comm_config.manager_sources,
        &comm_config,
        &sandbox_config,
    )
    .map_err(|e| extism_pdk::Error::msg(format!("{e}")))?;

    let compile_units = u32::from(manager_lang.compile.is_some())
        + if contestant_lang.compile.is_some() {
            comm_config.num_processes
        } else {
            0
        };
    let result_timeout_ms = sandbox_config.result_timeout_ms_for(
        req.time_limit_ms,
        compile_units,
        comm_config.manager_time_limit_s,
    );

    let mut results = host
        .operations
        .windowed(&operations)
        .collect(result_timeout_ms)
        .map_err(|e| extism_pdk::Error::msg(format!("{e}")))?;
    if results.len() != 1 {
        return Err(extism_pdk::Error::msg(format!(
            "expected exactly one operation result, got {}",
            results.len()
        ))
        .into());
    }
    let result = results.remove(0);

    let memory_limit_kb = u32::try_from(req.memory_limit_kb).unwrap_or(u32::MAX);
    let verdict = interpret::interpret_result(
        tc_id,
        &result,
        comm_config.num_processes as usize,
        memory_limit_kb,
    );

    Ok(serde_json::to_string(&verdict)?)
}
