use crate::registry::ContestTypeRegistry;
use broccoli_server_sdk::types::{
    AfterJudgingEvent, DetachedSubmissionCompletion, OnSubmissionInput, OnSubmissionOutput,
    SourceFile, TestCaseBodyRef, TestCaseRow,
};
use common::SubmissionDlqErrorCode;
use sea_orm::prelude::Expr;
use sea_orm::*;
use tracing::{Instrument, error, info, instrument, warn};

use crate::entity::{problem, submission, submission_judgement, test_case};
use crate::hooks;
use crate::services::judging::{self, ClaimedJudgement, Unit};
use crate::state::AppState;
use crate::utils::judging::files_log_summary;
use crate::utils::test_case_body::{
    AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES, select_inline_within_budget,
};

#[derive(FromQueryResult)]
struct SubmissionDispatchTestCaseRow {
    id: i32,
    score: i32,
    is_sample: bool,
    position: i32,
    description: Option<String>,
    label: String,
    input: String,
    expected_output: String,
    input_blob_hash: Option<String>,
    expected_output_blob_hash: Option<String>,
}

pub(crate) async fn fire_after_judging_hooks(
    db: &DatabaseConnection,
    hook_registry: hooks::SharedHookRegistry,
    contest_types: &ContestTypeRegistry,
    submission_id: i32,
    user_id: i32,
    problem_id: i32,
    contest_id: Option<i32>,
) {
    let sub = match submission::Entity::find_by_id(submission_id).one(db).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            warn!(submission_id, "Submission not found for after_judging hook");
            return;
        }
        Err(e) => {
            warn!(submission_id, error = %e, "DB error reading submission for after_judging hook");
            return;
        }
    };

    if !sub.status.is_terminal() {
        return;
    }

    dispatch_after_judging_hooks(
        db,
        hook_registry,
        contest_types,
        &sub,
        user_id,
        problem_id,
        contest_id,
    )
    .await;
}

pub(crate) async fn fire_after_judging_hooks_for_detached_completion(
    db: &DatabaseConnection,
    hook_registry: hooks::SharedHookRegistry,
    contest_types: &ContestTypeRegistry,
    completion: &DetachedSubmissionCompletion,
) {
    if !completion.fire_after_judging {
        return;
    }

    let sub = match submission::Entity::find_by_id(completion.submission_id)
        .one(db)
        .await
    {
        Ok(Some(s)) => s,
        Ok(None) => {
            warn!(
                submission_id = completion.submission_id,
                "Submission not found for after_judging hook"
            );
            return;
        }
        Err(e) => {
            warn!(submission_id = completion.submission_id, error = %e, "DB error reading submission for after_judging hook");
            return;
        }
    };

    if sub.judge_epoch != completion.judge_epoch || !sub.status.is_terminal() {
        return;
    }

    let current_judgement = submission_judgement::Entity::find_by_id(completion.judgement_id)
        .filter(submission_judgement::Column::SubmissionId.eq(completion.submission_id))
        .filter(submission_judgement::Column::JudgeEpoch.eq(completion.judge_epoch))
        .filter(submission_judgement::Column::IsCurrent.eq(true))
        .filter(submission_judgement::Column::IsFinalized.eq(true))
        .one(db)
        .await;
    match current_judgement {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(e) => {
            warn!(
                submission_id = completion.submission_id,
                judgement_id = completion.judgement_id,
                judge_epoch = completion.judge_epoch,
                error = %e,
                "DB error validating detached after_judging completion"
            );
            return;
        }
    }

    dispatch_after_judging_hooks(
        db,
        hook_registry,
        contest_types,
        &sub,
        sub.user_id,
        sub.problem_id,
        sub.contest_id,
    )
    .await;
}

