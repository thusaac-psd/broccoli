//! SystemError-retry reaper.
//!
//! A `SystemError` verdict reflects a SYSTEM-side condition - the interactive
//! manager failed to compile under contention, a contestant compile step hit a
//! sandbox error, an operation result was lost - never the contestant's own
//! code. The contest invariant forbids such a condition from standing as the
//! contestant's verdict.
//!
//! This fiber reconciles from the durable terminal state rather than any
//! in-flight control-flow site, which is what makes it path-agnostic: it does
//! not matter whether the submission was judged synchronously (batch) or via the
//! detached operation path (interactive) - both finalize the same
//! `submission_judgement` row, and the reaper sees it. It periodically scans for
//! current, finalized judgements whose terminal outcome is a `SystemError` in any
//! of its shapes - plugin-finalized (`status == Judged` + `verdict ==
//! SystemError`) or stuck-handler-terminal / dispatch-exhaustion (`status ==
//! SystemError`) - with retry budget remaining, requeues them to a fresh epoch
//! (in lockstep with the parent submission, results wiped - see
//! `crate::services::submission_dispatch::requeue_judgement_for_system_error_retry`)
//! and re-dispatches them. Bounded by `max_system_error_retries`; only when the
//! budget is exhausted does the `SystemError` stand (a genuinely broken problem).
//!
//! The reaper lives in a top-level fiber (not in the dispatch call graph), so the
//! re-dispatch is a plain `dispatch_to_plugin_with_judgement` call with no
//! recursion. Re-dispatch passes `fire_after_judging = true` so a retried
//! submission still fires its after-judging hooks when it finally completes.

use std::time::Duration;

use common::{SubmissionStatus, Verdict};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::entity::submission_judgement;
use crate::state::AppState;

/// How often to scan for retryable SystemError verdicts. Kept short so a
/// transient failure recovers quickly, but the scan is a cheap indexed query.
const SCAN_INTERVAL_SECS: u64 = 5;
/// Maximum judgements requeued per scan. Deliberately small: re-judges hold a
/// dispatcher permit for their whole duration, so a large batch would starve
/// live contest submissions. A backlog instead drains a few per tick as spare
/// capacity allows - slow is fine, the invariant is only that no SystemError is
/// ever abandoned.
const SCAN_BATCH_SIZE: u64 = 8;

