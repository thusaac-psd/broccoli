use broccoli_server_sdk::types::{AfterSubmissionEvent, BeforeSubmissionEvent};
use chrono::Utc;
use common::SubmissionStatus;
use sea_orm::*;

// visibility-bypass-audited: this module has no HTTP handlers - it is
// `pub(super)`/`pub(crate)` plumbing for hook dispatch and rejudge-judgement
// bookkeeping, called only from callers that are themselves permission-gated
// (`rejudge.rs`'s perm::SUBMISSION_REJUDGE handlers, and the create-submission
// paths in `mod.rs`). None of these functions return a `submission`/
// `submission_judgement` row to an HTTP response body.
use crate::entity::{submission, submission_judgement};
use crate::error::AppError;
use crate::hooks::{self, HookOutcome};
use crate::state::AppState;

pub(super) async fn dispatch_before_submission_hooks(
    state: &AppState,
    event: &BeforeSubmissionEvent,
    enabled_plugins: Option<&hooks::ResourceEnablements>,
) -> Result<(), AppError> {
    let outcome =
        hooks::dispatch_hooks_typed(event, enabled_plugins, &state.registries.hook_registry)
            .await?;

    match outcome {
        HookOutcome::Allowed(_) | HookOutcome::Stopped => Ok(()),
        HookOutcome::Rejected {
            code,
            message,
            status_code,
            details,
        } => Err(AppError::PluginRejection {
            code,
            message,
            status_code,
            details,
        }),
    }
}

pub(super) fn fire_after_submission_hooks(
    state: &AppState,
    submission_id: i32,
    user_id: i32,
    problem_id: i32,
    contest_id: Option<i32>,
    language: String,
    enabled_plugins: Option<hooks::ResourceEnablements>,
) {
    hooks::dispatch_hooks_background_typed(
        AfterSubmissionEvent {
            submission_id,
            user_id,
            problem_id,
            contest_id,
            language,
        },
        enabled_plugins,
        state.registries.hook_registry.clone(),
        Some(format!("after_submission:{}", submission_id)),
    );
}

pub(super) async fn find_submission<C: ConnectionTrait>(
    db: &C,
    id: i32,
) -> Result<submission::Model, AppError> {
    submission::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Submission not found".into()))
}

/// Opens a fresh judgement row for a rejudge. On the apply-immediately path the
/// previous current judgement is demoted to `is_current = false` and the new row
/// becomes the current dispatch target; on a deferred rejudge the new row is
/// inserted `is_current = false` and the existing current judgement is left in
/// place. The old row keeps its `test_case_result` attachments so the prior
/// verdict is preserved as version history.
///
/// Caller is responsible for committing the surrounding transaction.
pub(crate) async fn open_rejudge_judgement(
    txn: &DatabaseTransaction,
    sub: &submission::Model,
    triggered_by_user_id: i32,
    target_worker_id: Option<String>,
    note: Option<String>,
    new_judge_epoch: i32,
    apply_immediately: bool,
) -> Result<submission_judgement::Model, AppError> {
    use sea_orm::ColumnTrait;

    let max_version: Option<i32> = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
        .order_by_desc(submission_judgement::Column::Version)
        .one(txn)
        .await?
        .map(|j| j.version);
    let next_version = max_version.unwrap_or(0).saturating_add(1);

    if apply_immediately {
        // Demote any judgement currently flagged as current. There should be
        // at most one row matching this filter (enforced by the partial
        // unique index `idx_submission_judgement_one_current`).
        submission_judgement::Entity::update_many()
            .col_expr(
                submission_judgement::Column::IsCurrent,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
            .filter(submission_judgement::Column::IsCurrent.eq(true))
            .exec(txn)
            .await?;
    }

    let now = Utc::now();
    // For `apply_immediately=true`, the parent submission already goes
    // to `Queued` (UP#37) and the claim fiber promotes the *submission*;
    // the new judgement on this branch is the current one and starts at
    // `Pending` so the dispatch can write its first state transition.
    //
    // For `apply_immediately=false` (deferred rejudge) the parent
    // submission's status is **not** changed - it stays at its previous
    // terminal state - so the claim fiber's submission scan can't reach
    // this row. We instead start the *judgement* at `Queued` so the
    // claim fiber's judgement scan (added in the UP#37-residual fix)
    // promotes it to Pending and dispatches it. Without this, an api
    // crash between txn.commit() and the previous handler-side spawn
    // would silently strand the deferred rejudge forever.
    let initial_status = if apply_immediately {
        SubmissionStatus::Pending
    } else {
        SubmissionStatus::Queued
    };
    let new = submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(next_version),
        is_current: Set(apply_immediately),
        is_finalized: Set(false),
        triggered_by_user_id: Set(Some(triggered_by_user_id)),
        target_worker_id: Set(target_worker_id),
        note: Set(note),
        status: Set(initial_status),
        verdict: Set(None),
        score: Set(None),
        time_used: Set(None),
        memory_used: Set(None),
        compile_output: Set(None),
        error_code: Set(None),
        error_message: Set(None),
        judge_epoch: Set(new_judge_epoch),
        created_at: Set(now),
        finalized_at: Set(None),
        ..Default::default()
    };
    Ok(new.insert(txn).await?)
}
