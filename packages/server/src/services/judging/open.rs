//! Transitions that create judgements or move "current" between them.

use common::db::DatabaseTransaction;
use common::{SubmissionDlqErrorCode, SubmissionStatus};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, DbErr, EntityTrait, ExprTrait,
    NotSet, QueryFilter, QueryOrder, QuerySelect, Set, Statement, UpdateMany,
};

use super::unit::mirror_current;
use crate::dlq::{DlqService, DlqSubject};
use crate::entity::{submission, submission_judgement, test_case_result};

/// Chainable on `submission::Entity::update_many()`: allocates one fresh
/// judge epoch per matched submission. The new epoch is the returned row's
/// `max_judge_epoch`.
fn allocate_epoch(update: UpdateMany<submission::Entity>) -> UpdateMany<submission::Entity> {
    update.col_expr(
        submission::Column::MaxJudgeEpoch,
        Expr::col(submission::Column::MaxJudgeEpoch).add(1),
    )
}

/// Inserts a new submission and its first, current judgement in the
/// caller's transaction. Both start `Queued`; the claim fiber dispatches the
/// judgement. Any status, epoch or output on `new` is replaced.
pub async fn create_submission(
    txn: &DatabaseTransaction,
    mut new: submission::ActiveModel,
) -> Result<(submission::Model, submission_judgement::Model), DbErr> {
    new.status = Set(SubmissionStatus::Queued);
    new.judge_epoch = Set(0);
    new.max_judge_epoch = Set(0);
    let sub = new.insert(txn).await?;
    let judgement = new_judgement(&sub, 1, true, None, sub.target_worker_id.clone())
        .insert(txn)
        .await?;
    Ok((sub, judgement))
}

/// A `Queued`, never-dispatched judgement of `sub` at its latest allocated
/// epoch.
fn new_judgement(
    sub: &submission::Model,
    version: i32,
    is_current: bool,
    triggered_by_user_id: Option<i32>,
    target_worker_id: Option<String>,
) -> submission_judgement::ActiveModel {
    submission_judgement::ActiveModel {
        submission_id: Set(sub.id),
        version: Set(version),
        is_current: Set(is_current),
        is_finalized: Set(false),
        triggered_by_user_id: Set(triggered_by_user_id),
        target_worker_id: Set(target_worker_id),
        note: Set(None),
        status: Set(SubmissionStatus::Queued),
        verdict: Set(None),
        score: Set(None),
        time_used: Set(None),
        memory_used: Set(None),
        compile_output: Set(None),
        error_code: Set(None),
        error_message: Set(None),
        judge_epoch: Set(sub.max_judge_epoch),
        owner_server_id: Set(None),
        lease_heartbeat_at: Set(None),
        leased_at: Set(None),
        dispatch_retries: Set(0),
        // The first judgement carries its submission's arrival time; a
        // rejudge's is stamped by the database.
        created_at: if version == 1 {
            Set(sub.created_at)
        } else {
            NotSet
        },
        finalized_at: Set(None),
        ..Default::default()
    }
}

/// Why `open_judgement` opens a new judgement row.
pub(crate) enum OpenJudgement {
    /// Admin rejudge or DLQ retry that becomes current at once. The previous
    /// current judgement is superseded and the submission's system-error
    /// retry budget starts over.
    Rejudge {
        triggered_by_user_id: i32,
        /// `Some` overrides the submission's worker pin (`Some(None)`
        /// clears it); `None` keeps it.
        target_worker_id: Option<Option<String>>,
    },
    /// Admin rejudge that runs beside the current judgement until an admin
    /// applies it. The submission row is untouched apart from the epoch
    /// allocation.
    DeferredRejudge {
        triggered_by_user_id: i32,
        /// Pin for the new judgement only; `None` inherits the submission's.
        target_worker_id: Option<Option<String>>,
    },
}