async fn dispatch_after_judging_hooks(
    db: &DatabaseConnection,
    hook_registry: hooks::SharedHookRegistry,
    contest_types: &ContestTypeRegistry,
    sub: &submission::Model,
    user_id: i32,
    problem_id: i32,
    contest_id: Option<i32>,
) {
    let verdict = sub
        .verdict
        .as_ref()
        .map(|v| v.to_string())
        .unwrap_or_else(|| sub.status.to_string());

    let enabled_plugins =
        match hooks::resource_enablements(db, contest_types, problem_id, contest_id).await {
            Ok(e) => Some(e),
            Err(e) => {
                warn!(error = ?e, "Failed to fetch enablements for after_judging hook");
                None
            }
        };

    hooks::dispatch_hooks_background_typed(
        AfterJudgingEvent {
            submission_id: sub.id,
            user_id,
            problem_id,
            contest_id,
            verdict,
            score: sub.score,
        },
        enabled_plugins,
        hook_registry,
        Some(format!("after_judging:{}:{}", sub.id, sub.judge_epoch)),
    );
}

async fn record_dispatch_failure(
    db: &DatabaseConnection,
    metrics: &common::metrics::Metrics,
    judgement: JudgementRef,
    error_code: &'static str,
    error_message: &str,
) {
    let recovered = match judging::fail_dispatch(
        db,
        Unit::Judgement,
        judgement.id,
        judgement.judge_epoch,
        error_code,
        error_message,
    )
    .await
    {
        Ok(_) => true,
        Err(e) => {
            error!(
                submission_id = judgement.submission_id,
                judgement_id = judgement.id,
                error_code,
                marking_error = %e,
                "Failed to persist the dispatch failure; the judgement stays in flight until a detector requeues it"
            );
            false
        }
    };
    metrics.submission_dispatch_failure_total.add(
        1,
        &[
            opentelemetry::KeyValue::new("error_code", error_code),
            opentelemetry::KeyValue::new("recovered", recovered),
        ],
    );
}

/// The dispatch a failure is recorded against.
#[derive(Clone, Copy)]
struct JudgementRef {
    submission_id: i32,
    id: i32,
    judge_epoch: i32,
}

