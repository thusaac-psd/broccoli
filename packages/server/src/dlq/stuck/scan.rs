use std::time::Duration;

use chrono::Utc;
use common::{DlqConfig, SubmissionStatus};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    PaginatorTrait, QueryFilter, QuerySelect, Statement,
};
use tracing::{error, info, warn};

use crate::entity::{code_run, submission, submission_judgement};
use crate::state::AppState;

use super::handlers::{
    handle_stuck_code_run, handle_stuck_submission, handle_stuck_submission_judgement,
};
use super::{
    PENDING_ORPHAN_TIMEOUT_SECS, QUEUED_OBSERVABILITY_THRESHOLD_SECS,
    RECONCILE_FINALIZED_GRACE_SECS, STUCK_RECOVERY_STATUSES,
};

pub async fn run_stuck_job_detector(state: AppState, config: DlqConfig) {
    let scan_interval = Duration::from_secs(config.stuck_job_scan_interval_secs);
    let max_stuck_retries = state.config.server.max_stuck_retries;

    info!(
        timeout_secs = config.stuck_job_timeout_secs,
        scan_interval_secs = config.stuck_job_scan_interval_secs,
        max_stuck_retries,
        "Starting stuck job detector"
    );

    let mut interval = tokio::time::interval(scan_interval);

    loop {
        interval.tick().await;

        let scan = detect_and_handle_stuck_jobs(&state, &config, max_stuck_retries);
        if let Some(Err(e)) = common::deadline::bounded(
            "dlq.stuck_job_detector",
            common::deadline::BACKGROUND_STEP,
            scan,
        )
        .await
        {
            error!(error = %e, "Stuck job detection failed");
        }
    }
}

/// One detection pass, as the detector loop runs it. `pub` so integration
/// tests can drive it directly.
pub async fn detect_once(state: &AppState, config: &DlqConfig) -> anyhow::Result<()> {
    detect_and_handle_stuck_jobs(state, config, state.config.server.max_stuck_retries).await
}

/// Of the candidate `ids` of `table`, the ones this replica should recover:
/// recovery re-dispatches here, so work whose contest type another live
/// replica can judge (and this one cannot) is left to that replica's
/// detector, exactly as the claim, steal and SystemError-retry selects do
/// (see `dispatcher::prefer_local_handler`).
async fn judgeable_here(
    state: &AppState,
    table: &str,
    ids: Vec<i32>,
    local: &[String],
) -> Result<Vec<i32>, sea_orm::DbErr> {
    if ids.is_empty() {
        return Ok(ids);
    }
    let sql = format!(
        "SELECT id FROM {table} WHERE id = ANY($1) AND {}",
        crate::dispatcher::prefer_local_handler_for(table, "$2")
    );
    let rows = state
        .db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            sql,
            [ids.into(), local.to_vec().into()],
        ))
        .await?;
    rows.iter().map(|r| r.try_get::<i32>("", "id")).collect()
}

async fn observe_old_queued_backlog(
    db: &DatabaseConnection,
    queued_observability_threshold: chrono::DateTime<Utc>,
) -> anyhow::Result<()> {
    let old_submissions = submission::Entity::find()
        .filter(crate::dispatcher::is_queued())
        .filter(submission::Column::OwnerServerId.is_null())
        .filter(submission::Column::CreatedAt.lt(queued_observability_threshold))
        .count(db)
        .await?;

    if old_submissions > 0 {
        warn!(
            count = old_submissions,
            threshold_secs = QUEUED_OBSERVABILITY_THRESHOLD_SECS,
            "Queued submissions are older than the dispatcher observability threshold; leaving backlog intact"
        );
    }

    let old_code_runs = code_run::Entity::find()
        .filter(crate::dispatcher::is_queued())
        .filter(code_run::Column::OwnerServerId.is_null())
        .filter(code_run::Column::CreatedAt.lt(queued_observability_threshold))
        .count(db)
        .await?;

    if old_code_runs > 0 {
        warn!(
            count = old_code_runs,
            threshold_secs = QUEUED_OBSERVABILITY_THRESHOLD_SECS,
            "Queued code runs are older than the dispatcher observability threshold; leaving backlog intact"
        );
    }

    let old_judgements = submission_judgement::Entity::find()
        .filter(crate::dispatcher::is_queued())
        .filter(submission_judgement::Column::OwnerServerId.is_null())
        .filter(submission_judgement::Column::IsFinalized.eq(false))
        .filter(submission_judgement::Column::CreatedAt.lt(queued_observability_threshold))
        .count(db)
        .await?;

    if old_judgements > 0 {
        warn!(
            count = old_judgements,
            threshold_secs = QUEUED_OBSERVABILITY_THRESHOLD_SECS,
            "Queued submission judgements are older than the dispatcher observability threshold; leaving backlog intact"
        );
    }

    Ok(())
}

