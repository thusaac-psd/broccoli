pub mod api;
pub mod config;
pub mod evaluate_batch;
pub mod feedback;
pub mod judge;
pub mod persist;
pub mod score;
pub mod scoreboard;
pub mod scoring;
pub mod subtasks;
pub mod tokens;

// Host/SdkError/TestCaseRow (via the prelude) and everything below are only
// used by the wasm32-gated plugin entry points and loader helpers in this
// file; this file has no #[cfg(test)] unit tests of its own, so gate the
// same way a native (test/clippy) build doesn't see these as unused.
#[cfg(target_arch = "wasm32")]
use broccoli_server_sdk::prelude::*;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};

#[cfg(target_arch = "wasm32")]
use crate::config::{SubtaskDef, TaskConfig};
#[cfg(target_arch = "wasm32")]
use crate::evaluate_batch::{handle_detached_eval_callback, recover_detached_callback_error};
#[cfg(target_arch = "wasm32")]
use crate::subtasks::build_default_subtasks;
#[cfg(target_arch = "wasm32")]
use crate::tokens::TokenState;

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_task_config(
    host: &Host,
    contest_id: i32,
    problem_id: i32,
) -> Result<TaskConfig, SdkError> {
    Ok(serde_json::from_value(
        host.config
            .get_contest_problem(contest_id, problem_id, "task")?
            .config,
    )
    .unwrap_or_default())
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_effective_subtasks(
    host: &Host,
    problem_id: i32,
    task_config: &TaskConfig,
) -> Result<(Vec<TestCaseRow>, Vec<SubtaskDef>), SdkError> {
    let test_cases = host.submission.query_test_cases(problem_id)?;
    let subtasks = if task_config.subtasks.is_empty() {
        build_default_subtasks(&test_cases)
    } else {
        task_config.subtasks.clone()
    };

    Ok((test_cases, subtasks))
}

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_token_state(
    host: &Host,
    contest_id: i32,
    user_id: i32,
) -> Result<TokenState, SdkError> {
    let key = format!("tokens:{contest_id}:{user_id}");
    match host.storage.get_one(&key)? {
        Some(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
        None => Ok(TokenState::default()),
    }
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn init() -> FnResult<String> {
    let host = Host::new();
    host.registry
        .register_contest_type("ioi", "handle_ioi_submission", "handle_ioi_code_run")?;
    host.log.info("IOI contest plugin registered")?;
    Ok("ok".into())
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_ioi_submission(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: OnSubmissionInput = serde_json::from_str(&input)?;

    let output = match req.contest_id {
        None => OnSubmissionOutput {
            success: false,
            error_message: Some("IOI plugin requires contest_id".into()),
        },
        Some(id) => {
            host.log.info(&format!(
                "IOI: Judging submission {} for problem {} in contest {}",
                req.submission_id, req.problem_id, id
            ))?;
            match score::run_judge(&host, &req, id) {
                Ok(out) => out,
                Err(SdkError::StaleEpoch) => OnSubmissionOutput {
                    success: true,
                    error_message: None,
                },
                Err(e) => OnSubmissionOutput {
                    success: false,
                    error_message: Some(format!("{e:?}")),
                },
            }
        }
    };
    Ok(serde_json::to_string(&output)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_ioi_code_run(input: String) -> FnResult<String> {
    let host = Host::new();
    Ok(broccoli_server_sdk::evaluator::handle_code_run(
        &host, &input,
    )?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn on_ioi_eval_result(input: String) -> FnResult<String> {
    let host = Host::new();
    let input: DetachedEvaluateCallbackInput = serde_json::from_str(&input)?;
    // Snapshot the session state so a mid-stream persist failure can still
    // resolve the submission instead of stranding it: without this, the bare `?`
    // would abort the WASM callback on any error (including a benign StaleEpoch
    // from a concurrent rejudge), leaving the submission stuck on "Judging" with
    // no terminal verdict.
    let state_snapshot = input.state.clone();
    let output = match handle_detached_eval_callback(&host, input) {
        Ok(out) => out,
        Err(SdkError::StaleEpoch) => {
            // A newer judgement superseded this session - stop cleanly. The
            // newer epoch already owns the submission; nothing to finalize.
            let _ = host
                .log
                .info("IOI: detached callback epoch stale, cancelling");
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
        Err(e) => {
            // Transient persist failure mid-stream. Funnel the submission to a
            // terminal SystemError rather than leaving it on "Judging".
            let _ = host.log.info(&format!(
                "IOI: detached callback failed ({e:?}); finalizing as SystemError"
            ));
            recover_detached_callback_error(&host, &state_snapshot);
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
    };
    Ok(serde_json::to_string(&output)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn decide_visibility(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: VisibilityQueryInput = serde_json::from_str(&input)?;
    let decisions = feedback::decide_visibility_decisions(&host, &req)?;
    Ok(serde_json::to_string(&VisibilityQueryOutput { decisions })?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_use_token(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_use_token)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_contest_info(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_contest_info)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_task_config(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_task_config)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_submission_status(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_submission_status)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_token_status(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_token_status)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_scoreboard(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_scoreboard)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn api_submission_subtask_scores(input: String) -> FnResult<String> {
    run_api_handler(&input, api::handle_submission_subtask_scores)
}
