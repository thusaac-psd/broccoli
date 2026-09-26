use std::time::Duration;

use common::{DlqErrorCode, DlqMessageType, SubmissionDlqErrorCode, SubmissionStatus};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    DbBackend, EntityTrait, ExprTrait, QueryFilter, QueryOrder, QueryResult, QuerySelect, Set,
    Statement, TransactionTrait,
};
use tokio::sync::watch;
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::dispatcher::permits::DispatcherReservation;
use crate::dlq::DlqService;
use crate::entity::{
    code_run, judgement_reset::ClearJudgementColumns, submission, submission_judgement,
    test_case_result,
};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ClaimedRow {
    id: i32,
    judge_epoch: i32,
    retry_count: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClaimPlan {
    redispatch_ids: Vec<i32>,
    exhausted_rows: Vec<ClaimedRow>,
}

impl ClaimPlan {
    fn from_rows(rows: Vec<ClaimedRow>, max_dispatch_retries: u32) -> Self {
        let max = std::cmp::min(max_dispatch_retries, i32::MAX as u32) as i32;
        let mut redispatch_ids = Vec::new();
        let mut exhausted_rows = Vec::new();

        for row in rows {
            if row.retry_count.saturating_add(1) > max {
                exhausted_rows.push(row);
            } else {
                redispatch_ids.push(row.id);
            }
        }

        Self {
            redispatch_ids,
            exhausted_rows,
        }
    }
}

pub async fn run(
    state: AppState,
    server_id: String,
    lease_ttl_secs: u64,
    interval_secs: u64,
    batch_size: u32,
    max_dispatch_retries: u32,
    mut cancel: watch::Receiver<bool>,
) {
    if let Err(e) = scan_once(
        state.clone(),
        &server_id,
        lease_ttl_secs,
        batch_size,
        max_dispatch_retries,
    )
    .await
    {
        error!(server_id = %server_id, error = %e, "Initial steal scan failed");
    }

    let mut interval = tokio::time::interval(Duration::from_secs(std::cmp::max(interval_secs, 1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let jitter_ms = rand::random::<u64>() % 3001;
                tokio::time::sleep(Duration::from_millis(jitter_ms)).await;
                if let Err(e) = scan_once(
                    state.clone(),
                    &server_id,
                    lease_ttl_secs,
                    batch_size,
                    max_dispatch_retries,
                ).await {
                    error!(server_id = %server_id, error = %e, "Steal scan failed");
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return;
                }
            }
        }
    }
}

/// True if `e` (anywhere in its chain) is a Postgres deadlock abort. Prefers the
/// SQLSTATE (40P01) so it is locale-independent; falls back to the English
/// message text for error shapes that do not expose a code.
fn is_deadlock(e: &anyhow::Error) -> bool {
    fn sqlstate(err: &sea_orm::DbErr) -> Option<String> {
        use sea_orm::{DbErr, RuntimeErr};
        let sqlx_err = match err {
            DbErr::Query(RuntimeErr::SqlxError(e))
            | DbErr::Exec(RuntimeErr::SqlxError(e))
            | DbErr::Conn(RuntimeErr::SqlxError(e)) => e,
            _ => return None,
        };
        sqlx_err
            .as_database_error()
            .and_then(|db| db.code())
            .map(|c| c.into_owned())
    }
    e.chain().any(|cause| {
        cause
            .downcast_ref::<sea_orm::DbErr>()
            .and_then(sqlstate)
            .as_deref()
            == Some("40P01")
            || cause.to_string().contains("deadlock detected")
    })
}

/// Run a steal claim, retrying if Postgres aborts it with a deadlock. The
/// `submission` and `submission_judgement` steal scans lock the two rows of a
/// (submission, current-judgement) pair in opposite orders (see
/// `claim_deferred_judgements`), so during a coordinator failover two replicas
/// running the opposite scans can deadlock on the same pair. Postgres always
/// aborts exactly one, so retrying the victim after the winner commits succeeds -
/// turning a dropped scan (re-dispatch delayed a whole tick, plus an error log)
/// into an immediate in-tick recovery. Bounded so a genuinely persistent error
/// still surfaces.
async fn claim_retrying_deadlocks<T, F, Fut>(mut claim: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut attempt: u32 = 0;
    loop {
        match claim().await {
            Err(e) if attempt < 3 && is_deadlock(&e) => {
                attempt += 1;
                tracing::debug!(attempt, "steal claim deadlocked; retrying");
                // Brief backoff so the winning transaction commits and releases
                // its locks before the retry re-acquires them.
                tokio::time::sleep(std::time::Duration::from_millis(u64::from(attempt) * 20)).await;
            }
            other => return other,
        }
    }
}

async fn scan_once(
    state: AppState,
    server_id: &str,
    lease_ttl_secs: u64,
    batch_size: u32,
    max_dispatch_retries: u32,
) -> anyhow::Result<()> {
    let submission_slots = reserve_redispatch_slots(&state, batch_size, "submission");
    if !submission_slots.is_empty() {
        let submissions = claim_retrying_deadlocks(|| {
            claim_submissions(
                &state,
                server_id,
                lease_ttl_secs,
                submission_slots.len() as u32,
                max_dispatch_retries,
            )
        })
        .await?;
        for (sub, dispatch_slot) in submissions.into_iter().zip(submission_slots) {
            let state = state.clone();
            dispatch_slot.spawn("steal_submission", async move {
                crate::services::submission_dispatch::dispatch_submission_to_plugin(state, sub)
                    .await;
            });
        }
    }

    let deferred_slots = reserve_redispatch_slots(&state, batch_size, "deferred_judgement");
    if !deferred_slots.is_empty() {
        let deferred_judgements = claim_retrying_deadlocks(|| {
            claim_deferred_judgements(
                &state.db,
                server_id,
                lease_ttl_secs,
                deferred_slots.len() as u32,
                max_dispatch_retries,
            )
        })
        .await?;
        for ((sub, judgement_id, is_current), dispatch_slot) in
            deferred_judgements.into_iter().zip(deferred_slots)
        {
            let state = state.clone();
            // Mirror every sibling recovery path (`system_error_retry.rs`,
            // `stuck.rs`): fire after-judging hooks (scoreboard recompute,
            // notifications) iff this judgement is the current/live-verdict one.
            // `claim_deferred_judgement_rows` filters `is_current = TRUE`, so a
            // reclaimed row is always current; hardcoding `false` here silently
            // dropped those hooks and left the board/notifications stale.
            dispatch_slot.spawn("steal_deferred_judgement", async move {
                crate::services::submission_dispatch::dispatch_submission_to_plugin_with_judgement(
                    state,
                    sub,
                    Some(judgement_id),
                    is_current,
                )
                .await;
            });
        }
    }

    let code_run_slots = reserve_redispatch_slots(&state, batch_size, "code_run");
    if !code_run_slots.is_empty() {
        let code_runs = claim_retrying_deadlocks(|| {
            claim_code_runs(
                &state,
                server_id,
                lease_ttl_secs,
                code_run_slots.len() as u32,
                max_dispatch_retries,
            )
        })
        .await?;
        for (code_run, dispatch_slot) in code_runs.into_iter().zip(code_run_slots) {
            let state = state.clone();
            dispatch_slot.spawn("steal_code_run", async move {
                crate::services::code_run_dispatch::dispatch_code_run_to_plugin(state, code_run)
                    .await;
            });
        }
    }

    Ok(())
}

/// Release this server's OWN in-flight leases at startup so the steal sweeper
/// can reclaim them.
///
/// A restarted coordinator keeps the **same** `server_id`, so every submission,
/// code-run, and unfinalized in-flight judgement (current OR not) it owned
/// before the restart is still tagged `owner_server_id = <self>` in the DB - but
/// the in-memory
/// evaluate/operation driver that was actually judging it died with the old
/// process. The lease-refresh fiber ([`crate::dispatcher::lease`]) filters on
/// `owner_server_id = <self>`, so it would keep renewing those dead leases
/// forever; the steal sweeper's `lease_heartbeat_at < threshold` test then never
/// trips and the work hangs in `Running` until a human intervenes.
///
/// Boot-time recovery breaks the cycle: re-tag every own-owned in-flight row with
/// an `#orphaned` owner sentinel and NULL its heartbeat. That makes the rows look
/// exactly like a **dead foreign server's** expired lease
/// (`owner_server_id IS NOT NULL AND lease_heartbeat_at IS NULL`), which the steal
/// sweeper reclaims on its very first scan - no `created_at` aging required - and
/// re-dispatches onto a fresh driver. The lease fiber no longer matches them
/// (`owner != <self>`), so there is no refresh race regardless of task ordering.
///
/// Strictly scoped to `owner_server_id = <self>`: a sibling replica's in-flight
/// work (different `server_id`, live lease) is left untouched, preserving
/// multi-replica safety. Returns `(submissions, code_runs, judgements)` released.
pub async fn recover_orphaned_leases(
    db: &DatabaseConnection,
    server_id: &str,
) -> Result<(u64, u64, u64), sea_orm::DbErr> {
    // A sentinel owner that (a) is NON-NULL so the steal sweeper's
    // `owner_server_id IS NOT NULL AND lease_heartbeat_at IS NULL` branch
    // reclaims it on the first scan with no `created_at` aging, and (b) differs
    // from `server_id` so the lease fiber's `owner = server_id` refresh skips
    // it. The `#orphaned` suffix cannot collide with a real server id.
    let orphan_owner = format!("{server_id}#orphaned");
    let leased = || {
        [
            SubmissionStatus::Pending,
            SubmissionStatus::Compiling,
            SubmissionStatus::Running,
        ]
    };

    let submissions = submission::Entity::update_many()
        .col_expr(
            submission::Column::OwnerServerId,
            sea_orm::sea_query::Expr::value(Some(orphan_owner.clone())),
        )
        .col_expr(
            submission::Column::LeaseHeartbeatAt,
            sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
        )
        .col_expr(
            submission::Column::LeasedAt,
            sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
        )
        .filter(submission::Column::OwnerServerId.eq(server_id))
        .filter(submission::Column::Status.is_in(leased()))
        .exec(db)
        .await?
        .rows_affected;

    let code_runs = code_run::Entity::update_many()
        .col_expr(
            code_run::Column::OwnerServerId,
            sea_orm::sea_query::Expr::value(Some(orphan_owner.clone())),
        )
        .col_expr(
            code_run::Column::LeaseHeartbeatAt,
            sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
        )
        .col_expr(
            code_run::Column::LeasedAt,
            sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
        )
        .filter(code_run::Column::OwnerServerId.eq(server_id))
        .filter(code_run::Column::Status.is_in(leased()))
        .exec(db)
        .await?
        .rows_affected;

    let judgements = submission_judgement::Entity::update_many()
        .col_expr(
            submission_judgement::Column::OwnerServerId,
            sea_orm::sea_query::Expr::value(Some(orphan_owner.clone())),
        )
        .col_expr(
            submission_judgement::Column::LeaseHeartbeatAt,
            sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
        )
        .col_expr(
            submission_judgement::Column::LeasedAt,
            sea_orm::sea_query::Expr::value(None::<chrono::DateTime<chrono::Utc>>),
        )
        // Scoped strictly to `owner_server_id = <self>`: a sibling replica's rows
        // are never touched. We intentionally do NOT filter on `is_current` - a
        // NON-current in-flight deferred rejudge (e.g. an admin-triggered
        // re-judge that was superseded) owned by this restarting server would
        // otherwise never be released and would hang until the 6-hour ceiling.
        .filter(submission_judgement::Column::OwnerServerId.eq(server_id))
        .filter(submission_judgement::Column::IsFinalized.eq(false))
        .filter(submission_judgement::Column::Status.is_in(leased()))
        .exec(db)
        .await?
        .rows_affected;

    Ok((submissions, code_runs, judgements))
}

fn reserve_redispatch_slots(
    state: &AppState,
    batch_size: u32,
    work_kind: &'static str,
) -> Vec<DispatcherReservation> {
    let mut slots = Vec::with_capacity(batch_size as usize);
    for _ in 0..batch_size {
        match state.dispatcher_permits.reserve() {
            Ok(slot) => slots.push(slot),
            Err(e) => {
                warn!(
                    work_kind = work_kind,
                    reserved = slots.len(),
                    retry_after_secs = e.retry_after_secs,
                    "Steal scanner claim skipped because dispatcher queue is full"
                );
                break;
            }
        }
    }
    slots
}

async fn claim_submissions(
    state: &AppState,
    server_id: &str,
    lease_ttl_secs: u64,
    batch_size: u32,
    max_dispatch_retries: u32,
) -> anyhow::Result<Vec<submission::Model>> {
    let txn = state.db.begin().await?;
    let rows = claim_rows(&txn, "submission", lease_ttl_secs, batch_size).await?;
    let plan = ClaimPlan::from_rows(rows, max_dispatch_retries);

    if !plan.redispatch_ids.is_empty() {
        open_stolen_submission_judgements(&txn, server_id, &plan.redispatch_ids).await?;
        clear_current_submission_results(&txn, &plan.redispatch_ids).await?;

        submission::Entity::update_many()
            .col_expr(
                submission::Column::OwnerServerId,
                sea_orm::sea_query::Expr::value(Some(server_id.to_string())),
            )
            .col_expr(
                submission::Column::LeaseHeartbeatAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                submission::Column::LeasedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                submission::Column::RetryCount,
                sea_orm::sea_query::Expr::col(submission::Column::RetryCount).add(1),
            )
            .col_expr(
                submission::Column::JudgeEpoch,
                sea_orm::sea_query::Expr::col(submission::Column::JudgeEpoch).add(1),
            )
            .col_expr(
                submission::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
            )
            .clear_judgement_columns()
            .filter(submission::Column::Id.is_in(plan.redispatch_ids.clone()))
            .exec(&txn)
            .await?;
    }

    for row in &plan.exhausted_rows {
        let message = format!(
            "Submission dispatch retry limit exhausted after {max_dispatch_retries} attempts"
        );
        let payload = serde_json::json!({
            "submission_id": row.id,
            "judge_epoch": row.judge_epoch,
            "server_id": server_id
        });
        DlqService::new(&txn)
            .create_entry(
                format!(
                    "dispatch-retry-exhausted-submission-{}-{}",
                    row.id,
                    Uuid::new_v4()
                ),
                DlqMessageType::StuckSubmission,
                Some(row.id),
                payload,
                DlqErrorCode::DispatchRetryExhausted,
                message.clone(),
            )
            .await?;
        crate::consumers::mark_submission_system_error_with_epoch(
            &txn,
            row.id,
            SubmissionDlqErrorCode::DISPATCH_RETRY_EXHAUSTED,
            &message,
            Some(row.judge_epoch),
        )
        .await?;
        submission::Entity::update_many()
            .col_expr(
                submission::Column::RetryCount,
                sea_orm::sea_query::Expr::col(submission::Column::RetryCount).add(1),
            )
            .filter(submission::Column::Id.eq(row.id))
            .filter(submission::Column::JudgeEpoch.eq(row.judge_epoch))
            .exec(&txn)
            .await?;
    }

    let models = if plan.redispatch_ids.is_empty() {
        Vec::new()
    } else {
        submission::Entity::find()
            .filter(submission::Column::Id.is_in(plan.redispatch_ids.clone()))
            .all(&txn)
            .await?
    };

    txn.commit().await?;

    if !models.is_empty() || !plan.exhausted_rows.is_empty() {
        info!(
            server_id,
            redispatch = models.len(),
            exhausted = plan.exhausted_rows.len(),
            "Stole stale submission leases"
        );
    }

    Ok(models)
}

async fn open_stolen_submission_judgements(
    txn: &DatabaseTransaction,
    server_id: &str,
    submission_ids: &[i32],
) -> anyhow::Result<()> {
    if submission_ids.is_empty() {
        return Ok(());
    }

    let submissions = submission::Entity::find()
        .filter(submission::Column::Id.is_in(submission_ids.to_vec()))
        .all(txn)
        .await?;

    submission_judgement::Entity::update_many()
        .col_expr(
            submission_judgement::Column::IsCurrent,
            sea_orm::sea_query::Expr::value(false),
        )
        .filter(submission_judgement::Column::SubmissionId.is_in(submission_ids.to_vec()))
        .filter(submission_judgement::Column::IsCurrent.eq(true))
        .exec(txn)
        .await?;

    for sub in submissions {
        let max_version: Option<i32> = submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(sub.id))
            .order_by_desc(submission_judgement::Column::Version)
            .one(txn)
            .await?
            .map(|j| j.version);

        submission_judgement::ActiveModel {
            submission_id: Set(sub.id),
            version: Set(max_version.unwrap_or(0).saturating_add(1)),
            is_current: Set(true),
            is_finalized: Set(false),
            triggered_by_user_id: Set(None),
            target_worker_id: Set(sub.target_worker_id),
            note: Set(None),
            status: Set(SubmissionStatus::Pending),
            verdict: Set(None),
            score: Set(None),
            time_used: Set(None),
            memory_used: Set(None),
            compile_output: Set(None),
            error_code: Set(None),
            error_message: Set(None),
            judge_epoch: Set(sub.judge_epoch.saturating_add(1)),
            // Stamp ownership + a live lease on the replacement current judgement,
            // matching the sibling steal (`claim_deferred_judgements`) and
            // `requeue_judgement_for_system_error_retry`. Without this the row is
            // inserted with NULL owner/heartbeat: `ensure_active_judgement_id`
            // returns this existing id without setting either, the lease fiber
            // (owner = self) never refreshes it, and the steal sweeper re-claims it
            // (`owner_server_id IS NULL AND created_at < threshold`) mid-re-judge.
            owner_server_id: Set(Some(server_id.to_string())),
            lease_heartbeat_at: Set(Some(chrono::Utc::now())),
            leased_at: Set(Some(chrono::Utc::now())),
            created_at: Set(chrono::Utc::now()),
            finalized_at: Set(None),
            ..Default::default()
        }
        .insert(txn)
        .await?;
    }

    Ok(())
}

