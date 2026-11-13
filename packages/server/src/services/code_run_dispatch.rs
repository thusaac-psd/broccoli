use std::time::Duration;

use broccoli_server_sdk::types::{
    OnCodeRunInput, OnCodeRunOutput, SourceFile, TestCaseBodyRef, TestCaseRow,
};
use sea_orm::EntityTrait;
use tracing::{Instrument, error, info, instrument, warn};

use plugin_core::retry::{PoolRetryPolicy, call_raw_with_pool_retry};

use crate::entity::{code_run, problem};
use crate::models::code_run::CustomTestCaseInput;
use crate::services::judging::{self, Unit};
use crate::state::AppState;
use crate::utils::judging::files_log_summary;

/// Loose safety net for a code run's SYNCHRONOUS `evaluate_run` plugin call. It
/// must exceed the inner budget or it fails legitimate runs: `evaluate_run` waits
/// up to `DEFAULT_EVALUATION_RESULT_TIMEOUT_MAX_MS` (60 min) PER operation result,
/// a code run chains compile + run, and under a deep shared worker queue an
/// operation legitimately waits (the operation reaper floor alone is 30 min). The
/// previous 180s cap undercut all of that, spuriously turning custom runs into
/// DISPATCH_TIMEOUT SystemErrors under exactly the backlog conditions the
/// operation infra is built to tolerate. Sized well above the worst-case inner
/// budget so only a genuinely stuck call trips it.
const CODE_RUN_DISPATCH_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);

/// Finalizes a code run whose dispatch failed as SystemError, fenced on the
/// dispatch's epoch, logging a failure of the write itself: the run then
/// stays in flight until a detector requeues it.
async fn mark_code_run_error_or_log(
    db: &sea_orm::DatabaseConnection,
    code_run_id: i32,
    code: &str,
    message: &str,
    judge_epoch: i32,
) {
    if let Err(e) =
        judging::fail_dispatch(db, Unit::CodeRun, code_run_id, judge_epoch, code, message).await
    {
        error!(
            code_run_id,
            error = %e,
            "Failed to mark code_run as SystemError; the row may be left stuck"
        );
    }
}