// The submission model is skipped: its `files` hold the contestant's full
// source. Ids, language and file names/sizes only.
#[instrument(
    skip(state, claimed),
    fields(
        submission_id = claimed.submission.id,
        judgement_id = claimed.judgement.id,
        language = %claimed.submission.language,
        files = %files_log_summary(&claimed.submission.files),
        problem_id = claimed.submission.problem_id,
        contest_id = tracing::field::Empty,
    )
)]
pub(crate) async fn dispatch_judgement(state: AppState, claimed: ClaimedJudgement) {
    let ClaimedJudgement {
        submission,
        judgement,
    } = claimed;
    // Hooks fire only for the judgement whose verdict is the submission's.
    let fire_after_judging = judgement.is_current;
    let judgement_ref = JudgementRef {
        submission_id: submission.id,
        id: judgement.id,
        judge_epoch: judgement.judge_epoch,
    };
    let judgement_id = judgement.id;
    tracing::Span::current().record(
        "contest_id",
        tracing::field::display(
            submission
                .contest_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
        ),
    );

    let contest_type = Some(submission.contest_type.clone());

    let handler = {
        let registry = state.registries.contest_type_registry.read().await;
        contest_type.as_ref().and_then(|t| registry.get(t)).cloned()
    };

    let handler = match handler {
        Some(h) => h,
        None => {
            warn!(
                submission_id = submission.id,
                contest_type = ?contest_type,
                "No plugin registered for contest type"
            );
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                judgement_ref,
                "NO_HANDLER_REGISTERED",
                &format!("No plugin registered for contest type {:?}", contest_type),
            )
            .await;
            return;
        }
    };

    let problem = match problem::Entity::find_by_id(submission.problem_id)
        .one(&state.db)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => {
            error!(problem_id = submission.problem_id, "Problem not found");
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                judgement_ref,
                "PROBLEM_NOT_FOUND",
                &format!("Problem {} not found", submission.problem_id),
            )
            .await;
            return;
        }
        Err(e) => {
            error!(error = %e, "DB error fetching problem");
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                judgement_ref,
                "DATABASE_ERROR",
                &format!("Failed to fetch problem: {}", e),
            )
            .await;
            return;
        }
    };

    let files: Vec<SourceFile> = match serde_json::from_value(submission.files.clone()) {
        Ok(f) => f,
        Err(e) => {
            error!(error = %e, "Failed to parse submission files");
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                judgement_ref,
                "INVALID_FILES",
                &format!("Failed to parse submission files: {}", e),
            )
            .await;
            return;
        }
    };

    let resolved_test_cases = {
        let db_tcs = match test_case::Entity::find()
            .filter(test_case::Column::ProblemId.eq(submission.problem_id))
            .select_only()
            .column(test_case::Column::Id)
            .column(test_case::Column::Score)
            .column(test_case::Column::IsSample)
            .column(test_case::Column::Position)
            .column(test_case::Column::Description)
            .column(test_case::Column::Label)
            .column(test_case::Column::InputBlobHash)
            .column(test_case::Column::ExpectedOutputBlobHash)
            .column_as(
                Expr::cust("CASE WHEN \"input_blob_hash\" IS NULL THEN \"input\" ELSE '' END"),
                "input",
            )
            .column_as(
                Expr::cust(
                    "CASE WHEN \"expected_output_blob_hash\" IS NULL THEN \"expected_output\" ELSE '' END",
                ),
                "expected_output",
            )
            .order_by_asc(test_case::Column::Position)
            .into_model::<SubmissionDispatchTestCaseRow>()
            .all(&state.db)
            .await
        {
            Ok(tcs) => tcs,
            Err(e) => {
                error!(error = %e, "Failed to query test cases");
                record_dispatch_failure(
                    &state.db,
                    &state.metrics,
                    judgement_ref,
                    "DATABASE_ERROR",
                    &format!("Failed to query test cases: {}", e),
                )
                .await;
                return;
            }
        };
        // Bound the TOTAL inline bytes (input + expected_output, across every
        // test case) assembled into this submission's plugin payload, not
        // just each body individually. `INLINE_TEST_CASE_BODY_THRESHOLD_BYTES`
        // (checked at test-case create time) only bounds one body at a time;
        // fifty legally-under-threshold bodies can still sum to tens of MiB,
        // which is the measured cause of a guest OOM (see
        // AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES's doc comment). Bodies that
        // are already blob-backed are untouched -- they never occupy the
        // inline budget in the first place.
        //
        // The split is derived from current storage on every dispatch, so a
        // problem stored over budget is judged within budget. Candidates are walked in a fixed order (test-case
        // `position`, already the query's ORDER BY, then input before
        // expected_output within a case), so the split is deterministic and
        // stable across rejudges of the same test-case set.
        let candidate_sizes: Vec<usize> = db_tcs
            .iter()
            .flat_map(|tc| {
                [
                    tc.input_blob_hash.is_none().then_some(tc.input.len()),
                    tc.expected_output_blob_hash
                        .is_none()
                        .then_some(tc.expected_output.len()),
                ]
            })
            .flatten()
            .collect();
        let mut keep_flags =
            select_inline_within_budget(&candidate_sizes, AGGREGATE_INLINE_TEST_CASE_BUDGET_BYTES)
                .into_iter();

        let mut resolved_test_cases = Vec::with_capacity(db_tcs.len());
        for tc in db_tcs {
            let SubmissionDispatchTestCaseRow {
                id,
                score,
                is_sample,
                position,
                description,
                label,
                input,
                expected_output,
                input_blob_hash,
                expected_output_blob_hash,
            } = tc;

            let input_ref = match resolve_body_ref(&state, input_blob_hash, input, &mut keep_flags)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!(error = %e, "Failed to spill over-budget inline test case input to blob store");
                    record_dispatch_failure(
                        &state.db,
                        &state.metrics,
                        judgement_ref,
                        "BLOB_STORE_ERROR",
                        &format!("Failed to store oversized inline test case input: {e}"),
                    )
                    .await;
                    return;
                }
            };
            let expected_output_ref = match resolve_body_ref(
                &state,
                expected_output_blob_hash,
                expected_output,
                &mut keep_flags,
            )
            .await
            {
                Ok(r) => r,
                Err(e) => {
                    error!(error = %e, "Failed to spill over-budget inline test case expected_output to blob store");
                    record_dispatch_failure(
                        &state.db,
                        &state.metrics,
                        judgement_ref,
                        "BLOB_STORE_ERROR",
                        &format!("Failed to store oversized inline test case expected_output: {e}"),
                    )
                    .await;
                    return;
                }
            };

            resolved_test_cases.push(TestCaseRow {
                id,
                score: score as f64,
                is_sample,
                position,
                description,
                label: Some(label),
                input: input_ref,
                expected_output: expected_output_ref,
                is_custom: false,
            });
        }
        resolved_test_cases
    };

    let input = OnSubmissionInput {
        submission_id: submission.id,
        judgement_id,
        fire_after_judging,
        user_id: submission.user_id,
        problem_id: submission.problem_id,
        contest_id: submission.contest_id,
        files,
        language: submission.language.clone(),
        time_limit_ms: problem.time_limit,
        memory_limit_kb: problem.memory_limit,
        problem_type: problem.problem_type.clone(),
        test_cases: resolved_test_cases,
        judge_epoch: judgement.judge_epoch,
        target_worker_id: judgement.target_worker_id.clone(),
    };

    let input_bytes = match serde_json::to_vec(&input) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "Failed to serialize plugin input");
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                judgement_ref,
                "SERIALIZATION_ERROR",
                &format!("Failed to serialize input: {}", e),
            )
            .await;
            return;
        }
    };

    let plugin_id = handler.plugin_id.clone();
    let function_name = handler.submission_fn.clone();
    let plugins = state.plugins.clone();
    let hook_registry = state.registries.hook_registry.clone();
    let contest_types = state.registries.contest_type_registry.clone();
    let db = state.db.clone();
    let metrics = state.metrics.clone();
    let submission_id = submission.id;
    let user_id = submission.user_id;
    let problem_id = submission.problem_id;
    let contest_id = submission.contest_id;

    info!(
        submission_id,
        judgement_id,
        problem_id,
        contest_id = ?contest_id,
        plugin_id = %plugin_id,
        function_name = %function_name,
        "Dispatching submission to plugin"
    );

    let span = tracing::info_span!(
        "plugin_submission_call",
        submission_id,
        judgement_id,
        problem_id,
        contest_id = ?contest_id,
        plugin_id = %plugin_id,
        function_name = %function_name,
    );
    tokio::spawn(async move {
        async move {
            // The contest call waits in host functions for the whole
            // judgement, holding no CPU token while it does.
            let result = plugins
                .call_raw(
                    &plugin_id,
                    &function_name,
                    input_bytes,
                    plugins.get_config().background_call(),
                )
                .await;

            match result {
                Ok(output_bytes) => {
                    match serde_json::from_slice::<OnSubmissionOutput>(&output_bytes) {
                        Ok(output) => {
                            if !output.success {
                                error!(
                                    submission_id,
                                    judgement_id,
                                    problem_id,
                                    contest_id = ?contest_id,
                                    error = ?output.error_message,
                                    "Plugin reported failure"
                                );
                                // A refusal is deterministic: record it under
                                // its own code, which the SystemError-retry
                                // reaper leaves alone.
                                let error_code = if output.retryable {
                                    "PLUGIN_ERROR"
                                } else {
                                    SubmissionDlqErrorCode::PLUGIN_REFUSED
                                };
                                record_dispatch_failure(
                                    &db,
                                    &metrics,
                                    judgement_ref,
                                    error_code,
                                    &output
                                        .error_message
                                        .unwrap_or_else(|| "Unknown plugin error".to_string()),
                                )
                                .await;
                            } else {
                                info!(
                                    submission_id,
                                    judgement_id,
                                    problem_id,
                                    contest_id = ?contest_id,
                                    "Plugin completed successfully"
                                );
                                // NOTE: a plugin may "complete successfully" yet
                                // persist a SystemError verdict (a system-side
                                // condition, never the contestant's code). The
                                // bounded re-judge of such verdicts is handled
                                // uniformly - across BOTH synchronous (batch) and
                                // detached (interactive) finalization - by the
                                // SystemError-retry reaper fiber
                                // (`dispatcher::system_error_retry`), which keys
                                // off the persisted terminal state rather than
                                // this control-flow site.
                            }
                        }
                        Err(e) => {
                            error!(submission_id, judgement_id, problem_id, contest_id = ?contest_id, error = %e, "Failed to parse plugin output");
                            record_dispatch_failure(
                                &db,
                                &metrics,
                                judgement_ref,
                                "PLUGIN_INVALID_OUTPUT",
                                &format!("Plugin returned invalid output: {}", e),
                            )
                            .await;
                        }
                    }
                }
                Err(plugin_core::error::PluginError::ShuttingDown) => {
                    // Not a judging failure: this server is stopping. Leave the
                    // submission in flight (no verdict, no hooks, no retry
                    // spent); shutdown releases its lease and a peer
                    // re-dispatches it.
                    warn!(submission_id, judgement_id, "Server shutting down; leaving submission for a peer");
                    return;
                }
                Err(e) => {
                    error!(submission_id, judgement_id, problem_id, contest_id = ?contest_id, error = %e, "Plugin execution failed");
                    record_dispatch_failure(
                        &db,
                        &metrics,
                        judgement_ref,
                        "PLUGIN_EXECUTION_ERROR",
                        &e.to_string(),
                    )
                    .await;
                }
            }

            if fire_after_judging {
                fire_after_judging_hooks(
                    &db,
                    hook_registry,
                    &contest_types,
                    submission_id,
                    user_id,
                    problem_id,
                    contest_id,
                )
                .await;
            }
        }
        .instrument(span)
        .await;
    });
}