async fn clear_current_submission_results(
    txn: &DatabaseTransaction,
    submission_ids: &[i32],
) -> anyhow::Result<()> {
    if submission_ids.is_empty() {
        return Ok(());
    }

    let current_unfinalized_judgement_ids: Vec<i32> = submission_judgement::Entity::find()
        .select_only()
        .column(submission_judgement::Column::Id)
        .filter(submission_judgement::Column::SubmissionId.is_in(submission_ids.to_vec()))
        .filter(submission_judgement::Column::IsCurrent.eq(true))
        .filter(submission_judgement::Column::IsFinalized.eq(false))
        .into_tuple()
        .all(txn)
        .await?;

    if current_unfinalized_judgement_ids.is_empty() {
        return Ok(());
    }

    test_case_result::Entity::delete_many()
        .filter(test_case_result::Column::JudgementId.is_in(current_unfinalized_judgement_ids))
        .exec(txn)
        .await?;

    Ok(())
}

// Lock-order note: this path locks the JUDGEMENT first
// (`claim_deferred_judgement_rows`, FOR UPDATE SKIP LOCKED on
// `submission_judgement`) then the parent SUBMISSION (the UPDATE below);
// `claim_submissions` locks the SUBMISSION first then the judgement
// (`open_stolen_submission_judgements`). During a coordinator failover, two
// replicas running the opposite scans can deadlock (Postgres 40P01) on the same
// (submission, current-judgement) pair. Rather than rewrite the core steal query
// to force a uniform lock order (risky, and hard to make airtight across two
// batch scanners), the caller wraps each claim in `claim_retrying_deadlocks`,
// which retries the aborted victim in-tick - the textbook resolution for an
// unavoidable lock-order conflict.
async fn claim_deferred_judgements(
    db: &DatabaseConnection,
    server_id: &str,
    lease_ttl_secs: u64,
    batch_size: u32,
    max_dispatch_retries: u32,
) -> anyhow::Result<Vec<(submission::Model, i32, bool)>> {
    let txn = db.begin().await?;
    let rows = claim_deferred_judgement_rows(&txn, lease_ttl_secs, batch_size).await?;
    let plan = ClaimPlan::from_rows(rows, max_dispatch_retries);

    if !plan.redispatch_ids.is_empty() {
        submission_judgement::Entity::update_many()
            .col_expr(
                submission_judgement::Column::OwnerServerId,
                sea_orm::sea_query::Expr::value(Some(server_id.to_string())),
            )
            .col_expr(
                submission_judgement::Column::LeaseHeartbeatAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                submission_judgement::Column::LeasedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                submission_judgement::Column::RetryCount,
                sea_orm::sea_query::Expr::col(submission_judgement::Column::RetryCount).add(1),
            )
            .col_expr(
                submission_judgement::Column::JudgeEpoch,
                sea_orm::sea_query::Expr::col(submission_judgement::Column::JudgeEpoch).add(1),
            )
            .col_expr(
                submission_judgement::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
            )
            .clear_judgement_columns()
            .filter(submission_judgement::Column::Id.is_in(plan.redispatch_ids.clone()))
            .exec(&txn)
            .await?;

        // This deferred re-dispatch reuses the SAME judgement id and only bumps
        // its epoch (unlike the submission-first steal, which opens a fresh
        // judgement). `clear_judgement_columns` above wipes the judgement row's
        // own summary columns, but the failed attempt's per-testcase rows in
        // `test_case_result` are keyed by `judgement_id` and survive. The
        // re-judge appends new rows at the bumped epoch, and the submission read
        // (`build_*` in handlers/submission/response.rs) selects test_case_result
        // by `judgement_id` alone - no epoch filter - so both attempts' rows
        // would surface as duplicated per-testcase results. Wipe the stale rows,
        // mirroring `clear_current_submission_results` (submission-first steal)
        // and `requeue_judgement_for_system_error_retry` (system-error retry).
        // `plan.redispatch_ids` here ARE judgement ids, so delete directly.
        test_case_result::Entity::delete_many()
            .filter(test_case_result::Column::JudgementId.is_in(plan.redispatch_ids.clone()))
            .exec(&txn)
            .await?;
    }

    for row in &plan.exhausted_rows {
        let message = format!(
            "Deferred judgement dispatch retry limit exhausted after {max_dispatch_retries} attempts"
        );
        submission_judgement::Entity::update_many()
            .col_expr(
                submission_judgement::Column::RetryCount,
                sea_orm::sea_query::Expr::col(submission_judgement::Column::RetryCount).add(1),
            )
            .col_expr(
                submission_judgement::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::SystemError.to_string()),
            )
            .col_expr(
                submission_judgement::Column::ErrorCode,
                sea_orm::sea_query::Expr::value(Some(
                    SubmissionDlqErrorCode::DISPATCH_RETRY_EXHAUSTED.to_string(),
                )),
            )
            .col_expr(
                submission_judgement::Column::ErrorMessage,
                sea_orm::sea_query::Expr::value(Some(message.clone())),
            )
            .col_expr(
                submission_judgement::Column::IsFinalized,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                submission_judgement::Column::FinalizedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .filter(submission_judgement::Column::Id.eq(row.id))
            .filter(submission_judgement::Column::JudgeEpoch.eq(row.judge_epoch))
            .exec(&txn)
            .await?;
    }

    // Terminalize the parent submissions of exhausted deferred judgements. The
    // loop above finalizes the judgement rows as SystemError, but the parent
    // `submission` rows are left untouched and would otherwise hang in a
    // non-terminal status forever. Gate each flip on the submission's own
    // current epoch so a concurrently re-dispatched submission is not clobbered.
    if !plan.exhausted_rows.is_empty() {
        let exhausted_ids: Vec<i32> = plan.exhausted_rows.iter().map(|r| r.id).collect();
        let exhausted_judgements = submission_judgement::Entity::find()
            .filter(submission_judgement::Column::Id.is_in(exhausted_ids))
            .all(&txn)
            .await?;
        let submission_ids: Vec<i32> = exhausted_judgements
            .iter()
            .map(|j| j.submission_id)
            .collect();
        let submissions = if submission_ids.is_empty() {
            Vec::new()
        } else {
            submission::Entity::find()
                .filter(submission::Column::Id.is_in(submission_ids))
                .all(&txn)
                .await?
        };
        for sub in submissions {
            let message = format!(
                "Deferred judgement dispatch retry limit exhausted after {max_dispatch_retries} attempts"
            );
            crate::consumers::mark_submission_system_error_with_epoch(
                &txn,
                sub.id,
                SubmissionDlqErrorCode::DISPATCH_RETRY_EXHAUSTED,
                &message,
                Some(sub.judge_epoch),
            )
            .await?;
        }
    }

    let judgements = if plan.redispatch_ids.is_empty() {
        Vec::new()
    } else {
        submission_judgement::Entity::find()
            .filter(submission_judgement::Column::Id.is_in(plan.redispatch_ids.clone()))
            .all(&txn)
            .await?
    };

    let submission_ids: Vec<i32> = judgements.iter().map(|j| j.submission_id).collect();
    let submissions = if submission_ids.is_empty() {
        Vec::new()
    } else {
        submission::Entity::find()
            .filter(submission::Column::Id.is_in(submission_ids))
            .all(&txn)
            .await?
    };
    let submissions_by_id = submissions
        .into_iter()
        .map(|sub| (sub.id, sub))
        .collect::<std::collections::HashMap<_, _>>();

    // Advance each PARENT submission to the judgement's freshly-bumped epoch in
    // lockstep. The judgement update above incremented its `judge_epoch`; if the
    // submission row is left behind, the eventual epoch-gated finalize
    // (`WHERE id = $ AND judge_epoch = $epoch`) matches 0 rows and the submission
    // hangs in `Running` while its judgement finalizes. Set the epoch to the
    // judgement's value (not `+1`) so a previously-desynced row is repaired, and
    // reset the denormalized result cache the same way the submission-lease steal
    // does above.
    for judgement in &judgements {
        submission::Entity::update_many()
            .col_expr(
                submission::Column::JudgeEpoch,
                sea_orm::sea_query::Expr::value(judgement.judge_epoch),
            )
            .col_expr(
                submission::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
            )
            .col_expr(
                submission::Column::OwnerServerId,
                sea_orm::sea_query::Expr::value(Some(server_id.to_string())),
            )
            .col_expr(
                submission::Column::LeaseHeartbeatAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                submission::Column::LeasedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            // Never LOWER the submission's dispatch-retry count: claim_submissions
            // accumulates it on the submission row and terminalizes when it exceeds
            // the cap, but a deferred judgement carries its own (often fresh, 0)
            // count. Overwriting the submission's with the judgement's would reset
            // an already-flapping submission's budget every deferred steal, so it
            // would never hit max_dispatch_retries. GREATEST keeps it monotonic.
            .col_expr(
                submission::Column::RetryCount,
                sea_orm::sea_query::Expr::cust_with_values(
                    "GREATEST(retry_count, $1)",
                    [judgement.retry_count],
                ),
            )
            .clear_judgement_columns()
            .filter(submission::Column::Id.eq(judgement.submission_id))
            .exec(&txn)
            .await?;
    }

    txn.commit().await?;

    let mut dispatches = Vec::new();
    for judgement in judgements {
        let Some(mut sub) = submissions_by_id.get(&judgement.submission_id).cloned() else {
            error!(
                judgement_id = judgement.id,
                submission_id = judgement.submission_id,
                "Deferred judgement owner submission disappeared during steal"
            );
            continue;
        };
        sub.status = SubmissionStatus::Pending;
        sub.judge_epoch = judgement.judge_epoch;
        if let Some(target) = judgement.target_worker_id.clone() {
            sub.target_worker_id = Some(target);
        }
        sub.owner_server_id = Some(server_id.to_string());
        sub.lease_heartbeat_at = judgement.lease_heartbeat_at;
        sub.leased_at = judgement.leased_at;
        sub.retry_count = judgement.retry_count;
        dispatches.push((sub, judgement.id, judgement.is_current));
    }

    if !dispatches.is_empty() || !plan.exhausted_rows.is_empty() {
        info!(
            server_id,
            redispatch = dispatches.len(),
            exhausted = plan.exhausted_rows.len(),
            "Stole stale deferred submission judgements"
        );
    }

    Ok(dispatches)
}

async fn claim_code_runs(
    state: &AppState,
    server_id: &str,
    lease_ttl_secs: u64,
    batch_size: u32,
    max_dispatch_retries: u32,
) -> anyhow::Result<Vec<code_run::Model>> {
    let txn = state.db.begin().await?;
    let rows = claim_rows(&txn, "code_run", lease_ttl_secs, batch_size).await?;
    let plan = ClaimPlan::from_rows(rows, max_dispatch_retries);

    if !plan.redispatch_ids.is_empty() {
        code_run::Entity::update_many()
            .col_expr(
                code_run::Column::OwnerServerId,
                sea_orm::sea_query::Expr::value(Some(server_id.to_string())),
            )
            .col_expr(
                code_run::Column::LeaseHeartbeatAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                code_run::Column::LeasedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                code_run::Column::RetryCount,
                sea_orm::sea_query::Expr::col(code_run::Column::RetryCount).add(1),
            )
            .col_expr(
                code_run::Column::JudgeEpoch,
                sea_orm::sea_query::Expr::col(code_run::Column::JudgeEpoch).add(1),
            )
            .col_expr(
                code_run::Column::Status,
                sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
            )
            .clear_judgement_columns()
            .filter(code_run::Column::Id.is_in(plan.redispatch_ids.clone()))
            .exec(&txn)
            .await?;
    }

    for row in &plan.exhausted_rows {
        let message = format!(
            "Code run dispatch retry limit exhausted after {max_dispatch_retries} attempts"
        );
        crate::consumers::mark_code_run_system_error_with_epoch(
            &txn,
            row.id,
            SubmissionDlqErrorCode::DISPATCH_RETRY_EXHAUSTED,
            &message,
            Some(row.judge_epoch),
        )
        .await?;
        code_run::Entity::update_many()
            .col_expr(
                code_run::Column::RetryCount,
                sea_orm::sea_query::Expr::col(code_run::Column::RetryCount).add(1),
            )
            .filter(code_run::Column::Id.eq(row.id))
            .filter(code_run::Column::JudgeEpoch.eq(row.judge_epoch))
            .exec(&txn)
            .await?;
    }

    let models = if plan.redispatch_ids.is_empty() {
        Vec::new()
    } else {
        code_run::Entity::find()
            .filter(code_run::Column::Id.is_in(plan.redispatch_ids.clone()))
            .all(&txn)
            .await?
    };

    txn.commit().await?;

    if !models.is_empty() || !plan.exhausted_rows.is_empty() {
        info!(
            server_id,
            redispatch = models.len(),
            exhausted = plan.exhausted_rows.len(),
            "Stole stale code-run leases"
        );
    }

    Ok(models)
}

async fn claim_rows(
    txn: &DatabaseTransaction,
    table: &str,
    lease_ttl_secs: u64,
    batch_size: u32,
) -> Result<Vec<ClaimedRow>, sea_orm::DbErr> {
    let threshold = chrono::Utc::now() - chrono::Duration::seconds(lease_ttl_secs as i64);
    let sql = format!(
        r#"SELECT id, judge_epoch, retry_count
           FROM {table}
           WHERE status IN ('Pending', 'Compiling', 'Running')
             AND (
               (owner_server_id IS NULL AND created_at < $1)
               OR (owner_server_id IS NOT NULL AND (lease_heartbeat_at IS NULL OR lease_heartbeat_at < $1))
             )
           ORDER BY created_at
           LIMIT $2
           FOR UPDATE SKIP LOCKED"#
    );
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [threshold.into(), i64::from(batch_size).into()],
        ))
        .await?;

    rows.into_iter().map(row_to_claimed).collect()
}

