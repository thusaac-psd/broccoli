use chrono::Utc;
use common::SubmissionStatus;
use sea_orm::{ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter};

use crate::entity::{
    code_run, code_run_result,
    judgement_reset::{ClearJudgementColumns, ClearJudgementFields},
    submission, submission_judgement, test_case_result,
};

use super::{StuckRecovery, detector_retry_lease};
use crate::services::judgement_lifecycle::{
    AllocateJudgeEpoch, OpenJudgement, RedispatchReason, RetryBudget, allocated_epoch,
    open_judgement, redispatch_current_submission,
};

pub(super) async fn recover_stuck_submission_without_steal(
    txn: &DatabaseTransaction,
    submission: &submission::Model,
    server_id: &str,
) -> anyhow::Result<StuckRecovery> {
    // Supersede the abandoned current judgement and re-dispatch under a fresh
    // judgement, through the same `open_judgement` every new judgement goes
    // through. It applies only while the row is still in flight at the epoch
    // the detector observed; otherwise someone else already handled it.
    let Some((model, _judgement)) = open_judgement(
        txn,
        submission.id,
        OpenJudgement::Redispatch {
            expected_epoch: submission.judge_epoch,
            owner_server_id: server_id.to_string(),
            reason: RedispatchReason::StuckJob,
        },
    )
    .await?
    else {
        return Ok(StuckRecovery::Skip);
    };

    Ok(StuckRecovery::RedispatchSubmission {
        retry_count: model.retry_count,
        model,
    })
}

pub(super) async fn recover_stuck_code_run_without_steal(
    txn: &DatabaseTransaction,
    run: &code_run::Model,
    server_id: &str,
) -> anyhow::Result<StuckRecovery> {
    let new_retry_count = run.retry_count.saturating_add(1);
    let new_epoch = run.judge_epoch.saturating_add(1);
    let lease_heartbeat_at = Utc::now();
    let (owner_server_id, lease_heartbeat_at) = detector_retry_lease(server_id, lease_heartbeat_at);

    // Claim the run FIRST via the epoch+status-guarded update, then delete its
    // stale per-run results - the same guard-then-delete order the submission
    // and judgement siblings use. Two detectors can race the same stuck run;
    // the loser's guarded update matches 0 rows and returns `Skip`, but the
    // caller (`handlers::handle_stuck_code_run`) commits the transaction
    // unconditionally. Deleting `code_run_result` before the guard therefore
    // let a lost race persist the delete, wiping the results that the winning
    // epoch's re-dispatch is (re)producing - and `build_code_run_response`
    // reads results by `code_run_id` with no epoch filter, so a polling user
    // saw missing/partial test cases for a run that actually completed. Only
    // the winner (rows_affected == 1) reaches the delete now.
    let affected = code_run::Entity::update_many()
        .col_expr(
            code_run::Column::Status,
            sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
        )
        .col_expr(
            code_run::Column::RetryCount,
            sea_orm::sea_query::Expr::value(new_retry_count),
        )
        .col_expr(
            code_run::Column::JudgeEpoch,
            sea_orm::sea_query::Expr::value(new_epoch),
        )
        .col_expr(
            code_run::Column::OwnerServerId,
            sea_orm::sea_query::Expr::value(owner_server_id.clone()),
        )
        .col_expr(
            code_run::Column::LeaseHeartbeatAt,
            sea_orm::sea_query::Expr::value(lease_heartbeat_at),
        )
        .col_expr(
            code_run::Column::LeasedAt,
            sea_orm::sea_query::Expr::value(lease_heartbeat_at),
        )
        .clear_judgement_columns()
        .filter(code_run::Column::Id.eq(run.id))
        .filter(code_run::Column::JudgeEpoch.eq(run.judge_epoch))
        .filter(code_run::Column::Status.is_in(common::StatusPhase::InFlight.statuses()))
        .exec(txn)
        .await?;

    if affected.rows_affected == 0 {
        return Ok(StuckRecovery::Skip);
    }

    code_run_result::Entity::delete_many()
        .filter(code_run_result::Column::CodeRunId.eq(run.id))
        .exec(txn)
        .await?;

    let mut redispatch_model = run.clone();
    redispatch_model.status = SubmissionStatus::Pending;
    redispatch_model.retry_count = new_retry_count;
    redispatch_model.judge_epoch = new_epoch;
    redispatch_model.owner_server_id = owner_server_id;
    redispatch_model.lease_heartbeat_at = lease_heartbeat_at;
    redispatch_model.leased_at = lease_heartbeat_at;
    redispatch_model.clear_judgement_fields();

    Ok(StuckRecovery::RedispatchCodeRun {
        model: redispatch_model,
        retry_count: new_retry_count,
    })
}