async fn detect_and_handle_stuck_jobs(
    state: &AppState,
    config: &DlqConfig,
    max_stuck_retries: u32,
) -> anyhow::Result<()> {
    let db = &state.db;
    let timeout_threshold =
        Utc::now() - chrono::Duration::seconds(config.stuck_job_timeout_secs as i64);
    let queued_observability_threshold =
        Utc::now() - chrono::Duration::seconds(QUEUED_OBSERVABILITY_THRESHOLD_SECS);
    let pending_orphan_threshold =
        Utc::now() - chrono::Duration::seconds(PENDING_ORPHAN_TIMEOUT_SECS);
    // Dispatch-age cap threshold (`None` = disabled): an owned row whose
    // immutable `leased_at` predates this instant is recovered even if its
    // `lease_heartbeat_at` is fresh, because a live server refreshes the
    // heartbeat of a silently-wedged worker's row indefinitely.
    let inflight_cap_threshold = (config.max_inflight_secs != 0)
        .then(|| Utc::now() - chrono::Duration::seconds(config.max_inflight_secs as i64));

    observe_old_queued_backlog(db, queued_observability_threshold).await?;
    let local = crate::dispatcher::local_contest_types(state).await;

    // Composite recovery predicate:
    // - Pending rows with no owner are orphaned after the 5-minute
    //   per-state threshold.
    // - Owned Pending/Compiling/Running rows are stale when their lease
    //   heartbeat is missing or older than the wide-net threshold, or when
    //   `leased_at` predates the `max_inflight_secs` inflight cap (see above).
    // - Queued rows are intentionally excluded from row-level recovery.
    let mut submission_lease_stale = Condition::any()
        .add(submission::Column::LeaseHeartbeatAt.is_null())
        .add(submission::Column::LeaseHeartbeatAt.lt(timeout_threshold));
    if let Some(cap) = inflight_cap_threshold {
        submission_lease_stale = submission_lease_stale.add(submission::Column::LeasedAt.lt(cap));
    }
    let stuck_submission_ids: Vec<i32> = submission::Entity::find()
        .select_only()
        .column(submission::Column::Id)
        .filter(submission::Column::Status.is_in(STUCK_RECOVERY_STATUSES))
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(submission::Column::Status.eq(SubmissionStatus::Pending))
                        .add(submission::Column::OwnerServerId.is_null())
                        .add(submission::Column::CreatedAt.lt(pending_orphan_threshold)),
                )
                .add(
                    Condition::all()
                        .add(submission::Column::OwnerServerId.is_not_null())
                        .add(submission_lease_stale),
                ),
        )
        .into_tuple()
        .all(db)
        .await?;
    let stuck_submission_ids =
        judgeable_here(state, "submission", stuck_submission_ids, &local).await?;

    if !stuck_submission_ids.is_empty() {
        info!(
            count = stuck_submission_ids.len(),
            "Found stuck submissions, evaluating recovery"
        );

        for submission_id in stuck_submission_ids {
            if let Err(e) = handle_stuck_submission(
                state,
                submission_id,
                config,
                max_stuck_retries,
                timeout_threshold,
                inflight_cap_threshold,
            )
            .await
            {
                error!(
                    submission_id,
                    error = %e,
                    "Failed to handle stuck submission"
                );
            }
        }
    }

    let mut code_run_lease_stale = Condition::any()
        .add(code_run::Column::LeaseHeartbeatAt.is_null())
        .add(code_run::Column::LeaseHeartbeatAt.lt(timeout_threshold));
    if let Some(cap) = inflight_cap_threshold {
        code_run_lease_stale = code_run_lease_stale.add(code_run::Column::LeasedAt.lt(cap));
    }
    let stuck_code_run_ids: Vec<i32> = code_run::Entity::find()
        .select_only()
        .column(code_run::Column::Id)
        .filter(code_run::Column::Status.is_in(STUCK_RECOVERY_STATUSES))
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(code_run::Column::Status.eq(SubmissionStatus::Pending))
                        .add(code_run::Column::OwnerServerId.is_null())
                        .add(code_run::Column::CreatedAt.lt(pending_orphan_threshold)),
                )
                .add(
                    Condition::all()
                        .add(code_run::Column::OwnerServerId.is_not_null())
                        .add(code_run_lease_stale),
                ),
        )
        .into_tuple()
        .all(db)
        .await?;
    let stuck_code_run_ids = judgeable_here(state, "code_run", stuck_code_run_ids, &local).await?;

    if !stuck_code_run_ids.is_empty() {
        info!(
            count = stuck_code_run_ids.len(),
            "Found stuck code runs, evaluating recovery"
        );
        for code_run_id in stuck_code_run_ids {
            if let Err(e) = handle_stuck_code_run(
                state,
                code_run_id,
                config,
                max_stuck_retries,
                timeout_threshold,
                inflight_cap_threshold,
            )
            .await
            {
                error!(code_run_id, error = %e, "Failed to handle stuck code run");
            }
        }
    }

    let mut judgement_lease_stale = Condition::any()
        .add(submission_judgement::Column::LeaseHeartbeatAt.is_null())
        .add(submission_judgement::Column::LeaseHeartbeatAt.lt(timeout_threshold));
    if let Some(cap) = inflight_cap_threshold {
        judgement_lease_stale =
            judgement_lease_stale.add(submission_judgement::Column::LeasedAt.lt(cap));
    }
    let stuck_judgement_ids: Vec<i32> = submission_judgement::Entity::find()
        .select_only()
        .column(submission_judgement::Column::Id)
        .filter(submission_judgement::Column::Status.is_in(STUCK_RECOVERY_STATUSES))
        .filter(submission_judgement::Column::IsFinalized.eq(false))
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(submission_judgement::Column::Status.eq(SubmissionStatus::Pending))
                        .add(submission_judgement::Column::OwnerServerId.is_null())
                        .add(submission_judgement::Column::CreatedAt.lt(pending_orphan_threshold)),
                )
                .add(
                    Condition::all()
                        .add(submission_judgement::Column::OwnerServerId.is_not_null())
                        .add(judgement_lease_stale),
                ),
        )
        .into_tuple()
        .all(db)
        .await?;
    let stuck_judgement_ids =
        judgeable_here(state, "submission_judgement", stuck_judgement_ids, &local).await?;

    if !stuck_judgement_ids.is_empty() {
        info!(
            count = stuck_judgement_ids.len(),
            "Found stuck submission judgements, evaluating recovery"
        );
        for judgement_id in stuck_judgement_ids {
            if let Err(e) = handle_stuck_submission_judgement(
                state,
                judgement_id,
                config,
                max_stuck_retries,
                timeout_threshold,
                inflight_cap_threshold,
            )
            .await
            {
                error!(
                    judgement_id,
                    error = %e,
                    "Failed to handle stuck submission judgement"
                );
            }
        }
    }

    // Catch-up reconciliation: a current judgement may have committed
    // terminal+finalized while its denormalized `submission` row was left
    // non-terminal (a lost/failed submission-row write between the judgement
    // and submission updates, or a crash in between). Such a submission is
    // invisible to both the reaper (it is not finalized with a SystemError
    // verdict) and the stuck-handler (its status is not Pending/Compiling/
    // Running), so it would hang forever. Propagate the already-computed
    // verdict from the finalized current judgement onto the submission row.
    reconcile_finalized_submissions(db, RECONCILE_FINALIZED_GRACE_SECS).await?;

    Ok(())
}

