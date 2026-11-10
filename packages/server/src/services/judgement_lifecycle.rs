//! Single source of truth for judge epochs and for opening a new judgement.
//!
//! A judge epoch is the fencing token of one dispatch of one judgement.
//! Worker results, dispatch failures (`mark_submission_system_error_with_epoch`)
//! and the `after_judging:{id}:{epoch}` hook dedupe key are all keyed on it,
//! so two judgements of one submission must never share an epoch, and a
//! submission's epoch must never move backwards.
//!
//! Every epoch comes from one allocator: `submission.max_judge_epoch`, bumped
//! in SQL by [`AllocateJudgeEpoch`]. The bump runs inside an UPDATE of the
//! submission row, so concurrent allocators for the same submission are
//! serialized by its row lock and each sees the previous one's value. A
//! judgement's epoch is then copied from its submission
//! ([`allocated_epoch`] or the RETURNING row of [`open_judgement`]), never
//! computed locally as `epoch + 1`. Local `+ 1` arithmetic was how a deferred
//! rejudge and a later immediate rejudge both picked `judge_epoch + 1`.
//!
//! Every path that starts a new judgement row (admin rejudge, deferred
//! rejudge, DLQ retry, stuck-job retry and lease steal) goes through
//! [`open_judgement`], so the epoch, the supersede step and the submission
//! reset cannot drift apart between them.

use chrono::Utc;
use common::{SubmissionDlqErrorCode, SubmissionStatus};
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, DbErr, EntityTrait, ExprTrait,
    QueryFilter, QueryOrder, Set, Statement, UpdateMany,
};

use crate::entity::judgement_reset::ClearJudgementColumns;
use crate::entity::{submission, submission_judgement};

/// The next epoch of a `submission` row. `GREATEST` with `judge_epoch` keeps
/// allocation above the current epoch even for a row whose
/// `max_judge_epoch` predates the column's backfill.
const NEXT_EPOCH_SQL: &str = r#"GREATEST("max_judge_epoch", "judge_epoch") + 1"#;

/// Chainable on `submission::Entity::update_many()`: allocates one fresh epoch
/// per matched submission.
pub(crate) trait AllocateJudgeEpoch: Sized {
    /// Allocates a fresh epoch and makes it the submission's `judge_epoch`,
    /// for a re-dispatch of the submission's CURRENT judgement.
    fn advance_judge_epoch(self) -> Self;
    /// Allocates a fresh epoch for a NON-current judgement and leaves the
    /// submission's own `judge_epoch` (its current judgement's fence) alone.
    fn reserve_judge_epoch(self) -> Self;
}

impl AllocateJudgeEpoch for UpdateMany<submission::Entity> {
    fn advance_judge_epoch(self) -> Self {
        self.col_expr(submission::Column::JudgeEpoch, Expr::cust(NEXT_EPOCH_SQL))
            .col_expr(
                submission::Column::MaxJudgeEpoch,
                Expr::cust(NEXT_EPOCH_SQL),
            )
    }

    fn reserve_judge_epoch(self) -> Self {
        self.col_expr(
            submission::Column::MaxJudgeEpoch,
            Expr::cust(NEXT_EPOCH_SQL),
        )
    }
}

/// `submission_judgement.judge_epoch` value for an in-place re-dispatch: the
/// epoch most recently allocated for the judgement's submission. Use it in
/// the same transaction right after [`AllocateJudgeEpoch`] ran on that
/// submission; the row lock that UPDATE took keeps anyone else from
/// allocating in between.
pub(crate) fn allocated_epoch() -> SimpleExpr {
    Expr::cust(
        "(SELECT s.max_judge_epoch FROM submission s \
          WHERE s.id = submission_judgement.submission_id)",
    )
}

/// Statuses of a judgement that is still being dispatched or judged.
const IN_FLIGHT: [SubmissionStatus; 3] = [
    SubmissionStatus::Pending,
    SubmissionStatus::Compiling,
    SubmissionStatus::Running,
];