async fn claim_deferred_judgement_rows(
    txn: &DatabaseTransaction,
    lease_ttl_secs: u64,
    batch_size: u32,
) -> Result<Vec<ClaimedRow>, sea_orm::DbErr> {
    let threshold = chrono::Utc::now() - chrono::Duration::seconds(lease_ttl_secs as i64);
    let sql = r#"SELECT id, judge_epoch, retry_count
                 FROM submission_judgement
                 WHERE is_current = TRUE
                   AND is_finalized = FALSE
                   AND status IN ('Pending', 'Compiling', 'Running')
                   AND (
                     (owner_server_id IS NULL AND created_at < $1)
                     OR (owner_server_id IS NOT NULL AND (lease_heartbeat_at IS NULL OR lease_heartbeat_at < $1))
                   )
                 ORDER BY created_at
                 LIMIT $2
                 FOR UPDATE SKIP LOCKED"#;
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [threshold.into(), i64::from(batch_size).into()],
        ))
        .await?;

    rows.into_iter().map(row_to_claimed).collect()
}

fn row_to_claimed(row: QueryResult) -> Result<ClaimedRow, sea_orm::DbErr> {
    let id = row
        .try_get::<i32>("", "id")
        .map_err(|e| sea_orm::DbErr::Custom(e.to_string()))?;
    let retry_count = row
        .try_get::<i32>("", "retry_count")
        .map_err(|e| sea_orm::DbErr::Custom(e.to_string()))?;
    let judge_epoch = row
        .try_get::<i32>("", "judge_epoch")
        .map_err(|e| sea_orm::DbErr::Custom(e.to_string()))?;
    Ok(ClaimedRow {
        id,
        judge_epoch,
        retry_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    use common::storage::filesystem::FilesystemBlobStore;
    use plugin_core::config::PluginConfig;
    use plugin_core::error::PluginError;
    use plugin_core::host::HostFunctionRegistry;
    use plugin_core::i18n::I18nRegistry;
    use plugin_core::manifest::PluginManifest;
    use plugin_core::registry::PluginRegistry;
    use plugin_core::traits::{PluginInvoker, PluginManager};
    use sea_orm::{DatabaseBackend, MockDatabase};

    use crate::config::{
        AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
        MqAppConfig, ServerConfig, SubmissionConfig,
    };
    use crate::dispatcher::permits::DispatcherSemaphore;
    use crate::registry::{
        CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
        LanguageResolverRegistry, OperationBatches, OperationWaiters,
    };
    use crate::state::{AppState, RegistryState};

    struct NoopPluginManager {
        registry: PluginRegistry,
        config: PluginConfig,
        host_functions: HostFunctionRegistry,
        i18n: I18nRegistry,
    }

    #[async_trait::async_trait]
    impl PluginInvoker for NoopPluginManager {
        fn get_registry(&self) -> &PluginRegistry {
            &self.registry
        }

        fn get_config(&self) -> &PluginConfig {
            &self.config
        }

        async fn call_raw(
            &self,
            _plugin_id: &str,
            _func_name: &str,
            _input: Vec<u8>,
        ) -> Result<Vec<u8>, PluginError> {
            Err(PluginError::Internal(
                "NoopPluginManager cannot call plugins".into(),
            ))
        }
    }

    impl PluginManager for NoopPluginManager {
        fn get_host_functions(&self) -> &HostFunctionRegistry {
            &self.host_functions
        }

        fn get_i18n_registry(&self) -> &I18nRegistry {
            &self.i18n
        }

        fn resolve(&self, _manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
            None
        }
    }

    #[test]
    fn claim_plan_splits_retry_cap_before_increment() {
        let plan = ClaimPlan::from_rows(
            vec![
                ClaimedRow {
                    id: 1,
                    judge_epoch: 11,
                    retry_count: 0,
                },
                ClaimedRow {
                    id: 2,
                    judge_epoch: 12,
                    retry_count: 4,
                },
                ClaimedRow {
                    id: 3,
                    judge_epoch: 13,
                    retry_count: 5,
                },
            ],
            5,
        );

        assert_eq!(plan.redispatch_ids, vec![1, 2]);
        assert_eq!(
            plan.exhausted_rows,
            vec![ClaimedRow {
                id: 3,
                judge_epoch: 13,
                retry_count: 5,
            }]
        );
    }

    // METRICS_TEST_LOCK serializes tests that touch the global Prometheus
    // registry; holding it across the awaits below is the intended
    // serialization, not a deadlock risk - each `#[tokio::test]` runs on its own
    // runtime and the guard is never re-locked within a task.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn scan_once_skips_claims_when_dispatcher_queue_is_full() {
        let _guard = crate::metrics_test_lock();
        let dispatcher_permits = DispatcherSemaphore::new(true, 1, 0);
        let held_slot = dispatcher_permits
            .reserve()
            .expect("initial dispatcher slot should be available");

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let blob_dir = tempfile::tempdir().expect("create blob tempdir");
        let blob_store = Arc::new(
            FilesystemBlobStore::new(blob_dir.path().join("blobs"), 1024 * 1024)
                .await
                .expect("create blob store"),
        );
        let (metrics, prometheus_registry) =
            common::observability::init_metrics("broccoli-steal-test");

        let operation_batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches: EvaluateBatches = Arc::new(dashmap::DashMap::new());
        let contest_type_registry: ContestTypeRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let evaluator_registry: EvaluatorRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let checker_stage_registry: CheckerStageRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let language_resolver_registry: LanguageResolverRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        let state = AppState {
            plugins: Arc::new(NoopPluginManager {
                registry: Arc::new(RwLock::new(HashMap::new())),
                config: PluginConfig::default(),
                host_functions: HostFunctionRegistry::new(),
                i18n: I18nRegistry::new(),
            }),
            db,
            config: AppConfig {
                server: ServerConfig {
                    host: "127.0.0.1".to_string(),
                    port: 0,
                    cors: CorsConfig {
                        allow_origins: vec![],
                        max_age: 3600,
                    },
                    public_base_url: None,
                    frontend_dist: PathBuf::from("/srv/dist"),
                    trusted_proxies: vec![],
                    rate_limit_auth: false,
                    id: String::new(),
                    expects_multi_replica: false,
                    dispatcher_lease_steal_enabled: true,
                    dispatcher_semaphore_enabled: true,
                    dispatcher_concurrency: 1,
                    dispatcher_admission_queue_max: 0,
                    max_queued_submissions: 0,
                    lease_ttl_secs: 1,
                    lease_refresh_interval_secs: 10,
                    steal_scan_interval_secs: 15,
                    steal_batch_size: 8,
                    sweep_interval_secs: 300,
                    max_dispatch_retries: 5,
                    max_stuck_retries: 5,
                    max_system_error_retries: 50,
                    sweeper_dry_run: true,
                    operation_reaper_enabled: false,
                    operation_reaper_interval_secs: 30,
                    operation_reaper_grace_secs: 30,
                    operation_reaper_max_requeues_per_tick: 1000,
                    operation_reaper_dry_run: false,
                    cancel_primitive_enabled: false,
                    max_blocking_threads: None,
                    batch_evaluator_fanout_concurrency: 64,
                    operation_batch_publish_concurrency: 32,
                    healthz_listen: None,
                    healthz_worker_threads: 2,
                    claim_fiber_enabled: false,
                    claim_poll_interval_ms: 1000,
                    claim_batch_size: 32,
                    plugin_timer_tick_interval_secs: 1,
                    plugin_timer_lease_secs: 30,
                    plugin_timer_batch: 64,
                    plugin_timer_max_attempts: 5,
                },
                database: DatabaseConfig {
                    url: "mock://steal-test".to_string(),
                    max_connections: 1,
                    plugin_max_connections: 1,
                    plugin_privileged_max_connections: 1,
                    plugin_url: None,
                },
                auth: AuthConfig {
                    jwt_secret: "test-secret".to_string(),
                    secure_cookies: false,
                    login_failure_limit: 0,
                    login_failure_window_secs: 60,
                },
                plugin: PluginConfig::default(),
                submission: SubmissionConfig::default(),
                storage: BlobStoreConfig::default(),
                mq: MqAppConfig {
                    enabled: false,
                    ..Default::default()
                },
                observability: common::config::ObservabilityConfig::default(),
                batch_max_age_secs: 600,
                bootstrap: BootstrapConfig::default(),
            },
            mq: None,
            redis_client: None,
            blob_store,
            registries: RegistryState {
                contest_type_registry,
                evaluator_registry,
                checker_stage_registry,
                language_resolver_registry,
                operation_batches,
                operation_waiters,
                evaluate_batches,
                hook_registry: crate::hooks::new_shared_registry(),
            },
            device_codes: Arc::new(dashmap::DashMap::new()),
            metrics,
            prometheus_registry,
            dispatcher_permits,
            login_throttle: Arc::new(crate::utils::login_throttle::LoginThrottle::new(
                0,
                std::time::Duration::from_secs(60),
            )),
        };

        scan_once(state, "server-b", 1, 8, 5)
            .await
            .expect("full dispatcher queue should skip steal claims");

        drop(held_slot);
    }
}

/// Real-Postgres regression tests for the deferred-judgement steal path.
///
/// `MockDatabase` cannot exercise the epoch-gated `UPDATE ... WHERE` behavior
/// these tests assert, so each test boots a throwaway Postgres via
/// testcontainers and drives `claim_deferred_judgements` against a real schema.
#[cfg(test)]
mod deferred_steal_db_tests {
    use super::*;
    use crate::entity::{problem, user};
    use testcontainers::ContainerAsync;
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    /// Boot a fresh Postgres container and migrate the schema into it. The
    /// returned container guard must be kept alive for the duration of the test
    /// (its `Drop` tears the container down).
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

    /// Insert a `submission` (plus the `user`/`problem` it references) in the
    /// given status/epoch and return its id.
    async fn seed_submission(
        db: &DatabaseConnection,
        status: SubmissionStatus,
        judge_epoch: i32,
    ) -> i32 {
        let now = chrono::Utc::now();
        let u = user::ActiveModel {
            username: Set("steal-test-user".to_string()),
            password: Set("x".to_string()),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert user");
        let p = problem::ActiveModel {
            title: Set("steal-test-problem".to_string()),
            content: Set("c".to_string()),
            time_limit: Set(1000),
            memory_limit: Set(262_144),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert problem");
        let s = submission::ActiveModel {
            files: Set(serde_json::json!({})),
            language: Set("cpp".to_string()),
            user_id: Set(u.id),
            problem_id: Set(p.id),
            status: Set(status),
            judge_epoch: Set(judge_epoch),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert submission");
        s.id
    }

    /// Insert a current, unfinalized, Running judgement whose lease is an hour
    /// stale - exactly the shape the steal sweeper reclaims.
    async fn seed_stale_deferred_judgement(
        db: &DatabaseConnection,
        submission_id: i32,
        judge_epoch: i32,
        retry_count: i32,
    ) {
        let stale = chrono::Utc::now() - chrono::Duration::seconds(3600);
        submission_judgement::ActiveModel {
            submission_id: Set(submission_id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(false),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(judge_epoch),
            owner_server_id: Set(Some("dead-server".to_string())),
            lease_heartbeat_at: Set(Some(stale)),
            retry_count: Set(retry_count),
            created_at: Set(stale),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert deferred judgement");
    }

    /// Insert a current, unfinalized, Running judgement whose lease is FRESH
    /// (heartbeat = now) and owned by `owner` - the shape a restart leaves
    /// behind: still owned by the (now-dead) coordinator with a lease the
    /// lease fiber would keep refreshing.
    async fn seed_owned_fresh_judgement(
        db: &DatabaseConnection,
        submission_id: i32,
        judge_epoch: i32,
        retry_count: i32,
        owner: &str,
    ) {
        let now = chrono::Utc::now();
        submission_judgement::ActiveModel {
            submission_id: Set(submission_id),
            version: Set(1),
            is_current: Set(true),
            is_finalized: Set(false),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(judge_epoch),
            owner_server_id: Set(Some(owner.to_string())),
            lease_heartbeat_at: Set(Some(now)),
            retry_count: Set(retry_count),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert fresh owned judgement");
    }

    /// Stamp `owner`/fresh-lease onto the parent submission row, mirroring the
    /// ownership a live dispatch sets.
    async fn set_submission_owner(db: &DatabaseConnection, submission_id: i32, owner: &str) {
        let now = chrono::Utc::now();
        submission::Entity::update_many()
            .col_expr(
                submission::Column::OwnerServerId,
                sea_orm::sea_query::Expr::value(Some(owner.to_string())),
            )
            .col_expr(
                submission::Column::LeaseHeartbeatAt,
                sea_orm::sea_query::Expr::value(Some(now)),
            )
            .filter(submission::Column::Id.eq(submission_id))
            .exec(db)
            .await
            .expect("set submission owner");
    }

    async fn current_judgement(
        db: &DatabaseConnection,
        submission_id: i32,
    ) -> submission_judgement::Model {
        submission_judgement::Entity::find()
            .filter(submission_judgement::Column::SubmissionId.eq(submission_id))
            .one(db)
            .await
            .expect("query judgement")
            .expect("judgement exists")
    }

    async fn reload_submission(db: &DatabaseConnection, id: i32) -> submission::Model {
        submission::Entity::find_by_id(id)
            .one(db)
            .await
            .expect("query submission")
            .expect("submission exists")
    }

    /// The core regression: when a stale deferred judgement is re-dispatched,
    /// its epoch is bumped - and the parent `submission.judge_epoch` MUST be
    /// bumped in lockstep, or the epoch-gated finalize later writes 0 rows and
    /// the submission hangs in `Running` forever.
    #[tokio::test]
    async fn deferred_redispatch_advances_parent_submission_epoch() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        seed_stale_deferred_judgement(&db, sub_id, 0, 0).await;

        let claimed = claim_deferred_judgements(&db, "live-server", 60, 10, 5)
            .await
            .expect("claim deferred judgements");
        assert_eq!(
            claimed.len(),
            1,
            "the stale deferred judgement is reclaimed"
        );

        let judgement = current_judgement(&db, sub_id).await;
        assert_eq!(judgement.judge_epoch, 1, "judgement epoch is bumped to 1");

        let submission = reload_submission(&db, sub_id).await;
        assert_eq!(
            submission.judge_epoch, judgement.judge_epoch,
            "submission epoch must stay in lockstep with the judgement"
        );
        assert_eq!(submission.judge_epoch, 1, "submission epoch advances to 1");
        assert_eq!(
            submission.status,
            SubmissionStatus::Pending,
            "a re-dispatched submission returns to Pending"
        );
        assert_eq!(
            submission.owner_server_id.as_deref(),
            Some("live-server"),
            "the stealing server takes ownership of the submission row"
        );
    }

    /// Regression: the deferred steal REUSES the same judgement id and only
    /// bumps its epoch, so the failed attempt's `test_case_result` rows must be
    /// wiped on re-dispatch. The submission read selects test_case_result by
    /// `judgement_id` alone (no `judge_epoch` filter), so a surviving prior-epoch
    /// row would surface alongside the re-judge's rows as duplicated per-testcase
    /// results under one judgement.
    #[tokio::test]
    async fn deferred_redispatch_wipes_stale_testcase_results() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        seed_stale_deferred_judgement(&db, sub_id, 0, 0).await;
        let jid = current_judgement(&db, sub_id).await.id;

        // The prior attempt left a per-testcase row under this judgement id at
        // the pre-bump epoch.
        test_case_result::ActiveModel {
            submission_id: Set(sub_id),
            judgement_id: Set(Some(jid)),
            judge_epoch: Set(0),
            verdict: Set(common::Verdict::Accepted),
            score: Set(1.0),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("seed stale test_case_result");

        let claimed = claim_deferred_judgements(&db, "live-server", 60, 10, 5)
            .await
            .expect("claim deferred judgements");
        assert_eq!(
            claimed.len(),
            1,
            "the stale deferred judgement is reclaimed"
        );

        let judgement = current_judgement(&db, sub_id).await;
        assert_eq!(
            judgement.id, jid,
            "the deferred steal reuses the same judgement id (not a fresh row)"
        );
        assert_eq!(judgement.judge_epoch, 1, "judgement epoch is bumped to 1");

        // The stale prior-attempt row must be gone so the re-judge starts clean
        // and the submission view can't show duplicated per-testcase results.
        let remaining = test_case_result::Entity::find()
            .filter(test_case_result::Column::JudgementId.eq(Some(jid)))
            .all(&db)
            .await
            .expect("query test_case_result");
        assert!(
            remaining.is_empty(),
            "the failed attempt's per-testcase results are wiped on deferred re-dispatch"
        );
    }

    /// When a deferred judgement exhausts its retry budget it is finalized as
    /// `SystemError`; the parent submission must also leave `Running` instead of
    /// hanging.
    #[tokio::test]
    async fn exhausted_deferred_judgement_finalizes_parent_submission() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        // retry_count + 1 > max_dispatch_retries(5) => exhausted.
        seed_stale_deferred_judgement(&db, sub_id, 0, 5).await;

        let claimed = claim_deferred_judgements(&db, "live-server", 60, 10, 5)
            .await
            .expect("claim deferred judgements");
        assert_eq!(
            claimed.len(),
            0,
            "an exhausted judgement is not re-dispatched"
        );

        let judgement = current_judgement(&db, sub_id).await;
        assert_eq!(judgement.status, SubmissionStatus::SystemError);
        assert!(judgement.is_finalized, "exhausted judgement is finalized");

        let submission = reload_submission(&db, sub_id).await;
        assert_eq!(
            submission.status,
            SubmissionStatus::SystemError,
            "exhausting a deferred judgement must terminalize the parent submission"
        );
    }

    /// Startup-recovery regression: after a coordinator restart, its OWN
    /// in-flight judgement is still owned by `server-1` with a FRESH lease (the
    /// lease fiber keeps refreshing it), so the steal sweeper can never reclaim
    /// it and the submission hangs in `Running` forever.
    /// `recover_orphaned_leases` must release that lease so the steal then
    /// reclaims and re-dispatches it.
    #[tokio::test]
    async fn recover_orphaned_leases_makes_own_fresh_inflight_stealable() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        seed_owned_fresh_judgement(&db, sub_id, 0, 0, "server-1").await;
        set_submission_owner(&db, sub_id, "server-1").await;

        // Before recovery: the steal cannot reclaim it (own, fresh lease) -
        // this is the restart-orphan hang.
        let pre = claim_deferred_judgements(&db, "server-1", 60, 10, 5)
            .await
            .expect("pre-recovery claim");
        assert_eq!(
            pre.len(),
            0,
            "fresh-leased own judgement is not stealable yet"
        );

        // Recovery releases the lease on both the judgement and its parent.
        let (subs, _cr, judg) = recover_orphaned_leases(&db, "server-1")
            .await
            .expect("recover orphaned leases");
        assert_eq!(judg, 1, "the current unfinalized judgement is released");
        assert_eq!(subs, 1, "the parent submission lease is released");

        // After recovery: the steal reclaims and re-dispatches it.
        let post = claim_deferred_judgements(&db, "server-1", 60, 10, 5)
            .await
            .expect("post-recovery claim");
        assert_eq!(post.len(), 1, "released judgement is now stealable");

        // Lockstep is preserved end-to-end.
        let judgement = current_judgement(&db, sub_id).await;
        let submission = reload_submission(&db, sub_id).await;
        assert_eq!(
            submission.judge_epoch, judgement.judge_epoch,
            "submission epoch stays in lockstep after recovery + steal"
        );
        assert_eq!(
            submission.status,
            SubmissionStatus::Pending,
            "the recovered submission returns to Pending for re-judging"
        );
    }

    /// Fix regression: a NON-current in-flight deferred rejudge owned by the
    /// restarting server must ALSO be released by boot recovery. Previously the
    /// `is_current = TRUE` filter left it hanging until the 6-hour ceiling. The
    /// release must stay strictly scoped to `owner_server_id = <self>`.
    #[tokio::test]
    async fn recover_orphaned_leases_releases_own_non_current_inflight() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        let now = chrono::Utc::now();
        submission_judgement::ActiveModel {
            submission_id: Set(sub_id),
            version: Set(1),
            is_current: Set(false),
            is_finalized: Set(false),
            status: Set(SubmissionStatus::Running),
            judge_epoch: Set(0),
            owner_server_id: Set(Some("server-1".to_string())),
            lease_heartbeat_at: Set(Some(now)),
            retry_count: Set(0),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert non-current owned judgement");

        let (_subs, _cr, judg) = recover_orphaned_leases(&db, "server-1")
            .await
            .expect("recover orphaned leases");
        assert_eq!(
            judg, 1,
            "a non-current in-flight judgement owned by self is released"
        );

        let judgement = current_judgement(&db, sub_id).await;
        assert_eq!(
            judgement.owner_server_id.as_deref(),
            Some("server-1#orphaned"),
            "the released judgement is retagged with the orphan sentinel"
        );
        assert!(
            judgement.lease_heartbeat_at.is_none(),
            "the released judgement's heartbeat is nulled so the steal reclaims it"
        );
    }

    /// Fix regression: the replacement `is_current = TRUE` judgement inserted
    /// when a submission lease is stolen must be stamped with the stealing
    /// server's ownership AND a live lease heartbeat - mirroring the
    /// deferred-steal (`claim_deferred_judgements`) and
    /// `requeue_judgement_for_system_error_retry` inserts. A NULL-owner /
    /// NULL-heartbeat row is never refreshed by the lease fiber (owner = self)
    /// and gets re-claimed by the steal sweeper mid-re-judge.
    #[tokio::test]
    async fn stolen_submission_judgement_insert_stamps_owner_and_heartbeat() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;

        let txn = db.begin().await.expect("begin txn");
        open_stolen_submission_judgements(&txn, "live-server", &[sub_id])
            .await
            .expect("open stolen submission judgement");
        txn.commit().await.expect("commit txn");

        let judgement = current_judgement(&db, sub_id).await;
        assert!(judgement.is_current, "the inserted judgement is current");
        assert_eq!(
            judgement.owner_server_id.as_deref(),
            Some("live-server"),
            "the inserted judgement is owned by the stealing server"
        );
        assert!(
            judgement.lease_heartbeat_at.is_some(),
            "the inserted judgement carries a live lease heartbeat so the lease \
             fiber refreshes it instead of the steal sweeper re-claiming it"
        );
    }

    /// Fix regression: a reclaimed deferred judgement is always current
    /// (`claim_deferred_judgement_rows` filters `is_current = TRUE`), so the
    /// steal must surface `is_current` so `scan_once` re-dispatches it with
    /// `fire_after_judging = true`. Otherwise a re-judged live verdict never
    /// fires its after-judging hooks (scoreboard recompute, notifications).
    #[tokio::test]
    async fn deferred_redispatch_reports_current_for_fire_after_judging() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        seed_stale_deferred_judgement(&db, sub_id, 0, 0).await;

        let claimed = claim_deferred_judgements(&db, "live-server", 60, 10, 5)
            .await
            .expect("claim deferred judgements");
        assert_eq!(
            claimed.len(),
            1,
            "the stale deferred judgement is reclaimed"
        );
        let (_sub, _judgement_id, is_current) = &claimed[0];
        assert!(
            *is_current,
            "a reclaimed current judgement must re-dispatch with fire_after_judging = true"
        );
    }

    /// Multi-replica safety: a sibling replica's in-flight work (different
    /// `server_id`, live lease) must be left strictly alone when THIS server
    /// runs its boot recovery.
    #[tokio::test]
    async fn recover_orphaned_leases_leaves_other_servers_alone() {
        let (_pg, db) = start_pg().await;
        let sub_id = seed_submission(&db, SubmissionStatus::Running, 0).await;
        seed_owned_fresh_judgement(&db, sub_id, 0, 0, "server-2").await;
        set_submission_owner(&db, sub_id, "server-2").await;

        let (subs, _cr, judg) = recover_orphaned_leases(&db, "server-1")
            .await
            .expect("recover orphaned leases");
        assert_eq!(judg, 0, "another server's judgement is untouched");
        assert_eq!(subs, 0, "another server's submission is untouched");

        let judgement = current_judgement(&db, sub_id).await;
        assert_eq!(
            judgement.owner_server_id.as_deref(),
            Some("server-2"),
            "sibling replica retains ownership"
        );
        assert!(
            judgement.lease_heartbeat_at.is_some(),
            "sibling replica's lease is left intact"
        );
    }
}