pub async fn run(
    state: AppState,
    server_id: String,
    max_system_error_retries: u32,
    mut cancel: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(SCAN_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = scan_once(&state, &server_id, max_system_error_retries).await {
                    error!(server_id = %server_id, error = %e, "SystemError-retry scan failed");
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

async fn scan_once(
    state: &AppState,
    server_id: &str,
    max_system_error_retries: u32,
) -> anyhow::Result<()> {
    let max = std::cmp::min(max_system_error_retries, i32::MAX as u32) as i32;

    let candidates = find_retryable_system_errors(&state.db, max, SCAN_BATCH_SIZE).await?;

    if candidates.is_empty() {
        return Ok(());
    }

    let mut requeued = 0u32;
    for judgement in candidates {
        // Respect dispatcher admission control: reserve a permit BEFORE
        // re-dispatching, exactly as the steal does. Re-judges then share the
        // fleet's bounded judging capacity instead of fanning out unbounded - a
        // backlog of SystemErrors drains gradually rather than overwhelming the
        // workers (which can wedge interactive judging). When the queue is full,
        // stop this scan; the remaining candidates are retried next tick (no
        // SystemError is ever abandoned).
        let slot = match state.dispatcher_permits.reserve() {
            Ok(slot) => slot,
            Err(e) => {
                warn!(
                    server_id = %server_id,
                    requeued,
                    retry_after_secs = e.retry_after_secs,
                    "SystemError-retry scan paused: dispatcher queue full; remaining retries deferred"
                );
                break;
            }
        };

        // `requeue_*` re-reads under its own transaction and is epoch-gated, so a
        // concurrent reaper (multi-replica) or a steal that already advanced this
        // judgement loses harmlessly (returns None). No owner scoping is needed.
        match crate::services::submission_dispatch::requeue_judgement_for_system_error_retry(
            &state.db,
            judgement.submission_id,
            judgement.id,
            judgement.judge_epoch,
            server_id,
            max_system_error_retries,
        )
        .await
        {
            Ok(Some(resubmission)) => {
                requeued += 1;
                warn!(
                    server_id = %server_id,
                    submission_id = resubmission.id,
                    judgement_id = judgement.id,
                    new_epoch = resubmission.judge_epoch,
                    retry_count = resubmission.retry_count,
                    "Re-judging a SystemError verdict (system condition, not the contestant's code)"
                );
                let state = state.clone();
                let judgement_id = judgement.id;
                slot.spawn("system_error_retry", async move {
                    crate::services::submission_dispatch::dispatch_submission_to_plugin_with_judgement(
                        state,
                        resubmission,
                        Some(judgement_id),
                        true,
                    )
                    .await;
                });
            }
            // `slot` is dropped here, returning the reserved permit.
            Ok(None) => {}
            Err(e) => {
                error!(
                    server_id = %server_id,
                    submission_id = judgement.submission_id,
                    judgement_id = judgement.id,
                    error = %e,
                    "Failed to requeue SystemError judgement; leaving the verdict in place"
                );
            }
        }
    }

    if requeued > 0 {
        info!(server_id = %server_id, requeued, "Re-dispatched SystemError verdicts for bounded retry");
    }

    Ok(())
}

/// Current, finalized judgements whose terminal outcome is a `SystemError` in
/// ANY shape, with retry budget remaining. Path-agnostic: whatever finalized
/// them (synchronous batch, detached interactive, stuck-handler terminal, or
/// dispatch-exhaustion) wrote one of these rows. `retry_count < max` keeps a
/// genuinely broken problem terminalizing after a finite number of attempts.
///
/// Two shapes are selected, and only these:
/// * `status == Judged AND verdict == SystemError` - plugin-finalized;
/// * `status == SystemError` - stuck-handler terminal / dispatch-exhaustion
///   (verdict typically NULL).
///
/// A contestant `CompileError` lands as `status == CompilationError` (never
/// `SystemError`), so it is never selected; nor are own-code verdicts
/// (`status == Judged` with WA/TLE/MLE/RE), non-current, or unfinalized rows.
async fn find_retryable_system_errors(
    db: &sea_orm::DatabaseConnection,
    max_retry_count: i32,
    batch_size: u64,
) -> Result<Vec<submission_judgement::Model>, sea_orm::DbErr> {
    submission_judgement::Entity::find()
        .filter(submission_judgement::Column::IsCurrent.eq(true))
        .filter(submission_judgement::Column::IsFinalized.eq(true))
        .filter(
            Condition::any()
                .add(
                    Condition::all()
                        .add(submission_judgement::Column::Status.eq(SubmissionStatus::Judged))
                        .add(submission_judgement::Column::Verdict.eq(Verdict::SystemError)),
                )
                .add(submission_judgement::Column::Status.eq(SubmissionStatus::SystemError)),
        )
        .filter(submission_judgement::Column::RetryCount.lt(max_retry_count))
        .order_by_asc(submission_judgement::Column::Id)
        .limit(batch_size)
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{problem, submission, user};
    use sea_orm::{ActiveModelTrait, DatabaseConnection, Set};
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

    async fn seed_submission(db: &DatabaseConnection) -> i32 {
        let now = chrono::Utc::now();
        let u = user::ActiveModel {
            username: Set("reaper-user".to_string()),
            display_name: Set("reaper-user".to_string()),
            password: Set("x".to_string()),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert user");
        let p = problem::ActiveModel {
            title: Set("reaper-problem".to_string()),
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
        submission::ActiveModel {
            files: Set(serde_json::json!({})),
            language: Set("cpp".to_string()),
            user_id: Set(u.id),
            problem_id: Set(p.id),
            status: Set(SubmissionStatus::Judged),
            judge_epoch: Set(0),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert submission")
        .id
    }

    #[allow(clippy::too_many_arguments)]
    async fn seed_judgement(
        db: &DatabaseConnection,
        submission_id: i32,
        is_current: bool,
        is_finalized: bool,
        status: SubmissionStatus,
        verdict: Option<Verdict>,
        retry_count: i32,
    ) -> i32 {
        let now = chrono::Utc::now();
        submission_judgement::ActiveModel {
            submission_id: Set(submission_id),
            version: Set(1),
            is_current: Set(is_current),
            is_finalized: Set(is_finalized),
            status: Set(status),
            verdict: Set(verdict),
            judge_epoch: Set(0),
            retry_count: Set(retry_count),
            finalized_at: Set(is_finalized.then(chrono::Utc::now)),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert judgement")
        .id
    }

    /// The reaper selects ONLY current, finalized, `Judged`+`SystemError`
    /// judgements with retry budget - and nothing else. This is the safety
    /// boundary that keeps contestant verdicts (CompileError, WrongAnswer) and
    /// exhausted/superseded rows untouched.
    #[tokio::test]
    async fn reaper_selects_only_retryable_system_errors() {
        let (_pg, db) = start_pg().await;

        // SHOULD be selected: current, finalized, Judged + SystemError, budget.
        let s_ok = seed_submission(&db).await;
        let j_ok = seed_judgement(
            &db,
            s_ok,
            true,
            true,
            SubmissionStatus::Judged,
            Some(Verdict::SystemError),
            0,
        )
        .await;

        // NOT selected - exhausted (retry_count == max).
        let s_exhausted = seed_submission(&db).await;
        seed_judgement(
            &db,
            s_exhausted,
            true,
            true,
            SubmissionStatus::Judged,
            Some(Verdict::SystemError),
            5,
        )
        .await;

        // NOT selected - contestant CompileError (status=CompilationError).
        let s_ce = seed_submission(&db).await;
        seed_judgement(
            &db,
            s_ce,
            true,
            true,
            SubmissionStatus::CompilationError,
            None,
            0,
        )
        .await;

        // NOT selected - contestant WrongAnswer.
        let s_wa = seed_submission(&db).await;
        seed_judgement(
            &db,
            s_wa,
            true,
            true,
            SubmissionStatus::Judged,
            Some(Verdict::WrongAnswer),
            0,
        )
        .await;

        // NOT selected - a superseded (non-current) SystemError.
        let s_old = seed_submission(&db).await;
        seed_judgement(
            &db,
            s_old,
            false,
            true,
            SubmissionStatus::Judged,
            Some(Verdict::SystemError),
            0,
        )
        .await;

        // NOT selected - still judging (not finalized).
        let s_running = seed_submission(&db).await;
        seed_judgement(
            &db,
            s_running,
            true,
            false,
            SubmissionStatus::Running,
            None,
            0,
        )
        .await;

        // SHOULD be selected - stuck-handler terminal / dispatch-exhaustion shape:
        // status=SystemError with a NULL verdict, current, finalized, budget. The
        // reaper previously matched only Judged+SystemError, abandoning this.
        let s_stuck = seed_submission(&db).await;
        let j_stuck = seed_judgement(
            &db,
            s_stuck,
            true,
            true,
            SubmissionStatus::SystemError,
            None,
            0,
        )
        .await;

        let found = find_retryable_system_errors(&db, 5, 64)
            .await
            .expect("query candidates");
        let ids: Vec<i32> = found.iter().map(|j| j.id).collect();
        assert_eq!(
            ids,
            vec![j_ok, j_stuck],
            "both the Judged+SystemError and the status=SystemError shapes (with budget) are selected"
        );
    }
}