/// Why [`open_judgement`] opens a new judgement row.
pub(crate) enum OpenJudgement {
    /// Admin rejudge or DLQ retry that becomes current at once. The
    /// submission goes back to `Queued`; the claim fiber dispatches it.
    Rejudge {
        triggered_by_user_id: i32,
        /// `Some` overrides the submission's worker pin (`Some(None)`
        /// clears it); `None` keeps it.
        target_worker_id: Option<Option<String>>,
    },
    /// Admin rejudge that runs beside the current judgement until an admin
    /// applies it. The submission keeps its status, verdict and epoch; the
    /// new judgement starts `Queued` for the claim fiber's judgement scan.
    DeferredRejudge {
        triggered_by_user_id: i32,
        /// Pin for the new judgement only; `None` inherits the submission's.
        target_worker_id: Option<Option<String>>,
    },
    /// Re-dispatch of the in-flight current judgement after its owner was
    /// lost, by this server. Applies only while the submission is still in
    /// flight at `expected_epoch`.
    Redispatch {
        expected_epoch: i32,
        owner_server_id: String,
        reason: RedispatchReason,
    },
}

/// Which detector re-dispatches an abandoned judgement.
#[derive(Clone, Copy)]
pub(crate) enum RedispatchReason {
    /// The lease-steal scanner found the owner's lease expired.
    LeaseSteal,
    /// The stuck-job detector found the judgement in flight too long.
    StuckJob,
}

/// Why a current judgement stops being current. Each reason picks the
/// `error_code`/`error_message` a still-in-flight judgement is finalized with
/// (see [`supersede_current_judgement`]).
#[derive(Clone, Copy)]
pub(crate) enum SupersededBy {
    /// An admin rejudge or DLQ retry opened a new current judgement.
    Rejudge,
    /// An admin applied another, finalized judgement.
    AppliedJudgement,
    /// The lease-steal scanner re-dispatched under a new judgement.
    LeaseSteal,
    /// The stuck-job detector re-dispatched under a new judgement.
    StuckJob,
}

impl SupersededBy {
    fn error(self) -> (&'static str, &'static str) {
        match self {
            SupersededBy::Rejudge => (SubmissionDlqErrorCode::SUPERSEDED, "Superseded by rejudge"),
            SupersededBy::AppliedJudgement => (
                SubmissionDlqErrorCode::SUPERSEDED,
                "Superseded by an applied judgement",
            ),
            SupersededBy::LeaseSteal => (
                SubmissionDlqErrorCode::SUPERSEDED,
                "Superseded by lease-steal retry",
            ),
            SupersededBy::StuckJob => (
                SubmissionDlqErrorCode::STUCK_JOB,
                "Superseded by stuck-job retry",
            ),
        }
    }
}

impl OpenJudgement {
    /// The reason the previous current judgement is superseded, or `None`
    /// when the new judgement does not replace it.
    fn supersedes(&self) -> Option<SupersededBy> {
        match self {
            OpenJudgement::Rejudge { .. } => Some(SupersededBy::Rejudge),
            OpenJudgement::DeferredRejudge { .. } => None,
            OpenJudgement::Redispatch { reason, .. } => Some(match reason {
                RedispatchReason::LeaseSteal => SupersededBy::LeaseSteal,
                RedispatchReason::StuckJob => SupersededBy::StuckJob,
            }),
        }
    }
}