/// Resolve one test-case body (input or expected_output) to its dispatch
/// reference.
///
/// An already blob-backed body passes through unchanged -- it never
/// consumed a `keep_flags` entry (see `candidate_sizes` at the call site) and
/// is untouched by the aggregate inline budget. Otherwise this consumes the
/// next aggregate-budget decision -- callers must visit candidates in
/// exactly the order `candidate_sizes` was built in, so each body sees the
/// flag [`select_inline_within_budget`] computed for it -- and either keeps
/// the text inline or spills it to the blob store.
///
/// The blob store is content-addressed (`put` hashes the bytes and is a
/// no-op if that hash is already stored), so spilling a body that turns out
/// to already exist as a blob elsewhere, or re-spilling the same body on a
/// rejudge, is idempotent and cheap.
async fn resolve_body_ref(
    state: &AppState,
    blob_hash: Option<String>,
    inline_text: String,
    keep_flags: &mut impl Iterator<Item = bool>,
) -> Result<TestCaseBodyRef, common::storage::StorageError> {
    if let Some(hash) = blob_hash {
        return Ok(TestCaseBodyRef::blob(hash));
    }

    // `candidate_sizes` produces exactly one flag per inline candidate, in
    // the same order candidates are resolved here, so this always has a
    // matching entry. A missing flag keeps the body inline rather than
    // taking down submission dispatch.
    let keep = keep_flags.next().unwrap_or(true);
    if keep {
        return Ok(TestCaseBodyRef::inline(inline_text));
    }

    let hash = state.blob_store.put(inline_text.as_bytes()).await?;
    Ok(TestCaseBodyRef::blob(hash.to_hex()))
}