/// Opens a new `Queued` judgement for `submission_id` in the caller's
/// transaction and returns the submission as it is afterwards with the new
/// judgement, or `None` when the submission does not exist.
pub(crate) async fn open_judgement(
    txn: &DatabaseTransaction,
    submission_id: i32,
    open: OpenJudgement,
) -> Result<Option<(submission::Model, submission_judgement::Model)>, DbErr> {
    let mut update = allocate_epoch(
        submission::Entity::update_many().filter(submission::Column::Id.eq(submission_id)),
    );
    if let OpenJudgement::Rejudge { .. } = open {
        update = update.col_expr(submission::Column::SystemErrorRetries, Expr::value(0));
    }
    let Some(sub) = update.exec_with_returning(txn).await?.into_iter().next() else {
        return Ok(None);
    };

    let version = next_version(txn, submission_id).await?;
    match open {
        OpenJudgement::Rejudge {
            triggered_by_user_id,
            target_worker_id,
        } => {
            supersede_current_judgement(txn, submission_id, SupersededBy::Rejudge).await?;
            let target = target_worker_id.unwrap_or_else(|| sub.target_worker_id.clone());
            let judgement = new_judgement(&sub, version, true, Some(triggered_by_user_id), target)
                .insert(txn)
                .await?;
            let sub = mirror_current(txn, submission_id).await?;
            Ok(Some((sub, judgement)))
        }
        OpenJudgement::DeferredRejudge {
            triggered_by_user_id,
            target_worker_id,
        } => {
            let target = target_worker_id.unwrap_or_else(|| sub.target_worker_id.clone());
            let judgement = new_judgement(&sub, version, false, Some(triggered_by_user_id), target)
                .insert(txn)
                .await?;
            Ok(Some((sub, judgement)))
        }
    }
}

async fn next_version<C: ConnectionTrait>(txn: &C, submission_id: i32) -> Result<i32, DbErr> {
    let max: Option<i32> = submission_judgement::Entity::find()
        .select_only()
        .column(submission_judgement::Column::Version)
        .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
        .order_by_desc(submission_judgement::Column::Version)
        .into_tuple()
        .one(txn)
        .await?;
    Ok(max.unwrap_or(0).saturating_add(1))
}

/// Why a current judgement stops being current.
#[derive(Clone, Copy)]
enum SupersededBy {
    Rejudge,
    AppliedJudgement,
}

impl SupersededBy {
    fn message(self) -> &'static str {
        match self {
            SupersededBy::Rejudge => "Superseded by rejudge",
            SupersededBy::AppliedJudgement => "Superseded by an applied judgement",
        }
    }
}