/// Opens a new judgement row for `submission_id`, in the caller's
/// transaction:
///
/// 1. updates the submission row: allocates the new judgement's epoch and,
///    for a judgement that becomes current, resets the submission for its
///    dispatch;
/// 2. supersedes the previous current judgement, if the new one replaces it;
/// 3. inserts the new judgement at the next version.
///
/// Returns the updated submission and the new judgement, or `None` when the
/// submission does not exist or (for [`OpenJudgement::Redispatch`]) is no
/// longer in flight at the expected epoch. Nothing is written in that case.
pub(crate) async fn open_judgement<C: ConnectionTrait>(
    txn: &C,
    submission_id: i32,
    open: OpenJudgement,
) -> Result<Option<(submission::Model, submission_judgement::Model)>, DbErr> {
    let update = submission::Entity::update_many().filter(submission::Column::Id.eq(submission_id));
    let update = match &open {
        OpenJudgement::Rejudge {
            target_worker_id, ..
        } => {
            let update = update
                .advance_judge_epoch()
                .col_expr(
                    submission::Column::Status,
                    Expr::value(SubmissionStatus::Queued.to_string()),
                )
                .clear_judgement_columns();
            match target_worker_id {
                Some(target) => update.col_expr(
                    submission::Column::TargetWorkerId,
                    Expr::value(target.clone()),
                ),
                None => update,
            }
        }
        OpenJudgement::DeferredRejudge { .. } => update.reserve_judge_epoch(),
        OpenJudgement::Redispatch {
            expected_epoch,
            owner_server_id,
            ..
        } => update
            .advance_judge_epoch()
            .col_expr(
                submission::Column::Status,
                Expr::value(SubmissionStatus::Pending.to_string()),
            )
            .clear_judgement_columns()
            .col_expr(
                submission::Column::RetryCount,
                Expr::col(submission::Column::RetryCount).add(1),
            )
            .col_expr(
                submission::Column::OwnerServerId,
                Expr::value(Some(owner_server_id.clone())),
            )
            .col_expr(submission::Column::LeaseHeartbeatAt, Expr::cust("NOW()"))
            .col_expr(submission::Column::LeasedAt, Expr::cust("NOW()"))
            .filter(submission::Column::JudgeEpoch.eq(*expected_epoch))
            .filter(submission::Column::Status.is_in(IN_FLIGHT)),
    };
    let Some(sub) = update.exec_with_returning(txn).await?.into_iter().next() else {
        return Ok(None);
    };

    if let Some(reason) = open.supersedes() {
        supersede_current_judgement(txn, submission_id, reason).await?;
    }

    let max_version: Option<i32> = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
        .order_by_desc(submission_judgement::Column::Version)
        .one(txn)
        .await?
        .map(|j| j.version);

    let redispatch = matches!(open, OpenJudgement::Redispatch { .. });
    let (is_current, status, triggered_by_user_id, target_worker_id) = match open {
        OpenJudgement::Rejudge {
            triggered_by_user_id,
            ..
        } => (
            true,
            // The submission row is `Queued` and the claim fiber promotes it;
            // the current judgement starts at `Pending` so that dispatch can
            // write its first state transition.
            SubmissionStatus::Pending,
            Some(triggered_by_user_id),
            sub.target_worker_id.clone(),
        ),
        OpenJudgement::DeferredRejudge {
            triggered_by_user_id,
            target_worker_id,
        } => (
            false,
            // The submission keeps its terminal status, so the claim fiber's
            // submission scan never reaches this judgement; its judgement
            // scan picks up a `Queued` judgement instead. That makes the
            // deferred rejudge durable across an api crash after commit.
            SubmissionStatus::Queued,
            Some(triggered_by_user_id),
            target_worker_id.unwrap_or_else(|| sub.target_worker_id.clone()),
        ),
        OpenJudgement::Redispatch { .. } => (
            true,
            SubmissionStatus::Pending,
            None,
            sub.target_worker_id.clone(),
        ),
    };
    let judgement = submission_judgement::ActiveModel {
        submission_id: Set(submission_id),
        version: Set(max_version.unwrap_or(0).saturating_add(1)),
        is_current: Set(is_current),
        is_finalized: Set(false),
        triggered_by_user_id: Set(triggered_by_user_id),
        target_worker_id: Set(target_worker_id),
        note: Set(None),
        status: Set(status),
        verdict: Set(None),
        score: Set(None),
        time_used: Set(None),
        memory_used: Set(None),
        compile_output: Set(None),
        error_code: Set(None),
        error_message: Set(None),
        judge_epoch: Set(sub.max_judge_epoch),
        // A re-dispatched judgement is owned by this server with a live lease
        // from the start, so the lease fiber keeps it alive and the steal
        // scanner leaves it alone. A rejudge starts unowned (the submission
        // row still names the previous dispatch's owner and lease) and is
        // stamped by the claim fiber when it dispatches.
        owner_server_id: Set(sub.owner_server_id.clone().filter(|_| redispatch)),
        lease_heartbeat_at: Set(sub.lease_heartbeat_at.filter(|_| redispatch)),
        leased_at: Set(sub.leased_at.filter(|_| redispatch)),
        created_at: Set(Utc::now()),
        finalized_at: Set(None),
        ..Default::default()
    }
    .insert(txn)
    .await?;

    Ok(Some((sub, judgement)))
}

/// Takes the submission's current judgement out of service, in one
/// statement: clears `is_current` and, if it was still in flight, finalizes
/// it as `SystemError` with `reason`'s code and message. A finalized
/// judgement is never picked up again by the stuck detector or the steal
/// scanner, its worker's late writes are rejected (`submission_update`
/// requires `is_finalized = FALSE`), and `discard` accepts it. An already
/// finalized judgement keeps its verdict untouched as version history.
pub(crate) async fn supersede_current_judgement<C: ConnectionTrait>(
    txn: &C,
    submission_id: i32,
    reason: SupersededBy,
) -> Result<(), DbErr> {
    let (error_code, error_message) = reason.error();
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
            error_code.into(),
            error_message.into(),
            submission_id.into(),
        ],
    ))
    .await?;
    Ok(())
}