// The code-run model is skipped: its `files` hold the full source.
#[instrument(
    skip(state, code_run),
    fields(
        code_run_id = code_run.id,
        language = %code_run.language,
        files = %files_log_summary(&code_run.files),
        problem_id = code_run.problem_id,
        contest_id = tracing::field::Empty,
    )
)]
pub(crate) async fn dispatch_code_run_to_plugin(state: AppState, code_run: code_run::Model) {
    tracing::Span::current().record(
        "contest_id",
        tracing::field::display(
            code_run
                .contest_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
    );
    let handler = {
        let registry = state.registries.contest_type_registry.read().await;
        registry.get(&code_run.contest_type).cloned()
    };

    let handler = match handler {
        Some(h) => h,
        None => {
            warn!(
                code_run_id = code_run.id,
                contest_type = %code_run.contest_type,
                "No plugin registered for contest type"
            );
            mark_code_run_error_or_log(
                &state.db,
                code_run.id,
                "NO_HANDLER_REGISTERED",
                &format!(
                    "No plugin registered for contest type {:?}",
                    code_run.contest_type
                ),
                code_run.judge_epoch,
            )
            .await;
            return;
        }
    };

    let problem = match problem::Entity::find_by_id(code_run.problem_id)
        .one(&state.db)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => {
            error!(problem_id = code_run.problem_id, "Problem not found");
            mark_code_run_error_or_log(
                &state.db,
                code_run.id,
                "PROBLEM_NOT_FOUND",
                &format!("Problem {} not found", code_run.problem_id),
                code_run.judge_epoch,
            )
            .await;
            return;
        }
        Err(e) => {
            error!(error = %e, "DB error fetching problem");
            mark_code_run_error_or_log(
                &state.db,
                code_run.id,
                "DATABASE_ERROR",
                &format!("Failed to fetch problem: {}", e),
                code_run.judge_epoch,
            )
            .await;
            return;
        }
    };

    let files: Vec<SourceFile> = match serde_json::from_value(code_run.files.clone()) {
        Ok(f) => f,
        Err(e) => {
            error!(error = %e, "Failed to parse code run files");
            mark_code_run_error_or_log(
                &state.db,
                code_run.id,
                "INVALID_FILES",
                &format!("Failed to parse code run files: {}", e),
                code_run.judge_epoch,
            )
            .await;
            return;
        }
    };

    let custom_tcs: Vec<CustomTestCaseInput> =
        serde_json::from_value(code_run.custom_test_cases.clone()).unwrap_or_default();
    let resolved_test_cases: Vec<TestCaseRow> = custom_tcs
        .iter()
        .enumerate()
        .map(|(i, tc)| TestCaseRow {
            id: i as i32,
            score: 0.0,
            is_sample: false,
            position: i as i32,
            description: None,
            label: None,
            input: TestCaseBodyRef::inline(tc.input.clone()),
            expected_output: tc
                .expected_output
                .clone()
                .map(TestCaseBodyRef::inline)
                .unwrap_or_default(),
            is_custom: true,
        })
        .collect();

    let input = OnCodeRunInput {
        id: code_run.id,
        judge_epoch: code_run.judge_epoch,
        user_id: code_run.user_id,
        problem_id: code_run.problem_id,
        contest_id: code_run.contest_id,
        files,
        language: code_run.language.clone(),
        time_limit_ms: problem.time_limit,
        memory_limit_kb: problem.memory_limit,
        problem_type: problem.problem_type.clone(),
        test_cases: resolved_test_cases,
    };

    let input_bytes = match serde_json::to_vec(&input) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "Failed to serialize code run input");
            mark_code_run_error_or_log(
                &state.db,
                code_run.id,
                "SERIALIZATION_ERROR",
                &format!("Failed to serialize input: {}", e),
                code_run.judge_epoch,
            )
            .await;
            return;
        }
    };

    let plugin_id = handler.plugin_id.clone();
    let function_name = handler.code_run_fn.clone();
    let plugins = state.plugins.clone();
    let db = state.db.clone();
    let code_run_id = code_run.id;
    let judge_epoch = code_run.judge_epoch;

    info!(
        code_run_id,
        problem_id = code_run.problem_id,
        contest_id = ?code_run.contest_id,
        plugin_id = %plugin_id,
        function_name = %function_name,
        "Dispatching code run to plugin"
    );

    let problem_id = code_run.problem_id;
    let contest_id = code_run.contest_id;
    let span = tracing::info_span!(
        "plugin_code_run_call",
        code_run_id,
        problem_id,
        contest_id = ?contest_id,
        plugin_id = %plugin_id,
        function_name = %function_name,
    );
    tokio::spawn(async move {
        async move {
            // Retry on plugin-pool contention, like the graded-submission path:
            // PoolTimeout is transient backpressure and must not become a permanent
            // SystemError for a custom run at exactly the peak-load moment the
            // retry exists to absorb.
            let call_fut = call_raw_with_pool_retry(
                plugins.as_ref(),
                &plugin_id,
                &function_name,
                input_bytes,
                PoolRetryPolicy::default(),
            );
            let result = match tokio::time::timeout(CODE_RUN_DISPATCH_TIMEOUT, call_fut).await {
                Ok(r) => r,
                Err(_) => {
                    error!(
                        code_run_id,
                        problem_id,
                        contest_id = ?contest_id,
                        plugin_id = %plugin_id,
                        function_name = %function_name,
                        timeout_secs = CODE_RUN_DISPATCH_TIMEOUT.as_secs(),
                        "Code run dispatch timed out"
                    );
                    mark_code_run_error_or_log(
                        &db,
                        code_run_id,
                        "DISPATCH_TIMEOUT",
                        &format!(
                            "Code run dispatch exceeded {}s timeout",
                            CODE_RUN_DISPATCH_TIMEOUT.as_secs()
                        ),
                        judge_epoch,
                    )
                    .await;
                    return;
                }
            };

            match result {
                Ok(output_bytes) => match serde_json::from_slice::<OnCodeRunOutput>(&output_bytes)
                {
                    Ok(output) => {
                        if !output.success {
                            error!(
                                code_run_id,
                                problem_id,
                                contest_id = ?contest_id,
                                error = ?output.error_message,
                                "Plugin reported failure"
                            );
                            mark_code_run_error_or_log(
                                &db,
                                code_run_id,
                                "PLUGIN_ERROR",
                                &output
                                    .error_message
                                    .unwrap_or_else(|| "Unknown plugin error".to_string()),
                                judge_epoch,
                            )
                            .await;
                        } else {
                            info!(
                                code_run_id,
                                problem_id,
                                contest_id = ?contest_id,
                                "Plugin completed code run successfully"
                            );
                        }
                    }
                    Err(e) => {
                        error!(code_run_id, problem_id, contest_id = ?contest_id, error = %e, "Failed to parse plugin output");
                        mark_code_run_error_or_log(
                            &db,
                            code_run_id,
                            "PLUGIN_INVALID_OUTPUT",
                            &format!("Plugin returned invalid output: {}", e),
                            judge_epoch,
                        )
                        .await;
                    }
                },
                Err(plugin_core::error::PluginError::ShuttingDown) => {
                    // This server is stopping: leave the code run in flight for
                    // a peer (see the matching arm in submission dispatch).
                    warn!(code_run_id, "Server shutting down; leaving code run for a peer");
                }
                Err(e) => {
                    error!(code_run_id, problem_id, contest_id = ?contest_id, error = %e, "Plugin execution failed");
                    mark_code_run_error_or_log(
                        &db,
                        code_run_id,
                        "PLUGIN_EXECUTION_ERROR",
                        &e.to_string(),
                        judge_epoch,
                    )
                    .await;
                }
            }
        }
        .instrument(span)
        .await;
    });
}