pub(super) async fn recover_stuck_judgement_without_steal(
    txn: &DatabaseTransaction,
    judgement: &submission_judgement::Model,
    server_id: &str,
) -> anyhow::Result<StuckRecovery> {
    let new_retry_count = judgement.retry_count.saturating_add(1);
    let lease_heartbeat_at = Utc::now();
    let (owner_server_id, lease_heartbeat_at) = detector_retry_lease(server_id, lease_heartbeat_at);

    // Allocate the judgement's fresh epoch on its submission first. A current
    // judgement's epoch is also the submission's fence, so it advances in
    // lockstep (epoch-gated); a non-current (deferred) judgement only
    // reserves one, leaving the current judgement's fence alone. The
    // judgement row then copies the allocated epoch below.
    let parent = if judgement.is_current {
        redispatch_current_submission(
            judgement.submission_id,
            server_id,
            RetryBudget::AtLeast(new_retry_count),
        )
        .filter(submission::Column::JudgeEpoch.eq(judgement.judge_epoch))
        .filter(submission::Column::Status.is_in(common::StatusPhase::InFlight.statuses()))
    } else {
        submission::Entity::update_many()
            .filter(submission::Column::Id.eq(judgement.submission_id))
            .reserve_judge_epoch()
    };
    let Some(mut sub) = parent.exec_with_returning(txn).await?.into_iter().next() else {
        return Ok(StuckRecovery::Skip);
    };

    test_case_result::Entity::delete_many()
        .filter(test_case_result::Column::JudgementId.eq(judgement.id))
        .exec(txn)
        .await?;

    let Some(redispatched) = submission_judgement::Entity::update_many()
        .col_expr(
            submission_judgement::Column::Status,
            sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
        )
        .col_expr(
            submission_judgement::Column::RetryCount,
            sea_orm::sea_query::Expr::value(new_retry_count),
        )
        .col_expr(submission_judgement::Column::JudgeEpoch, allocated_epoch())
        .col_expr(
            submission_judgement::Column::OwnerServerId,
            sea_orm::sea_query::Expr::value(owner_server_id.clone()),
        )
        .col_expr(
            submission_judgement::Column::LeaseHeartbeatAt,
            sea_orm::sea_query::Expr::value(lease_heartbeat_at),
        )
        .col_expr(
            submission_judgement::Column::LeasedAt,
            sea_orm::sea_query::Expr::value(lease_heartbeat_at),
        )
        .clear_judgement_columns()
        .filter(submission_judgement::Column::Id.eq(judgement.id))
        .filter(submission_judgement::Column::JudgeEpoch.eq(judgement.judge_epoch))
        .filter(
            submission_judgement::Column::Status.is_in(common::StatusPhase::InFlight.statuses()),
        )
        .filter(submission_judgement::Column::IsFinalized.eq(false))
        .exec_with_returning(txn)
        .await?
        .into_iter()
        .next()
    else {
        anyhow::bail!(
            "stuck judgement recovery parent update succeeded but judgement update matched no rows"
        );
    };

    // The dispatch carries the submission model with the judgement's epoch as
    // its fence; for a deferred judgement that differs from the submission
    // row's own (current) epoch, which stays untouched in the database.
    sub.status = SubmissionStatus::Pending;
    sub.judge_epoch = redispatched.judge_epoch;
    sub.retry_count = sub.retry_count.max(new_retry_count);
    sub.owner_server_id = owner_server_id;
    sub.lease_heartbeat_at = lease_heartbeat_at;
    sub.leased_at = lease_heartbeat_at;
    if let Some(target) = judgement.target_worker_id.clone() {
        sub.target_worker_id = Some(target);
    }

    Ok(StuckRecovery::RedispatchJudgement {
        submission: sub,
        judgement_id: judgement.id,
        fire_after_judging: judgement.is_current,
        retry_count: new_retry_count,
    })
}
