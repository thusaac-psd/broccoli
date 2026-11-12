use chrono::Utc;
use common::{DlqConfig, DlqErrorCode, DlqMessageType, SubmissionDlqErrorCode, SubmissionStatus};
use sea_orm::sea_query::LockType;
use sea_orm::{
    ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QuerySelect, TransactionTrait,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::consumers::{mark_code_run_system_error, mark_submission_system_error_with_epoch};
use crate::dlq::DlqService;
use crate::entity::{code_run, dead_letter_message, submission, submission_judgement};
use crate::state::AppState;

use super::recovery::{
    recover_stuck_code_run_without_steal, recover_stuck_judgement_without_steal,
    recover_stuck_submission_without_steal,
};
use super::{
    PENDING_ORPHAN_TIMEOUT_SECS, QUEUED_OBSERVABILITY_THRESHOLD_SECS, StuckDisposition,
    StuckRecovery, inflight_capped, should_recover_directly, stuck_code_run_message_id,
    stuck_disposition, stuck_retries_exceeded_message, stuck_retry_budget_exhausted,
    stuck_submission_judgement_message_id,
};

pub(super) async fn handle_stuck_submission(
    state: &AppState,
    submission_id: i32,
    config: &DlqConfig,
    max_stuck_retries: u32,
    timeout_threshold: chrono::DateTime<Utc>,
    inflight_cap_threshold: Option<chrono::DateTime<Utc>>,
) -> anyhow::Result<()> {
    let queued_observability_threshold =
        Utc::now() - chrono::Duration::seconds(QUEUED_OBSERVABILITY_THRESHOLD_SECS);
    let pending_orphan_threshold =
        Utc::now() - chrono::Duration::seconds(PENDING_ORPHAN_TIMEOUT_SECS);
    let db = &state.db;
    let txn = db.begin().await?;

    let submission = submission::Entity::find_by_id(submission_id)
        .lock(LockType::Update)
        .one(&txn)
        .await?;

    let Some(submission) = submission else {
        txn.rollback().await?;
        return Ok(());
    };

    if submission.status.is_terminal()
        || stuck_disposition(
            &submission.status,
            submission.owner_server_id.as_deref(),
            submission.created_at,
            submission.lease_heartbeat_at,
            submission.leased_at,
            queued_observability_threshold,
            pending_orphan_threshold,
            timeout_threshold,
            inflight_cap_threshold,
        ) != StuckDisposition::Recover
    {
        txn.rollback().await?;
        return Ok(());
    }

    let current_retry_count = submission.retry_count;
    // A row past the dispatch-age cap is recovered directly even when lease
    // steal is enabled (it typically still has a fresh heartbeat, which the
    // steal sweeper keys off of, so steal would otherwise never touch it).
    // Doing so cannot double-dispatch with a concurrent steal: this detector
    // holds the row under `SELECT ... FOR UPDATE` and the steal sweeper uses
    // `FOR UPDATE SKIP LOCKED`, and every recovery/reclaim path stamps both
    // `lease_heartbeat_at` and `leased_at` to NOW() and bumps the judge epoch
    // in one transaction. Whichever transaction wins the row lock invalidates
    // the other's precondition, so the loser re-reads a fresh, non-stuck row
    // and no-ops. Recovery here is epoch-gated + row-locked.
    let is_inflight_capped = inflight_capped(submission.leased_at, inflight_cap_threshold);

    let recovery = if stuck_retry_budget_exhausted(current_retry_count, max_stuck_retries) {
        // Retry budget exhausted: keep the terminal SystemError path and
        // record an unresolved DLQ entry so operators see the failure.
        let system_error_message = stuck_retries_exceeded_message(max_stuck_retries);
        let existing = dead_letter_message::Entity::find()
            .filter(dead_letter_message::Column::SubmissionId.eq(submission_id))
            .filter(
                dead_letter_message::Column::MessageType
                    .eq(DlqMessageType::StuckSubmission.to_string()),
            )
            .filter(dead_letter_message::Column::Resolved.eq(false))
            .one(&txn)
            .await?;

        if existing.is_some() {
            warn!(
                submission_id,
                "Submission already has unresolved DLQ entry, marking terminal without creating duplicate"
            );
            mark_submission_system_error_with_epoch(
                &txn,
                submission.id,
                SubmissionDlqErrorCode::STUCK_JOB,
                &system_error_message,
                Some(submission.judge_epoch),
            )
            .await?;
            StuckRecovery::Terminal
        } else {
            let payload = serde_json::json!({
                "submission_id": submission.id,
                "problem_id": submission.problem_id,
                "user_id": submission.user_id,
                "language": submission.language,
                "contest_id": submission.contest_id,
                "created_at": submission.created_at,
                "retry_count": current_retry_count,
            });

            let dlq = DlqService::new(&txn);
            dlq.create_entry(
                format!("stuck-submission-{}-{}", submission.id, Uuid::new_v4()),
                DlqMessageType::StuckSubmission,
                Some(submission.id),
                payload,
                DlqErrorCode::StuckJob,
                format!(
                    "{}: submission stuck in {} for over {} seconds (retry {}/{})",
                    system_error_message,
                    submission.status,
                    config.stuck_job_timeout_secs,
                    current_retry_count,
                    max_stuck_retries
                ),
            )
            .await?;

            mark_submission_system_error_with_epoch(
                &txn,
                submission.id,
                SubmissionDlqErrorCode::STUCK_JOB,
                &system_error_message,
                Some(submission.judge_epoch),
            )
            .await?;
            StuckRecovery::Terminal
        }
    } else if should_recover_directly(state.config.server.dispatcher_lease_steal_enabled, None)
        || is_inflight_capped
    {
        recover_stuck_submission_without_steal(&txn, &submission, &state.config.server.id).await?
    } else {
        StuckRecovery::Skip
    };

    txn.commit().await?;

    match recovery {
        StuckRecovery::RedispatchSubmission { model, retry_count } => {
            info!(
                submission_id,
                retry = retry_count,
                max_retries = max_stuck_retries,
                "Re-dispatching stuck submission because lease/steal is disabled"
            );
            let dispatch_state = state.clone();
            tokio::spawn(async move {
                crate::services::submission_dispatch::dispatch_submission_to_plugin(
                    dispatch_state,
                    model,
                )
                .await;
            });
        }
        StuckRecovery::RedispatchCodeRun { .. } | StuckRecovery::RedispatchJudgement { .. } => {
            unreachable!("submission handler returned a non-submission recovery")
        }
        StuckRecovery::Terminal => {
            info!(
                submission_id,
                retry = current_retry_count,
                max_retries = max_stuck_retries,
                "Marking stuck submission as SystemError (retry budget exhausted)"
            );
        }
        StuckRecovery::Skip => {}
    }

    Ok(())
}

pub(super) async fn handle_stuck_code_run(
    state: &AppState,
    code_run_id: i32,
    config: &DlqConfig,
    max_stuck_retries: u32,
    timeout_threshold: chrono::DateTime<Utc>,
    inflight_cap_threshold: Option<chrono::DateTime<Utc>>,
) -> anyhow::Result<()> {
    let queued_observability_threshold =
        Utc::now() - chrono::Duration::seconds(QUEUED_OBSERVABILITY_THRESHOLD_SECS);
    let pending_orphan_threshold =
        Utc::now() - chrono::Duration::seconds(PENDING_ORPHAN_TIMEOUT_SECS);
    let db = &state.db;
    let txn = db.begin().await?;

    let run = code_run::Entity::find_by_id(code_run_id)
        .lock(LockType::Update)
        .one(&txn)
        .await?;

    let Some(run) = run else {
        txn.rollback().await?;
        return Ok(());
    };

    if run.status.is_terminal()
        || stuck_disposition(
            &run.status,
            run.owner_server_id.as_deref(),
            run.created_at,
            run.lease_heartbeat_at,
            run.leased_at,
            queued_observability_threshold,
            pending_orphan_threshold,
            timeout_threshold,
            inflight_cap_threshold,
        ) != StuckDisposition::Recover
    {
        txn.rollback().await?;
        return Ok(());
    }

    let current_retry_count = run.retry_count;
    let is_inflight_capped = inflight_capped(run.leased_at, inflight_cap_threshold);

    let recovery = if stuck_retry_budget_exhausted(current_retry_count, max_stuck_retries) {
        let system_error_message = stuck_retries_exceeded_message(max_stuck_retries);
        let payload = serde_json::json!({
            "code_run_id": run.id,
            "problem_id": run.problem_id,
            "user_id": run.user_id,
            "language": run.language,
            "contest_id": run.contest_id,
            "created_at": run.created_at,
            "retry_count": current_retry_count,
        });
        let created = create_unresolved_stuck_dlq_entry(
            &txn,
            stuck_code_run_message_id(run.id),
            DlqMessageType::StuckCodeRun,
            None,
            payload,
            format!(
                "{}: code run stuck in {} for over {} seconds (retry {}/{})",
                system_error_message,
                run.status,
                config.stuck_job_timeout_secs,
                current_retry_count,
                max_stuck_retries
            ),
        )
        .await?;
        if !created {
            warn!(
                code_run_id = run.id,
                "Code run already has unresolved DLQ entry, marking terminal without creating duplicate"
            );
        }
        mark_code_run_system_error(&txn, run.id, "STUCK_JOB", &system_error_message).await?;
        StuckRecovery::Terminal
    } else if should_recover_directly(state.config.server.dispatcher_lease_steal_enabled, None)
        || is_inflight_capped
    {
        recover_stuck_code_run_without_steal(&txn, &run, &state.config.server.id).await?
    } else {
        StuckRecovery::Skip
    };

    txn.commit().await?;

    match recovery {
        StuckRecovery::RedispatchCodeRun { model, retry_count } => {
            info!(
                code_run_id,
                retry = retry_count,
                max_retries = max_stuck_retries,
                "Re-dispatching stuck code run because lease/steal is disabled"
            );
            let dispatch_state = state.clone();
            tokio::spawn(async move {
                crate::services::code_run_dispatch::dispatch_code_run_to_plugin(
                    dispatch_state,
                    model,
                )
                .await;
            });
        }
        StuckRecovery::RedispatchSubmission { .. } | StuckRecovery::RedispatchJudgement { .. } => {
            unreachable!("code-run handler returned a non-code-run recovery")
        }
        StuckRecovery::Terminal => {
            info!(
                code_run_id,
                retry = current_retry_count,
                max_retries = max_stuck_retries,
                "Marking stuck code run as SystemError (retry budget exhausted)"
            );
        }
        StuckRecovery::Skip => {}
    }

    Ok(())
}

pub(super) async fn handle_stuck_submission_judgement(
    state: &AppState,
    judgement_id: i32,
    config: &DlqConfig,
    max_stuck_retries: u32,
    timeout_threshold: chrono::DateTime<Utc>,
    inflight_cap_threshold: Option<chrono::DateTime<Utc>>,
) -> anyhow::Result<()> {
    let queued_observability_threshold =
        Utc::now() - chrono::Duration::seconds(QUEUED_OBSERVABILITY_THRESHOLD_SECS);
    let pending_orphan_threshold =
        Utc::now() - chrono::Duration::seconds(PENDING_ORPHAN_TIMEOUT_SECS);
    let db = &state.db;
    let txn = db.begin().await?;

    let judgement = submission_judgement::Entity::find_by_id(judgement_id)
        .lock(LockType::Update)
        .one(&txn)
        .await?;

    let Some(judgement) = judgement else {
        txn.rollback().await?;
        return Ok(());
    };

    if judgement.status.is_terminal()
        || judgement.is_finalized
        || stuck_disposition(
            &judgement.status,
            judgement.owner_server_id.as_deref(),
            judgement.created_at,
            judgement.lease_heartbeat_at,
            judgement.leased_at,
            queued_observability_threshold,
            pending_orphan_threshold,
            timeout_threshold,
            inflight_cap_threshold,
        ) != StuckDisposition::Recover
    {
        txn.rollback().await?;
        return Ok(());
    }

    let current_retry_count = judgement.retry_count;
    let is_inflight_capped = inflight_capped(judgement.leased_at, inflight_cap_threshold);

    let recovery = if stuck_retry_budget_exhausted(current_retry_count, max_stuck_retries) {
        let system_error_message = stuck_retries_exceeded_message(max_stuck_retries);
        let payload = serde_json::json!({
            "judgement_id": judgement.id,
            "submission_id": judgement.submission_id,
            "version": judgement.version,
            "is_current": judgement.is_current,
            "judge_epoch": judgement.judge_epoch,
            "created_at": judgement.created_at,
            "retry_count": current_retry_count,
        });
        let created = create_unresolved_stuck_dlq_entry(
            &txn,
            stuck_submission_judgement_message_id(judgement.id),
            DlqMessageType::StuckSubmissionJudgement,
            Some(judgement.submission_id),
            payload,
            format!(
                "{}: submission judgement stuck in {} for over {} seconds (retry {}/{})",
                system_error_message,
                judgement.status,
                config.stuck_job_timeout_secs,
                current_retry_count,
                max_stuck_retries
            ),
        )
        .await?;
        if !created {
            warn!(
                judgement_id = judgement.id,
                submission_id = judgement.submission_id,
                "Submission judgement already has unresolved DLQ entry, marking terminal without creating duplicate"
            );
        }

        submission_judgement::Entity::update_many()
            .col_expr(
                submission_judgement::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::SystemError.to_string()),
            )
            .col_expr(
                submission_judgement::Column::ErrorCode,
                sea_orm::sea_query::Expr::value(Some(
                    SubmissionDlqErrorCode::STUCK_JOB.to_string(),
                )),
            )
            .col_expr(
                submission_judgement::Column::ErrorMessage,
                sea_orm::sea_query::Expr::value(Some(system_error_message)),
            )
            .col_expr(
                submission_judgement::Column::IsFinalized,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                submission_judgement::Column::FinalizedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .filter(submission_judgement::Column::Id.eq(judgement.id))
            .filter(
                submission_judgement::Column::Status
                    .is_in(common::StatusPhase::InFlight.statuses()),
            )
            .filter(submission_judgement::Column::IsFinalized.eq(false))
            .exec(&txn)
            .await?;

        StuckRecovery::Terminal
    } else if should_recover_directly(
        state.config.server.dispatcher_lease_steal_enabled,
        Some(judgement.is_current),
    ) || is_inflight_capped
    {
        recover_stuck_judgement_without_steal(&txn, &judgement, &state.config.server.id).await?
    } else {
        StuckRecovery::Skip
    };

    txn.commit().await?;

    match recovery {
        StuckRecovery::RedispatchJudgement {
            submission,
            judgement_id,
            fire_after_judging,
            retry_count,
        } => {
            info!(
                judgement_id,
                retry = retry_count,
                max_retries = max_stuck_retries,
                "Re-dispatching stuck submission judgement because lease/steal is disabled"
            );
            let dispatch_state = state.clone();
            tokio::spawn(async move {
                crate::services::submission_dispatch::dispatch_submission_to_plugin_with_judgement(
                    dispatch_state,
                    submission,
                    Some(judgement_id),
                    fire_after_judging,
                )
                .await;
            });
        }
        StuckRecovery::RedispatchSubmission { .. } | StuckRecovery::RedispatchCodeRun { .. } => {
            unreachable!("judgement handler returned a non-judgement recovery")
        }
        StuckRecovery::Terminal => {
            info!(
                judgement_id,
                retry = current_retry_count,
                max_retries = max_stuck_retries,
                "Marking stuck submission judgement as SystemError (retry budget exhausted)"
            );
        }
        StuckRecovery::Skip => {}
    }

    Ok(())
}

async fn create_unresolved_stuck_dlq_entry(
    txn: &DatabaseTransaction,
    message_id: String,
    message_type: DlqMessageType,
    submission_id: Option<i32>,
    payload: serde_json::Value,
    error_message: String,
) -> anyhow::Result<bool> {
    let existing = dead_letter_message::Entity::find()
        .filter(dead_letter_message::Column::MessageId.eq(&message_id))
        .filter(dead_letter_message::Column::Resolved.eq(false))
        .one(txn)
        .await?;

    if existing.is_some() {
        return Ok(false);
    }

    DlqService::new(txn)
        .create_entry(
            message_id,
            message_type,
            submission_id,
            payload,
            DlqErrorCode::StuckJob,
            error_message,
        )
        .await?;

    Ok(true)
}
