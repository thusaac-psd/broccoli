use broccoli_server_sdk::types::{
    OnCodeRunInput, OnCodeRunOutput, SourceFile, TestCaseBodyRef, TestCaseRow,
};
use sea_orm::EntityTrait;
use tracing::{Instrument, error, info, instrument, warn};

use crate::entity::{code_run, problem};
use crate::models::code_run::CustomTestCaseInput;
use crate::services::judging::{self, CallEnd, DispatchFailure, DispatchFaults, Unit};
use crate::state::AppState;
use crate::utils::judging::files_log_summary;

/// Ends a code run whose dispatch failed with `failure`, or, when its
/// operations met a platform fault, with that fault (see
/// `judging::fail_dispatch`), fenced on the dispatch's epoch, logging a
/// failure of the write itself: the run then stays in flight until a
/// detector requeues it.
async fn record_dispatch_failure(
    db: &common::db::DatabaseConnection,
    code_run_id: i32,
    judge_epoch: i32,
    faults: &DispatchFaults,
    failure: DispatchFailure,
) {
    let failure = failure.unless_fault(faults);
    if let Err(e) =
        judging::fail_dispatch(db, Unit::CodeRun, code_run_id, judge_epoch, &failure).await
    {
        error!(
            code_run_id,
            error_code = failure.code,
            error = %e,
            "Failed to record the code run's dispatch failure; the row may be left stuck"
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
    // What the dispatch's operations meet, wherever they run: its plugin
    // call starts with these faults in its call locals.
    let faults = DispatchFaults::default();
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
            record_dispatch_failure(
                &state.db,
                code_run.id,
                code_run.judge_epoch,
                &faults,
                DispatchFailure::judging(
                    "NO_HANDLER_REGISTERED",
                    format!(
                        "No plugin registered for contest type {:?}",
                        code_run.contest_type
                    ),
                ),
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
            record_dispatch_failure(
                &state.db,
                code_run.id,
                code_run.judge_epoch,
                &faults,
                DispatchFailure::judging(
                    "PROBLEM_NOT_FOUND",
                    format!("Problem {} not found", code_run.problem_id),
                ),
            )
            .await;
            return;
        }
        Err(e) => {
            error!(error = %e, "DB error fetching problem");
            record_dispatch_failure(
                &state.db,
                code_run.id,
                code_run.judge_epoch,
                &faults,
                DispatchFailure::judging(
                    "DATABASE_ERROR",
                    format!("Failed to fetch problem: {}", e),
                ),
            )
            .await;
            return;
        }
    };

    let files: Vec<SourceFile> = match serde_json::from_value(code_run.files.clone()) {
        Ok(f) => f,
        Err(e) => {
            error!(error = %e, "Failed to parse code run files");
            record_dispatch_failure(
                &state.db,
                code_run.id,
                code_run.judge_epoch,
                &faults,
                DispatchFailure::judging(
                    "INVALID_FILES",
                    format!("Failed to parse code run files: {}", e),
                ),
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
            record_dispatch_failure(
                &state.db,
                code_run.id,
                code_run.judge_epoch,
                &faults,
                DispatchFailure::judging(
                    "SERIALIZATION_ERROR",
                    format!("Failed to serialize input: {}", e),
                ),
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
            // The run goes to its contest type's plugin, which alone may
            // write it; the dispatch records into this run's faults, which
            // decide how the call ends.
            let dispatch = crate::services::judging::Dispatch::new(
                &plugin_id,
                crate::services::judging::DispatchedUnit::CodeRun {
                    code_run_id,
                    judge_epoch,
                },
                faults.clone(),
            );
            let options = plugins
                .get_config()
                .judging_call()
                .with_locals(dispatch.call_locals());
            let result = plugins
                .call_raw(&plugin_id, &function_name, input_bytes, options)
                .await;
            match CallEnd::of::<OnCodeRunOutput>(result, &faults) {
                CallEnd::Handled => info!(
                    code_run_id,
                    problem_id,
                    contest_id = ?contest_id,
                    "Plugin completed code run successfully"
                ),
                CallEnd::Failed(failure) => {
                    error!(
                        code_run_id,
                        problem_id,
                        contest_id = ?contest_id,
                        error_code = failure.code,
                        error = %failure.message,
                        "Code run plugin call ended without a verdict"
                    );
                    record_dispatch_failure(&db, code_run_id, judge_epoch, &faults, failure).await;
                }
                // This server is stopping: leave the code run in flight for
                // a peer (see the matching arm in submission dispatch).
                CallEnd::ShuttingDown => {
                    warn!(
                        code_run_id,
                        "Server shutting down; leaving code run for a peer"
                    );
                }
            }
        }
        .instrument(span)
        .await;
    });
}