/// Sync submissions whose *current* judgement (at the submission's own epoch)
/// has finalized, but whose denormalized `submission` row never advanced to a
/// terminal status. Pure, idempotent catch-up: it copies the already-computed
/// verdict fields from the finalized judgement onto the submission. Only rows
/// whose judgement finalized longer than `stuck_timeout_secs` ago are touched,
/// so a normal in-flight finalize (which writes both rows within a few
/// milliseconds) is never raced; only genuinely stuck rows are reconciled.
async fn reconcile_finalized_submissions(
    db: &DatabaseConnection,
    stuck_timeout_secs: i64,
) -> anyhow::Result<()> {
    let stmt = Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"UPDATE submission s
           SET status = j.status,
               verdict = j.verdict,
               score = j.score,
               time_used = j.time_used,
               memory_used = j.memory_used,
               compile_output = j.compile_output,
               error_code = j.error_code,
               error_message = j.error_message,
               judged_at = COALESCE(j.finalized_at, NOW())
           FROM submission_judgement j
           WHERE j.submission_id = s.id
             AND j.is_current = TRUE
             AND j.is_finalized = TRUE
             AND j.judge_epoch = s.judge_epoch
             AND j.finalized_at < NOW() - CAST($1 AS INTERVAL)
             AND s.status NOT IN ('Judged', 'CompilationError', 'SystemError')"#,
        vec![format!("{stuck_timeout_secs} seconds").into()],
    );
    let res = db.execute_raw(stmt).await?;
    if res.rows_affected() > 0 {
        warn!(
            count = res.rows_affected(),
            "Reconciled submissions whose current judgement was finalized but whose submission row had not advanced to a terminal status"
        );
    }
    Ok(())
}
