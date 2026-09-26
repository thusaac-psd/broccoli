use broccoli_server_sdk::types::{
    AfterJudgingEvent, DetachedSubmissionCompletion, OnSubmissionInput, OnSubmissionOutput,
    SourceFile, TestCaseBodyRef, TestCaseRow, sanitize_text_field,
};
use chrono::Utc;
use common::{SubmissionStatus, Verdict};
use plugin_core::retry::{PoolRetryPolicy, call_raw_with_pool_retry};
use sea_orm::prelude::Expr;
use sea_orm::*;
use tracing::{Instrument, error, info, instrument, warn};

use crate::entity::{
    judgement_reset::ClearJudgementColumns, problem, submission, submission_judgement, test_case,
    test_case_result,
};
use crate::hooks;
use crate::state::AppState;
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

    dispatch_after_judging_hooks(db, hook_registry, &sub, user_id, problem_id, contest_id).await;
}

pub(crate) async fn fire_after_judging_hooks_for_detached_completion(
    db: &DatabaseConnection,
    hook_registry: hooks::SharedHookRegistry,
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

    let enabled_plugins = match hooks::fetch_resource_enablements(problem_id, contest_id, db).await
    {
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

pub(crate) async fn ensure_active_judgement_id(
    db: &DatabaseConnection,
    sub: &submission::Model,
) -> i32 {
    let existing = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
        .filter(submission_judgement::Column::IsCurrent.eq(true))
        .filter(submission_judgement::Column::IsFinalized.eq(false))
        .one(db)
        .await;
    match existing {
        Ok(Some(j)) => {
            // Adopt the existing current judgement under THIS dispatch's owner +
            // a fresh lease, exactly as the creation branch below stamps a new
            // one. A judgement created unowned elsewhere - notably the
            // apply-immediately rejudge path (`open_rejudge_judgement`), which
            // inserts it with owner_server_id=NULL - would otherwise stay unowned
            // while being judged, and the deferred-judgement steal
            // (`owner_server_id IS NULL AND created_at < threshold`) would reclaim
            // it ~lease_ttl later and double-dispatch it onto a second worker,
            // aborting the in-progress (possibly interactive) judge. Only write
            // when the owner/lease actually needs updating, to avoid a redundant
            // UPDATE on the common already-owned dispatch.
            if j.owner_server_id != sub.owner_server_id || j.lease_heartbeat_at.is_none() {
                let mut am: submission_judgement::ActiveModel = j.clone().into();
                am.owner_server_id = Set(sub.owner_server_id.clone());
                am.lease_heartbeat_at = Set(Some(Utc::now()));
                am.leased_at = Set(Some(Utc::now()));
                if let Err(e) = am.update(db).await {
                    warn!(
                        error = %e,
                        submission_id = sub.id,
                        judgement_id = j.id,
                        "Failed to stamp adopted judgement owner/lease; deferred steal may double-dispatch"
                    );
                }
            }
            return j.id;
        }
        Ok(None) => {}
        Err(e) => {
            warn!(error = %e, submission_id = sub.id, "Judgement lookup failed, dispatching with id=0");
            return 0;
        }
    }

    let next_version: i32 = match submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
        .order_by_desc(submission_judgement::Column::Version)
        .one(db)
        .await
    {
        Ok(Some(j)) => j.version.saturating_add(1),
        _ => 1,
    };
    let active = submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(next_version),
        is_current: Set(true),
        is_finalized: Set(false),
        triggered_by_user_id: Set(None),
        target_worker_id: Set(sub.target_worker_id.clone()),
        note: Set(None),
        status: Set(sub.status.clone()),
        verdict: Set(sub.verdict.clone()),
        score: Set(sub.score),
        time_used: Set(sub.time_used),
        memory_used: Set(sub.memory_used),
        compile_output: Set(sub.compile_output.clone()),
        error_code: Set(sub.error_code.clone()),
        error_message: Set(sub.error_message.clone()),
        judge_epoch: Set(sub.judge_epoch),
        // Inherit the submission's owner and stamp a fresh lease heartbeat so the
        // lease-refresh fiber (`dispatcher::lease`) keeps this judgement alive
        // while it evaluates. Without an owner, the deferred-judgement steal's
        // `owner_server_id IS NULL AND created_at < threshold` branch reclaims it
        // ~lease_ttl after the submission was created - churning long evaluates
        // (and, before the lockstep-epoch fix, hanging the submission).
        owner_server_id: Set(sub.owner_server_id.clone()),
        lease_heartbeat_at: Set(Some(Utc::now())),
        // `created_at` intentionally mirrors the submission's original creation
        // time, but `leased_at` is the *dispatch* anchor and must be NOW - it
        // marks when this judgement started its in-flight evaluation for the
        // stuck-detector's in-flight cap.
        leased_at: Set(Some(Utc::now())),
        created_at: Set(sub.created_at),
        finalized_at: Set(None),
        ..Default::default()
    };
    match active.insert(db).await {
        Ok(j) => j.id,
        Err(e) => {
            warn!(error = %e, submission_id = sub.id, "Judgement insert failed, dispatching with id=0");
            0
        }
    }
}

async fn mark_submission_dispatch_system_error(
    db: &DatabaseConnection,
    submission_id: i32,
    judgement_id: i32,
    error_code: &str,
    error_message: &str,
    judge_epoch: i32,
) -> anyhow::Result<()> {
    // Scrub embedded NULs before this direct-by-id write. Judged error text
    // can carry an embedded `\0` (e.g. a plugin forwarding raw worker/compiler
    // stderr); Postgres rejects a NUL in a text column, so an unsanitized
    // update would return `Err` here and short-circuit before the resilient,
    // sanitized `mark_submission_system_error_with_epoch` below ever runs -
    // leaving the submission stuck non-terminal in a re-dispatch loop. Mirror
    // the sanitization the epoch-guarded path already applies.
    let safe_code = sanitize_text_field(error_code);
    let safe_message = sanitize_text_field(error_message);
    if judgement_id > 0 {
        // Epoch-gate this by-id finalize exactly like the sibling submission
        // write below. A dispatch task captures `submission.judge_epoch` at
        // spawn time and carries it, unre-read, into its detached future; if
        // the age-cap stuck-detector (`max_inflight_secs`) recovers this job's
        // judgement mid-flight, it re-dispatches the SAME judgement id at
        // `judge_epoch + 1`. Without the epoch guard, this task's later failure
        // would clobber that live, newer-epoch judgement by id alone — flipping
        // a correctly-judged (or still-in-flight) result to a finalized
        // SystemError. The `is_finalized = FALSE` guard mirrors the sibling and
        // avoids re-finalizing an already-terminal row. Unlike
        // `mark_submission_system_error_with_epoch`'s judgement write, this keys
        // on the judgement id (not submission_id + is_current) so a NON-current
        // deferred-rejudge judgement this task owns is still covered.
        submission_judgement::Entity::update_many()
            .col_expr(
                submission_judgement::Column::Status,
                Expr::value(SubmissionStatus::SystemError.to_string()),
            )
            .col_expr(
                submission_judgement::Column::ErrorCode,
                Expr::value(Some(safe_code.as_ref().to_string())),
            )
            .col_expr(
                submission_judgement::Column::ErrorMessage,
                Expr::value(Some(safe_message.as_ref().to_string())),
            )
            .col_expr(submission_judgement::Column::IsFinalized, Expr::value(true))
            .col_expr(
                submission_judgement::Column::FinalizedAt,
                Expr::value(Some(Utc::now())),
            )
            .filter(submission_judgement::Column::Id.eq(judgement_id))
            .filter(submission_judgement::Column::JudgeEpoch.eq(judge_epoch))
            .filter(submission_judgement::Column::IsFinalized.eq(false))
            .exec(db)
            .await?;
    }

    crate::consumers::mark_submission_system_error_with_epoch(
        db,
        submission_id,
        error_code,
        error_message,
        Some(judge_epoch),
    )
    .await
}

/// When a plugin finalizes a judgement with a `SystemError` verdict, that
/// reflects a SYSTEM-side condition (e.g. the interactive manager failed to
/// compile under contention, or an operation result was lost) - never the
/// contestant's own code. The contest invariant forbids such a condition from
/// standing as the contestant's verdict. The host owns judging lifecycle, so it
/// treats a finalized `SystemError` as RETRYABLE: reset the judgement + its
/// parent submission to a fresh epoch (in lockstep) and hand the caller the
/// updated submission to re-dispatch, bounded by `max_system_error_retries`.
/// Only when that budget is exhausted does the `SystemError` stand (a genuinely
/// broken problem, e.g. a manager that never compiles).
///
/// The gate covers every shape a SYSTEM condition terminalizes into, and only
/// those:
/// * plugin-finalized - `status == Judged AND verdict == SystemError` (manager
///   compile failure, lost operation result, or a transient per-testcase
///   SystemError that aggregates to SystemError because it outranks every
///   contestant-code verdict — though a plugin custom `Other(_)` verdict or a
///   `CompileError` in the same result set ranks higher still and would stand
///   instead; see `broccoli_types::types::Verdict::severity`);
/// * stuck-handler terminal / dispatch-exhaustion - `status == SystemError`
///   (`record_dispatch_failure`, `mark_submission_dispatch_system_error`, and
///   the stuck detector all land here with a NULL verdict). Reusing only the
///   first shape abandoned these, leaving a system fault as the verdict.
///
/// A contestant `CompileError` (`status == CompilationError`) and any own-code
/// verdict (`status == Judged` with WA/TLE/MLE/RE) are left strictly alone.
///
/// Re-dispatching the returned submission via `dispatch_to_plugin_with_judgement`
/// with `fire_after_judging = true` keeps after-judging hooks firing when the
/// re-judge finally completes.
///
/// Returns `Some(submission)` (reset to `Pending` at the bumped epoch, owned by
/// this server) when the judgement was requeued and the caller should
/// re-dispatch it; `None` when it must NOT be retried - not a finalized
/// SystemError, superseded (epoch advanced / no longer current), or the retry
/// budget is exhausted.
pub(crate) async fn requeue_judgement_for_system_error_retry(
    db: &DatabaseConnection,
    submission_id: i32,
    judgement_id: i32,
    expected_epoch: i32,
    server_id: &str,
    max_system_error_retries: u32,
) -> anyhow::Result<Option<submission::Model>> {
    if judgement_id <= 0 {
        return Ok(None);
    }

    let txn = db.begin().await?;

    let Some(judgement) = submission_judgement::Entity::find_by_id(judgement_id)
        .one(&txn)
        .await?
    else {
        txn.rollback().await.ok();
        return Ok(None);
    };

    // Gate (see the doc comment): a CURRENT, FINALIZED judgement at the epoch we
    // dispatched, whose terminal outcome is a SYSTEM condition in ANY of its
    // shapes, is retryable:
    //   * plugin-finalized - `status == Judged` with `verdict == SystemError`;
    //   * stuck-handler terminal / dispatch-exhaustion - `status == SystemError`
    //     (verdict typically NULL, `error_code` STUCK_JOB or a dispatch code).
    // A contestant `CompileError` (`status == CompilationError`) or any own-code
    // verdict (`status == Judged` with WA/TLE/MLE/RE) is left strictly alone - the
    // contestant's verdict stands.
    let is_finalized_system_error = judgement.is_current
        && judgement.is_finalized
        && judgement.judge_epoch == expected_epoch
        && ((judgement.status == SubmissionStatus::Judged
            && judgement.verdict == Some(Verdict::SystemError))
            || judgement.status == SubmissionStatus::SystemError);
    if !is_finalized_system_error {
        txn.rollback().await.ok();
        return Ok(None);
    }

    // Bounded retry against the dedicated SystemError-retry budget (`retry + 1 >
    // max`). This budget is deliberately larger than the dispatch/stuck cap: the
    // stuck-handler and dispatch-exhaustion paths spend the dispatch/stuck budget
    // (5) before terminalizing to `status == SystemError`, so reusing that cap
    // here would make those shapes structurally un-retryable. A high cap honors
    // "no system condition becomes the contestant's verdict" while remaining a
    // finite runaway backstop; it is rarely approached because the re-judge
    // succeeds once the underlying contention clears.
    let max = std::cmp::min(max_system_error_retries, i32::MAX as u32) as i32;
    if judgement.retry_count.saturating_add(1) > max {
        txn.rollback().await.ok();
        return Ok(None);
    }

    let new_epoch = expected_epoch.saturating_add(1);
    let new_retry = judgement.retry_count.saturating_add(1);
    let now = Utc::now();

    // Reset the judgement to a fresh, re-dispatchable epoch, owned by this server
    // with a live lease (so the lease fiber keeps it alive and the steal leaves
    // it alone while it re-judges). Epoch-gated so a concurrent steal/retry that
    // already advanced this judgement wins and we abandon (rows == 0).
    let judgement_rows = submission_judgement::Entity::update_many()
        .col_expr(
            submission_judgement::Column::JudgeEpoch,
            Expr::value(new_epoch),
        )
        .col_expr(
            submission_judgement::Column::Status,
            Expr::value(SubmissionStatus::Pending.to_string()),
        )
        // clear_judgement_columns() NULLs every judged-output column,
        // including CompileOutput — which the hand-rolled list this replaced
        // had silently omitted (dormant today only because finalize nulls
        // compile_output for non-CE verdicts; see entity/judgement_reset.rs).
        .clear_judgement_columns()
        .col_expr(
            submission_judgement::Column::IsFinalized,
            Expr::value(false),
        )
        .col_expr(
            submission_judgement::Column::RetryCount,
            Expr::value(new_retry),
        )
        .col_expr(
            submission_judgement::Column::OwnerServerId,
            Expr::value(Some(server_id.to_string())),
        )
        .col_expr(
            submission_judgement::Column::LeaseHeartbeatAt,
            Expr::value(Some(now)),
        )
        .col_expr(
            submission_judgement::Column::LeasedAt,
            Expr::value(Some(now)),
        )
        .filter(submission_judgement::Column::Id.eq(judgement_id))
        .filter(submission_judgement::Column::JudgeEpoch.eq(expected_epoch))
        .exec(&txn)
        .await?
        .rows_affected;
    if judgement_rows == 0 {
        txn.rollback().await.ok();
        return Ok(None);
    }

    // Advance the parent submission in lockstep, epoch-gated for the same reason.
    let submission_rows = submission::Entity::update_many()
        .col_expr(submission::Column::JudgeEpoch, Expr::value(new_epoch))
        .col_expr(
            submission::Column::Status,
            Expr::value(SubmissionStatus::Pending.to_string()),
        )
        .clear_judgement_columns()
        .col_expr(submission::Column::RetryCount, Expr::value(new_retry))
        .col_expr(
            submission::Column::OwnerServerId,
            Expr::value(Some(server_id.to_string())),
        )
        .col_expr(submission::Column::LeaseHeartbeatAt, Expr::value(Some(now)))
        .col_expr(submission::Column::LeasedAt, Expr::value(Some(now)))
        .filter(submission::Column::Id.eq(submission_id))
        .filter(submission::Column::JudgeEpoch.eq(expected_epoch))
        .exec(&txn)
        .await?
        .rows_affected;
    if submission_rows == 0 {
        txn.rollback().await.ok();
        return Ok(None);
    }

    // Wipe the failed attempt's per-testcase results so the re-judge starts clean.
    test_case_result::Entity::delete_many()
        .filter(test_case_result::Column::JudgementId.eq(judgement_id))
        .exec(&txn)
        .await?;

    txn.commit().await?;

    Ok(submission::Entity::find_by_id(submission_id)
        .one(db)
        .await?)
}

async fn record_dispatch_failure(
    db: &DatabaseConnection,
    metrics: &common::metrics::Metrics,
    submission_id: i32,
    judgement_id: i32,
    error_code: &'static str,
    error_message: &str,
    judge_epoch: i32,
) {
    let recovered = match mark_submission_dispatch_system_error(
        db,
        submission_id,
        judgement_id,
        error_code,
        error_message,
        judge_epoch,
    )
    .await
    {
        Ok(()) => true,
        Err(e) => {
            error!(
                submission_id,
                judgement_id,
                error_code,
                marking_error = %e,
                "Failed to persist submission SystemError — submission left non-terminal, stuck-detector will retry"
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

#[instrument(skip(state), fields(submission_id = submission.id))]
pub(crate) async fn dispatch_submission_to_plugin(state: AppState, submission: submission::Model) {
    dispatch_submission_to_plugin_with_judgement(state, submission, None, true).await;
}

#[instrument(
    skip(state),
    fields(
        submission_id = submission.id,
        judgement_id = ?judgement_id,
        problem_id = tracing::field::Empty,
        contest_id = tracing::field::Empty,
    )
)]
pub(crate) async fn dispatch_submission_to_plugin_with_judgement(
    state: AppState,
    submission: submission::Model,
    judgement_id: Option<i32>,
    fire_after_judging: bool,
) {
    let judgement_id = judgement_id.unwrap_or(0);
    let judgement_id = if judgement_id > 0 {
        judgement_id
    } else {
        ensure_active_judgement_id(&state.db, &submission).await
    };
    tracing::Span::current().record("judgement_id", judgement_id);
    tracing::Span::current().record("problem_id", submission.problem_id);
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
                submission.id,
                judgement_id,
                "NO_HANDLER_REGISTERED",
                &format!("No plugin registered for contest type {:?}", contest_type),
                submission.judge_epoch,
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
                submission.id,
                judgement_id,
                "PROBLEM_NOT_FOUND",
                &format!("Problem {} not found", submission.problem_id),
                submission.judge_epoch,
            )
            .await;
            return;
        }
        Err(e) => {
            error!(error = %e, "DB error fetching problem");
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                submission.id,
                judgement_id,
                "DATABASE_ERROR",
                &format!("Failed to fetch problem: {}", e),
                submission.judge_epoch,
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
                submission.id,
                judgement_id,
                "INVALID_FILES",
                &format!("Failed to parse submission files: {}", e),
                submission.judge_epoch,
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
                    submission.id,
                    judgement_id,
                    "DATABASE_ERROR",
                    &format!("Failed to query test cases: {}", e),
                    submission.judge_epoch,
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
        // This check runs on EVERY dispatch, so it also transparently repairs
        // problems that were already stored over-budget before this fix
        // existed: there is no separate backfill migration, because the split
        // is re-derived from current storage state each time a submission is
        // judged. Candidates are walked in a fixed order (test-case
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
                        submission.id,
                        judgement_id,
                        "BLOB_STORE_ERROR",
                        &format!("Failed to store oversized inline test case input: {e}"),
                        submission.judge_epoch,
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
                        submission.id,
                        judgement_id,
                        "BLOB_STORE_ERROR",
                        &format!("Failed to store oversized inline test case expected_output: {e}"),
                        submission.judge_epoch,
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
        judge_epoch: submission.judge_epoch,
        target_worker_id: submission.target_worker_id.clone(),
    };

    let input_bytes = match serde_json::to_vec(&input) {
        Ok(b) => b,
        Err(e) => {
            error!(error = %e, "Failed to serialize plugin input");
            record_dispatch_failure(
                &state.db,
                &state.metrics,
                submission.id,
                judgement_id,
                "SERIALIZATION_ERROR",
                &format!("Failed to serialize input: {}", e),
                submission.judge_epoch,
            )
            .await;
            return;
        }
    };

    let plugin_id = handler.plugin_id.clone();
    let function_name = handler.submission_fn.clone();
    let plugins = state.plugins.clone();
    let hook_registry = state.registries.hook_registry.clone();
    let db = state.db.clone();
    let metrics = state.metrics.clone();
    let submission_id = submission.id;
    let judge_epoch = submission.judge_epoch;
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
            // Retry on plugin-pool contention. Pool exhaustion is transient backpressure
            // and must never produce a permanent SystemError verdict for the contestant.
            let result = call_raw_with_pool_retry(
                plugins.as_ref(),
                &plugin_id,
                &function_name,
                input_bytes,
                PoolRetryPolicy::default(),
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
                                record_dispatch_failure(
                                    &db,
                                    &metrics,
                                    submission_id,
                                    judgement_id,
                                    "PLUGIN_ERROR",
                                    &output
                                        .error_message
                                        .unwrap_or_else(|| "Unknown plugin error".to_string()),
                                    judge_epoch,
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
                                submission_id,
                                judgement_id,
                                "PLUGIN_INVALID_OUTPUT",
                                &format!("Plugin returned invalid output: {}", e),
                                judge_epoch,
                            )
                            .await;
                        }
                    }
                }
                Err(e) => {
                    error!(submission_id, judgement_id, problem_id, contest_id = ?contest_id, error = %e, "Plugin execution failed");
                    record_dispatch_failure(
                        &db,
                        &metrics,
                        submission_id,
                        judgement_id,
                        "PLUGIN_EXECUTION_ERROR",
                        &e.to_string(),
                        judge_epoch,
                    )
                    .await;
                }
            }

            if fire_after_judging {
                fire_after_judging_hooks(
                    &db,
                    hook_registry,
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
    // matching entry. Fall back to "keep inline" (today's behavior) rather
    // than panicking if that invariant is ever violated by a future edit --
    // a stale/missing flag should degrade to the pre-fix behavior, not take
    // down submission dispatch.
    let keep = keep_flags.next().unwrap_or(true);
    if keep {
        return Ok(TestCaseBodyRef::inline(inline_text));
    }

    let hash = state.blob_store.put(inline_text.as_bytes()).await?;
    Ok(TestCaseBodyRef::blob(hash.to_hex()))
}

/// Real-Postgres regression tests for dispatch-time judgement bookkeeping.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::user;
    use testcontainers::ContainerAsync;
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    async fn start_pg() -> (ContainerAsync<Postgres>, DatabaseConnection) {
        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let db = crate::database::init_db(&url)
            .await
            .expect("init schema on test db");
        (container, db)
    }

    /// Seed an owned, in-flight submission whose `created_at` is deliberately an
    /// hour old (mirroring a submission that has been judging for a while).
    async fn seed_owned_submission(db: &DatabaseConnection, owner: &str) -> submission::Model {
        let old = Utc::now() - chrono::Duration::seconds(3600);
        let u = user::ActiveModel {
            username: Set("dispatch-test-user".to_string()),
            password: Set("x".to_string()),
            created_at: Set(old),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert user");
        let p = problem::ActiveModel {
            title: Set("dispatch-test-problem".to_string()),
            content: Set("c".to_string()),
            time_limit: Set(1000),
            memory_limit: Set(262_144),
            created_at: Set(old),
            updated_at: Set(old),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert problem");
        submission::ActiveModel {
            files: Set(serde_json::json!({})),
            language: Set("cpp".to_string()),
            user_id: Set(u.id),
            problem_id: Set(p.id),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(0),
            owner_server_id: Set(Some(owner.to_string())),
            lease_heartbeat_at: Set(Some(old)),
            created_at: Set(old),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert submission")
    }

    /// A judgement created for an owned, in-flight submission must inherit the
    /// submission's `owner_server_id` and carry a fresh `lease_heartbeat_at`, so
    /// the lease-refresh fiber keeps it alive and the deferred-judgement steal
    /// does not reclaim it mid-evaluate. Without an owner, the steal's
    /// `owner_server_id IS NULL AND created_at < threshold` branch grabs it 60s
    /// after creation regardless of active progress.
    #[tokio::test]
    async fn ensure_active_judgement_inherits_submission_owner_and_heartbeat() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;

        let before = Utc::now();
        let judgement_id = ensure_active_judgement_id(&db, &sub).await;
        assert!(judgement_id > 0, "a judgement is created");

        let judgement = submission_judgement::Entity::find_by_id(judgement_id)
            .one(&db)
            .await
            .expect("query judgement")
            .expect("judgement exists");
        assert_eq!(
            judgement.owner_server_id.as_deref(),
            Some("srv-A"),
            "judgement inherits the submission's owner so lease::run refreshes it"
        );
        let heartbeat = judgement
            .lease_heartbeat_at
            .expect("judgement carries a lease heartbeat");
        assert!(
            heartbeat >= before - chrono::Duration::seconds(5),
            "heartbeat is stamped fresh at dispatch, not the submission's stale created_at"
        );
    }

    /// Drive a submission + its current judgement into the post-plugin
    /// finalized state, then return the judgement id. `status`/`verdict` let a
    /// test exercise the gate (e.g. a real SystemError vs a CompileError).
    ///
    /// Both rows are deliberately seeded with a NON-NULL `compile_output`:
    /// today's finalize path nulls it for non-CE verdicts, which kept a
    /// missing `CompileOutput` in this module's hand-rolled reset list
    /// invisible to every test. Seeding it makes the requeue tests prove the
    /// reset actually clears it (it is user-visible via the submission
    /// response when a status co-populates it).
    async fn finalize_judgement(
        db: &DatabaseConnection,
        sub: &submission::Model,
        epoch: i32,
        retry_count: i32,
        status: SubmissionStatus,
        verdict: Option<Verdict>,
    ) -> i32 {
        let now = Utc::now();
        submission::Entity::update_many()
            .col_expr(submission::Column::JudgeEpoch, Expr::value(epoch))
            .col_expr(submission::Column::Status, Expr::value(status.to_string()))
            .col_expr(
                submission::Column::Verdict,
                Expr::value(verdict.as_ref().map(|v| v.as_str().to_string())),
            )
            .col_expr(
                submission::Column::CompileOutput,
                Expr::value(Some("leftover compile diagnostics".to_string())),
            )
            .col_expr(submission::Column::RetryCount, Expr::value(retry_count))
            .filter(submission::Column::Id.eq(sub.id))
            .exec(db)
            .await
            .expect("finalize submission");
        let j = submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(true),
            status: Set(status),
            verdict: Set(verdict),
            compile_output: Set(Some("leftover compile diagnostics".to_string())),
            judge_epoch: Set(epoch),
            retry_count: Set(retry_count),
            owner_server_id: Set(Some("srv-A".to_string())),
            lease_heartbeat_at: Set(Some(now)),
            finalized_at: Set(Some(now)),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert finalized judgement");
        j.id
    }

    async fn seed_tcr(db: &DatabaseConnection, sub_id: i32, judgement_id: i32, epoch: i32) {
        test_case_result::ActiveModel {
            submission_id: Set(sub_id),
            judgement_id: Set(Some(judgement_id)),
            judge_epoch: Set(epoch),
            verdict: Set(Verdict::SystemError),
            score: Set(0.0),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert tcr");
    }

    /// The core fix: a plugin-finalized SystemError (status=Judged,
    /// verdict=SystemError) is a system condition and MUST be requeued for a
    /// bounded retry, in epoch lockstep, owned by this server, with the failed
    /// attempt's results wiped.
    #[tokio::test]
    async fn finalized_system_error_judgement_is_requeued_for_retry() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        let jid = finalize_judgement(
            &db,
            &sub,
            0,
            0,
            SubmissionStatus::Judged,
            Some(Verdict::SystemError),
        )
        .await;
        seed_tcr(&db, sub.id, jid, 0).await;

        let resub = requeue_judgement_for_system_error_retry(&db, sub.id, jid, 0, "srv-A", 5)
            .await
            .expect("requeue call")
            .expect("system-error judgement is requeued");
        assert_eq!(resub.judge_epoch, 1, "submission epoch is bumped");
        assert_eq!(resub.status, SubmissionStatus::Pending, "reset to Pending");
        assert_eq!(resub.verdict, None, "verdict cleared for re-judge");
        assert_eq!(
            resub.compile_output, None,
            "seeded compile_output is cleared on the submission — the \
             hand-rolled reset list this pins against once omitted it"
        );
        assert_eq!(resub.retry_count, 1, "retry budget consumed");
        assert_eq!(
            resub.owner_server_id.as_deref(),
            Some("srv-A"),
            "this server keeps ownership for the re-dispatch"
        );

        let j = submission_judgement::Entity::find_by_id(jid)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(j.judge_epoch, 1, "judgement epoch in lockstep");
        assert!(!j.is_finalized, "judgement un-finalized for re-judge");
        assert!(j.finalized_at.is_none(), "finalized_at cleared");
        assert!(j.is_current, "still the current judgement");
        assert_eq!(j.status, SubmissionStatus::Pending);
        assert_eq!(j.verdict, None);
        assert_eq!(
            j.compile_output, None,
            "seeded compile_output is cleared on the judgement — this column \
             was missing from the pre-extraction reset list (user-visible via \
             the submission response if a future path co-populates it with a \
             SystemError-shaped status)"
        );
        assert_eq!(j.retry_count, 1);
        assert_eq!(
            j.owner_server_id.as_deref(),
            Some("srv-A"),
            "judgement owned by this server with a live lease"
        );
        assert!(j.lease_heartbeat_at.is_some(), "lease stamped fresh");

        let tcr = test_case_result::Entity::find()
            .filter(test_case_result::Column::JudgementId.eq(jid))
            .count(&db)
            .await
            .unwrap();
        assert_eq!(tcr, 0, "the failed attempt's testcase results are wiped");
    }

    /// Shape 2/3: a SystemError that lands as `status == SystemError` with a
    /// NULL verdict - produced by the stuck-handler's terminal give-up
    /// (`error_code == STUCK_JOB`) and by the dispatch-exhaustion path - is just
    /// as much a system condition as a plugin-finalized `verdict == SystemError`,
    /// and must ALSO be requeued. Critically it is seeded at `retry_count == 5`
    /// (the shape on the box: those paths spend the dispatch/stuck budget of 5
    /// before terminalizing), yet the dedicated SystemError-retry budget (50)
    /// still requeues it: no system condition stands as the contestant's verdict.
    #[tokio::test]
    async fn stuck_terminal_system_error_status_is_requeued_within_system_error_budget() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        let jid = finalize_judgement(&db, &sub, 0, 5, SubmissionStatus::SystemError, None).await;
        seed_tcr(&db, sub.id, jid, 0).await;

        let resub = requeue_judgement_for_system_error_retry(&db, sub.id, jid, 0, "srv-A", 50)
            .await
            .expect("requeue call")
            .expect("a stuck-terminal status=SystemError within the budget is requeued");
        assert_eq!(resub.status, SubmissionStatus::Pending, "reset to Pending");
        assert_eq!(
            resub.retry_count, 6,
            "SystemError-retry budget is consumed beyond the dispatch/stuck cap of 5"
        );

        let j = submission_judgement::Entity::find_by_id(jid)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert!(!j.is_finalized, "judgement un-finalized for re-judge");
        assert_eq!(j.status, SubmissionStatus::Pending);
        assert_eq!(j.verdict, None);
    }

    /// Bounded: once the retry budget is exhausted, the SystemError stands.
    #[tokio::test]
    async fn exhausted_system_error_judgement_is_not_requeued() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        // retry_count + 1 > max(5) => exhausted.
        let jid = finalize_judgement(
            &db,
            &sub,
            0,
            5,
            SubmissionStatus::Judged,
            Some(Verdict::SystemError),
        )
        .await;

        let out = requeue_judgement_for_system_error_retry(&db, sub.id, jid, 0, "srv-A", 5)
            .await
            .expect("requeue call");
        assert!(out.is_none(), "an exhausted SystemError is not requeued");

        let j = submission_judgement::Entity::find_by_id(jid)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert!(j.is_finalized, "the SystemError finalize is left intact");
        assert_eq!(j.verdict, Some(Verdict::SystemError));
    }

    /// Safety: a contestant verdict (here WrongAnswer) must never be retried.
    #[tokio::test]
    async fn non_system_error_judgement_is_not_requeued() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        let jid = finalize_judgement(
            &db,
            &sub,
            0,
            0,
            SubmissionStatus::Judged,
            Some(Verdict::WrongAnswer),
        )
        .await;

        let out = requeue_judgement_for_system_error_retry(&db, sub.id, jid, 0, "srv-A", 5)
            .await
            .expect("requeue call");
        assert!(out.is_none(), "a WrongAnswer verdict is never retried");
    }

    /// Safety: a contestant CompileError lands as status=CompilationError; even
    /// though its db verdict text can collapse to "SystemError", the status gate
    /// must keep it final and unretried.
    #[tokio::test]
    async fn compile_error_status_is_not_requeued() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        let jid = finalize_judgement(
            &db,
            &sub,
            0,
            0,
            SubmissionStatus::CompilationError,
            Some(Verdict::SystemError),
        )
        .await;

        let out = requeue_judgement_for_system_error_retry(&db, sub.id, jid, 0, "srv-A", 5)
            .await
            .expect("requeue call");
        assert!(
            out.is_none(),
            "a CompilationError must stay final (contestant's own code)"
        );
    }

    /// Safety: a superseded judgement (its epoch already advanced past the one we
    /// dispatched) must not be requeued.
    #[tokio::test]
    async fn superseded_epoch_judgement_is_not_requeued() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        let jid = finalize_judgement(
            &db,
            &sub,
            3,
            0,
            SubmissionStatus::Judged,
            Some(Verdict::SystemError),
        )
        .await;

        // We dispatched at epoch 2, but the judgement is now at epoch 3.
        let out = requeue_judgement_for_system_error_retry(&db, sub.id, jid, 2, "srv-A", 5)
            .await
            .expect("requeue call");
        assert!(out.is_none(), "a superseded judgement is not requeued");
    }

    /// Seed a CURRENT, still-in-flight (un-finalized) judgement at `epoch` for an
    /// owned submission — the shape a live evaluation (or a just-re-dispatched
    /// age-cap recovery) presents while it is judging.
    async fn seed_current_inflight_judgement(
        db: &DatabaseConnection,
        sub: &submission::Model,
        epoch: i32,
        status: SubmissionStatus,
    ) -> i32 {
        let now = Utc::now();
        submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(false),
            status: Set(status),
            judge_epoch: Set(epoch),
            retry_count: Set(0),
            owner_server_id: Set(Some("srv-A".to_string())),
            lease_heartbeat_at: Set(Some(now)),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert in-flight judgement")
        .id
    }

    /// The dispatch-time epoch guard on the by-id finalize: a zombie dispatch
    /// task whose in-flight job the age-cap stuck-detector recovered — bumping
    /// the SAME judgement id to `epoch + 1` and re-dispatching it — must NOT
    /// clobber that live, newer-epoch judgement when it later fails carrying its
    /// stale captured epoch. Before the guard, the by-id `.update()` finalized
    /// the row unconditionally, flipping a still-in-flight (or already correct)
    /// result to a spurious SystemError.
    #[tokio::test]
    async fn stale_epoch_dispatch_failure_does_not_clobber_readvanced_judgement() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        // The age-cap recovery advanced BOTH rows to epoch 1 in lockstep, and
        // the judgement is judging afresh (current, un-finalized, Running).
        submission::Entity::update_many()
            .col_expr(submission::Column::JudgeEpoch, Expr::value(1))
            .filter(submission::Column::Id.eq(sub.id))
            .exec(&db)
            .await
            .expect("advance submission epoch to 1");
        let jid = seed_current_inflight_judgement(&db, &sub, 1, SubmissionStatus::Running).await;

        // A zombie dispatch task from the ORIGINAL attempt (epoch 0) now fails.
        mark_submission_dispatch_system_error(
            &db,
            sub.id,
            jid,
            "PLUGIN_EXECUTION_ERROR",
            "worker connection dropped",
            0,
        )
        .await
        .expect("stale-epoch failure is recorded without error");

        let j = submission_judgement::Entity::find_by_id(jid)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            j.judge_epoch, 1,
            "the live judgement is untouched at epoch 1"
        );
        assert!(
            !j.is_finalized,
            "the live judgement is NOT prematurely finalized by the stale zombie"
        );
        assert_eq!(
            j.status,
            SubmissionStatus::Running,
            "the live judgement keeps evaluating — not flipped to SystemError"
        );
        assert!(j.finalized_at.is_none(), "no finalize timestamp written");
        assert!(
            j.error_code.is_none(),
            "no stale error stamped onto the live judgement"
        );

        let s = submission::Entity::find_by_id(sub.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(s.judge_epoch, 1, "submission epoch preserved");
        assert_ne!(
            s.status,
            SubmissionStatus::SystemError,
            "the epoch-guarded submission-cache write also no-ops"
        );
    }

    /// The guard does not break the normal path: a dispatch that fails on the
    /// CURRENT epoch still finalizes its judgement (and submission cache) as
    /// SystemError, exactly as the pre-guard by-id write did.
    #[tokio::test]
    async fn matching_epoch_dispatch_failure_finalizes_judgement_as_system_error() {
        let (_pg, db) = start_pg().await;
        let sub = seed_owned_submission(&db, "srv-A").await;
        let jid = seed_current_inflight_judgement(&db, &sub, 0, SubmissionStatus::Running).await;

        mark_submission_dispatch_system_error(
            &db,
            sub.id,
            jid,
            "PLUGIN_EXECUTION_ERROR",
            "worker connection dropped",
            0,
        )
        .await
        .expect("matching-epoch dispatch failure is recorded");

        let j = submission_judgement::Entity::find_by_id(jid)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert!(
            j.is_finalized,
            "matching-epoch failure finalizes the judgement"
        );
        assert_eq!(j.status, SubmissionStatus::SystemError);
        assert_eq!(j.error_code.as_deref(), Some("PLUGIN_EXECUTION_ERROR"));
        assert!(j.finalized_at.is_some());

        let s = submission::Entity::find_by_id(sub.id)
            .one(&db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            s.status,
            SubmissionStatus::SystemError,
            "submission cache reflects the SystemError"
        );
    }
}