/// Takes the submission's current judgement out of service in one
/// statement: clears `is_current` and, if it was not finalized, finalizes it
/// as `SystemError`. A finalized judgement accepts no plugin writes and is
/// never dispatched again; an already finalized one keeps its verdict as
/// version history.
async fn supersede_current_judgement<C: ConnectionTrait>(
    txn: &C,
    submission_id: i32,
    reason: SupersededBy,
) -> Result<(), DbErr> {
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"UPDATE submission_judgement SET
               is_current = FALSE,
               status = CASE WHEN is_finalized THEN status ELSE $1 END,
               error_code = CASE WHEN is_finalized THEN error_code ELSE $2 END,
               error_message = CASE WHEN is_finalized THEN error_message ELSE $3 END,
               finalized_at = CASE WHEN is_finalized THEN finalized_at ELSE NOW() END,
               is_finalized = TRUE
           WHERE submission_id = $4 AND is_current = TRUE"#,
        [
            SubmissionStatus::SystemError.to_string().into(),
            SubmissionDlqErrorCode::SUPERSEDED.into(),
            reason.message().into(),
            submission_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Outcome of `apply_judgement`.
pub(crate) enum ApplyJudgement {
    Applied(Box<submission::Model>),
    SubmissionNotFound,
    JudgementNotFound,
    NotFinalized,
}

/// Makes a finalized judgement the submission's current one, in the
/// caller's transaction.
///
/// The judgement becomes current under a FRESH epoch, never its own older
/// one: copying it back would move `judge_epoch` backwards and reuse an
/// `after_judging:{id}:{epoch}` dedupe key that already fired. Its kept
/// `test_case_result` rows move to the new epoch with it, so plugins that
/// join results to their judgement on the epoch still find them.
pub(crate) async fn apply_judgement(
    txn: &DatabaseTransaction,
    submission_id: i32,
    judgement_id: i32,
) -> Result<ApplyJudgement, DbErr> {
    // Allocating first also locks the submission row for the rest of the
    // transaction, serializing this against rejudge and discard.
    let Some(sub) = allocate_epoch(
        submission::Entity::update_many().filter(submission::Column::Id.eq(submission_id)),
    )
    .exec_with_returning(txn)
    .await?
    .into_iter()
    .next() else {
        return Ok(ApplyJudgement::SubmissionNotFound);
    };

    let Some(judgement) = submission_judgement::Entity::find_by_id(judgement_id)
        .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
        .one(txn)
        .await?
    else {
        return Ok(ApplyJudgement::JudgementNotFound);
    };
    if !judgement.is_finalized {
        return Ok(ApplyJudgement::NotFinalized);
    }

    supersede_current_judgement(txn, submission_id, SupersededBy::AppliedJudgement).await?;
    submission_judgement::Entity::update_many()
        .col_expr(submission_judgement::Column::IsCurrent, Expr::value(true))
        .col_expr(
            submission_judgement::Column::JudgeEpoch,
            Expr::value(sub.max_judge_epoch),
        )
        .filter(submission_judgement::Column::Id.eq(judgement_id))
        .exec(txn)
        .await?;
    test_case_result::Entity::update_many()
        .col_expr(
            test_case_result::Column::JudgeEpoch,
            Expr::value(sub.max_judge_epoch),
        )
        .filter(test_case_result::Column::JudgementId.eq(judgement_id))
        .exec(txn)
        .await?;
    let sub = mirror_current(txn, submission_id).await?;
    Ok(ApplyJudgement::Applied(Box::new(sub)))
}

/// Outcome of `discard_judgement`.
pub(crate) enum DiscardJudgement {
    Discarded,
    SubmissionNotFound,
    JudgementNotFound,
    IsCurrent,
    NotFinalized,
}

/// Deletes a finalized, non-current judgement and its results, and resolves
/// its dead letter on behalf of `operator_id`, in the caller's transaction.
pub(crate) async fn discard_judgement(
    txn: &DatabaseTransaction,
    submission_id: i32,
    judgement_id: i32,
    operator_id: i32,
) -> Result<DiscardJudgement, DbErr> {
    let locked = submission::Entity::find_by_id(submission_id)
        .select_only()
        .column(submission::Column::Id)
        .lock_exclusive()
        .into_tuple::<i32>()
        .one(txn)
        .await?;
    if locked.is_none() {
        return Ok(DiscardJudgement::SubmissionNotFound);
    }
    let Some(judgement) = submission_judgement::Entity::find_by_id(judgement_id)
        .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
        .one(txn)
        .await?
    else {
        return Ok(DiscardJudgement::JudgementNotFound);
    };
    if judgement.is_current {
        return Ok(DiscardJudgement::IsCurrent);
    }
    if !judgement.is_finalized {
        return Ok(DiscardJudgement::NotFinalized);
    }
    test_case_result::Entity::delete_many()
        .filter(test_case_result::Column::JudgementId.eq(judgement_id))
        .exec(txn)
        .await?;
    submission_judgement::Entity::delete_by_id(judgement_id)
        .exec(txn)
        .await?;
    DlqService::new(txn)
        .resolve_subject(DlqSubject::Judgement(judgement_id), operator_id)
        .await?;
    Ok(DiscardJudgement::Discarded)
}
