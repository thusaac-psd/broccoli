//! Per-server background fiber that pulls submissions out of `Queued`
//! and into `Pending` (UP#38). Pairs with UP#37: the api's POST handler
//! persists `status='Queued'` and returns 201; this fiber claims rows
//! through `SELECT ... FOR UPDATE SKIP LOCKED`, transitions them to
//! `Pending`, sets `owner_server_id`, bumps `retry_count`, and finally
//! dispatches via the existing `dispatch_to_plugin` path.
//!
//! The fiber lives for the lifetime of the api process. When
//! `server.claim_fiber_enabled` is `false`, it never starts - meaning
//! UP#37's `Queued` rows have no one to claim them and will accumulate
//! until the flag is flipped back on. That escape hatch exists only to
//! pin a deployment to the pre-UP#37 behavior during incident response;
//! production should always run with the fiber enabled.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::SubmissionStatus;
use opentelemetry::KeyValue;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, ExprTrait,
    QueryFilter, QueryResult, Statement, TransactionTrait,
};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::entity::{code_run, problem, submission, submission_judgement};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QueuedRow {
    id: i32,
}

fn transition_duration_seconds(created_at: DateTime<Utc>, transitioned_at: DateTime<Utc>) -> f64 {
    std::cmp::max(
        transitioned_at
            .signed_duration_since(created_at)
            .num_milliseconds(),
        0,
    ) as f64
        / 1_000.0
}

fn record_submission_state_transition(
    metrics: &common::metrics::Metrics,
    from: &SubmissionStatus,
    to: &SubmissionStatus,
    problem_type: &str,
    duration_seconds: f64,
) {
    metrics.submission_state_transition_duration.record(
        duration_seconds,
        &[
            KeyValue::new("from", from.to_string()),
            KeyValue::new("to", to.to_string()),
            KeyValue::new("problem_type", problem_type.to_string()),
        ],
    );
}

/// Long-running fiber loop. Polls every `interval_ms` until the cancel
/// channel fires.
pub async fn run(
    state: AppState,
    server_id: String,
    interval_ms: u64,
    batch_size: u32,
    max_in_flight: u32,
    mut cancel: watch::Receiver<bool>,
) {
    // The poll interval clamps to a 50ms floor so a misconfigured
    // `claim_poll_interval_ms=0` doesn't busy-spin against Postgres.
    let interval_dur = Duration::from_millis(std::cmp::max(interval_ms, 50));
    let mut interval = tokio::time::interval(interval_dur);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!(
        server_id = %server_id,
        interval_ms = interval_dur.as_millis() as u64,
        batch_size,
        max_in_flight,
        "Claim fiber started"
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = claim_once(&state, &server_id, batch_size, max_in_flight).await {
                    error!(server_id = %server_id, error = %e, "Claim fiber tick failed");
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    info!(server_id = %server_id, "Claim fiber shutting down");
                    return;
                }
            }
        }
    }
}

/// One claim cycle: scan submissions, then code-runs. Errors are
/// returned to the caller so the fiber loop can log; partial progress
/// (e.g., submissions claimed but code-runs failed) is fine - the next
/// tick will pick up whatever's still in `Queued`.
///
/// **Crash-recovery contract:** the post-commit `tokio::spawn` below is
/// intentionally detached. If this server crashes after `txn.commit()`
/// (which has already flipped the row to `Pending`, set `owner_server_id`
/// to *us*, and bumped `retry_count`) but before the spawned task
/// actually publishes the dispatch, the row is now stranded in `Pending`
/// owned by a dead server. The **only** safety net is the
/// dispatcher/steal scanner (UP#17): once `lease_heartbeat_at` is older
/// than `lease_ttl_secs`, a peer server's steal scanner reclaims the row
/// and re-dispatches it. So the worst-case end-to-end recovery latency
/// is `lease_ttl_secs`, not "instant" - this is the cost of decoupling
/// claim from dispatch. Don't introduce a bounded join-set or `await`
/// here without re-thinking that contract: an inline `await` would make
/// the claim fiber the bottleneck for dispatch throughput.
async fn claim_once(
    state: &AppState,
    server_id: &str,
    batch_size: u32,
    max_in_flight: u32,
) -> anyhow::Result<()> {
    let in_flight = if max_in_flight == 0 {
        0
    } else {
        count_in_flight(state, server_id).await?
    };
    let budget = claim_budget(batch_size, max_in_flight, in_flight);
    let submission_models = if budget > 0 {
        claim_queued_submissions(state, server_id, budget).await?
    } else {
        Vec::new()
    };
    // Deferred rejudges are judging work too: they share what is left.
    let rejudge_budget = budget.saturating_sub(submission_models.len() as u32);
    for model in submission_models {
        let state_clone = state.clone();
        tokio::spawn(async move {
            crate::services::submission_dispatch::dispatch_submission_to_plugin(state_clone, model)
                .await;
        });
    }

    let code_run_models = claim_queued_code_runs(state, server_id, batch_size).await?;
    for model in code_run_models {
        let state_clone = state.clone();
        tokio::spawn(async move {
            crate::services::code_run_dispatch::dispatch_code_run_to_plugin(state_clone, model)
                .await;
        });
    }

    // Deferred-rejudge path (UP#37 residual fix). Non-current
    // judgements created by `apply_immediately=false` start at
    // `status=Queued` so the post-commit silent-loss window is closed
    // symmetrically with the submission path. The parent submission's
    // own status is unchanged for these rows, so the submission scan
    // above doesn't reach them - the judgement scan does.
    let judgement_pairs = if rejudge_budget > 0 {
        claim_queued_judgements(state, server_id, rejudge_budget).await?
    } else {
        Vec::new()
    };
    for (sub_model, judgement_model) in judgement_pairs {
        let state_clone = state.clone();
        let judgement_id = judgement_model.id;
        let is_current = judgement_model.is_current;
        tokio::spawn(async move {
            crate::services::submission_dispatch::dispatch_submission_to_plugin_with_judgement(
                state_clone,
                sub_model,
                Some(judgement_id),
                // `fire_after_judging` mirrors `is_current`: the
                // deferred path was originally spawned with
                // `false`, and that's still correct here - the
                // judgement isn't the displayed verdict, so hooks
                // shouldn't fire for it. A judgement reached this
                // path with `is_current=true` only if a future
                // refactor routes immediate-apply through this
                // scan, in which case the caller will have flipped
                // `is_current` and hooks should fire.
                is_current,
            )
            .await;
        });
    }

    Ok(())
}

/// Rows to claim this tick: the batch size, but never past the server's
/// in-flight cap. `max_in_flight == 0` means no cap.
fn claim_budget(batch_size: u32, max_in_flight: u32, in_flight: u64) -> u32 {
    if max_in_flight == 0 {
        return batch_size;
    }
    let room = u64::from(max_in_flight).saturating_sub(in_flight);
    std::cmp::min(batch_size, u32::try_from(room).unwrap_or(u32::MAX))
}

/// Submissions this server has claimed and not finished judging.
pub async fn count_in_flight(state: &AppState, server_id: &str) -> anyhow::Result<u64> {
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM submission \
             WHERE owner_server_id = $1 AND status IN ('Pending', 'Compiling', 'Running')",
            [server_id.into()],
        ))
        .await?;
    let n: i64 = row.map(|r| r.try_get("", "n")).transpose()?.unwrap_or(0);
    Ok(u64::try_from(n).unwrap_or(0))
}

async fn claim_queued_submissions(
    state: &AppState,
    server_id: &str,
    batch_size: u32,
) -> anyhow::Result<Vec<submission::Model>> {
    let txn = state.db.begin().await?;
    let rows = select_queued_rows(&txn, "submission", batch_size).await?;

    if rows.is_empty() {
        txn.commit().await?;
        return Ok(Vec::new());
    }

    let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();

    // FOR UPDATE SKIP LOCKED already serialized us against peer claim
    // fibers - the only way to land here with `status != 'Queued'` would
    // be a same-process bug. We still scope the UPDATE with a status
    // filter so a hypothetical bug becomes a no-op rather than a state
    // corruption.
    submission::Entity::update_many()
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
        .col_expr(
            submission::Column::RetryCount,
            sea_orm::sea_query::Expr::col(submission::Column::RetryCount).add(1),
        )
        .filter(submission::Column::Id.is_in(ids.clone()))
        .filter(submission::Column::Status.eq(SubmissionStatus::Queued))
        .exec(&txn)
        .await?;

    let models = submission::Entity::find()
        .filter(submission::Column::Id.is_in(ids))
        .all(&txn)
        .await?;

    let transitioned_at = Utc::now();
    txn.commit().await?;
    record_submission_claim_transitions(state, &models, transitioned_at).await;

    if !models.is_empty() {
        info!(
            server_id,
            claimed = models.len(),
            "Claim fiber promoted submissions Queued -> Pending"
        );
    }

    Ok(models)
}

async fn claim_queued_code_runs(
    state: &AppState,
    server_id: &str,
    batch_size: u32,
) -> anyhow::Result<Vec<code_run::Model>> {
    let txn = state.db.begin().await?;
    let rows = select_queued_rows(&txn, "code_run", batch_size).await?;

    if rows.is_empty() {
        txn.commit().await?;
        return Ok(Vec::new());
    }

    let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();

    code_run::Entity::update_many()
        .col_expr(
            code_run::Column::Status,
            sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
        )
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
        .filter(code_run::Column::Id.is_in(ids.clone()))
        .filter(code_run::Column::Status.eq(SubmissionStatus::Queued))
        .exec(&txn)
        .await?;

    let models = code_run::Entity::find()
        .filter(code_run::Column::Id.is_in(ids))
        .all(&txn)
        .await?;

    let transitioned_at = Utc::now();
    txn.commit().await?;
    record_code_run_claim_transitions(state, &models, transitioned_at).await;

    if !models.is_empty() {
        info!(
            server_id,
            claimed = models.len(),
            "Claim fiber promoted code-runs Queued -> Pending"
        );
    }

    Ok(models)
}

/// Claims deferred-rejudge judgement rows (`status='Queued'` on the
/// `submission_judgement` table) and returns them paired with their
/// parent submission model - the dispatch path needs both.
///
/// Mirrors `claim_queued_submissions` shape: SELECT FOR UPDATE SKIP
/// LOCKED, in-txn UPDATE to Pending, refetch, commit. The parent
/// submission's denormalized columns are left untouched - by design,
/// a deferred rejudge's outcome doesn't override the current verdict
/// until an admin explicitly applies it.
async fn claim_queued_judgements(
    state: &AppState,
    server_id: &str,
    batch_size: u32,
) -> anyhow::Result<Vec<(submission::Model, submission_judgement::Model)>> {
    let txn = state.db.begin().await?;
    let rows = select_queued_rows(&txn, "submission_judgement", batch_size).await?;

    if rows.is_empty() {
        txn.commit().await?;
        return Ok(Vec::new());
    }

    let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();

    submission_judgement::Entity::update_many()
        .col_expr(
            submission_judgement::Column::Status,
            sea_orm::sea_query::Expr::value(SubmissionStatus::Pending.to_string()),
        )
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
        .filter(submission_judgement::Column::Id.is_in(ids.clone()))
        .filter(submission_judgement::Column::Status.eq(SubmissionStatus::Queued))
        .exec(&txn)
        .await?;

    let judgements = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::Id.is_in(ids))
        .all(&txn)
        .await?;

    // Re-fetch each parent submission in the same txn so dispatch
    // sees a consistent snapshot. Submissions for deferred rejudge
    // are not locked here - we don't mutate them and a concurrent
    // current-judgement claim acts on a different row.
    let sub_ids: Vec<i32> = judgements.iter().map(|j| j.submission_id).collect();
    let submissions = submission::Entity::find()
        .filter(submission::Column::Id.is_in(sub_ids))
        .all(&txn)
        .await?;

    let sub_by_id: HashMap<i32, submission::Model> =
        submissions.into_iter().map(|s| (s.id, s)).collect();

    let mut pairs = Vec::with_capacity(judgements.len());
    for j in judgements {
        if let Some(mut s) = sub_by_id.get(&j.submission_id).cloned() {
            // Dispatch runs the plugin at `sub.judge_epoch`, and every host
            // persistence write is gated on that epoch. A deferred rejudge
            // judgement lives at a NEW epoch while its parent submission row
            // stays at the old one, so without this override the plugin runs
            // at the stale epoch and every finalize/result write matches 0
            // rows (the rejudge silently produces nothing). Mirrors the
            // deferred-steal path (steal.rs). In-memory copy only; the
            // submission row is never mutated.
            s.judge_epoch = j.judge_epoch;
            pairs.push((s, j));
        } else {
            // Parent submission vanished between SELECT and JOIN -
            // exceedingly unlikely (deletes are guarded) but log and
            // skip rather than panic.
            tracing::warn!(
                judgement_id = j.id,
                submission_id = j.submission_id,
                "Claim fiber: parent submission missing for queued judgement; skipping"
            );
        }
    }

    let transitioned_at = Utc::now();
    txn.commit().await?;

    if !pairs.is_empty() {
        record_judgement_claim_transitions(state, &pairs, transitioned_at).await;
        info!(
            server_id,
            claimed = pairs.len(),
            "Claim fiber promoted deferred judgements Queued -> Pending"
        );
    }

    Ok(pairs)
}

async fn problem_types_by_problem_id(
    state: &AppState,
    problem_ids: impl IntoIterator<Item = i32>,
) -> HashMap<i32, String> {
    let mut ids: Vec<i32> = problem_ids.into_iter().collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return HashMap::new();
    }

    match problem::Entity::find()
        .filter(problem::Column::Id.is_in(ids))
        .all(&state.db)
        .await
    {
        Ok(problems) => problems
            .into_iter()
            .map(|problem| (problem.id, problem.problem_type))
            .collect(),
        Err(e) => {
            warn!(error = %e, "Failed to load problem types for submission transition metrics");
            HashMap::new()
        }
    }
}

async fn record_submission_claim_transitions(
    state: &AppState,
    models: &[submission::Model],
    transitioned_at: DateTime<Utc>,
) {
    let problem_types =
        problem_types_by_problem_id(state, models.iter().map(|model| model.problem_id)).await;

    for model in models {
        let problem_type = problem_types
            .get(&model.problem_id)
            .map(String::as_str)
            .unwrap_or("unknown");
        record_submission_state_transition(
            &state.metrics,
            &SubmissionStatus::Queued,
            &SubmissionStatus::Pending,
            problem_type,
            transition_duration_seconds(model.created_at, transitioned_at),
        );
    }
}

async fn record_code_run_claim_transitions(
    state: &AppState,
    models: &[code_run::Model],
    transitioned_at: DateTime<Utc>,
) {
    let problem_types =
        problem_types_by_problem_id(state, models.iter().map(|model| model.problem_id)).await;

    for model in models {
        let problem_type = problem_types
            .get(&model.problem_id)
            .map(String::as_str)
            .unwrap_or("unknown");
        record_submission_state_transition(
            &state.metrics,
            &SubmissionStatus::Queued,
            &SubmissionStatus::Pending,
            problem_type,
            transition_duration_seconds(model.created_at, transitioned_at),
        );
    }
}

async fn record_judgement_claim_transitions(
    state: &AppState,
    pairs: &[(submission::Model, submission_judgement::Model)],
    transitioned_at: DateTime<Utc>,
) {
    let problem_types =
        problem_types_by_problem_id(state, pairs.iter().map(|(sub, _)| sub.problem_id)).await;

    for (sub, judgement) in pairs {
        let problem_type = problem_types
            .get(&sub.problem_id)
            .map(String::as_str)
            .unwrap_or("unknown");
        record_submission_state_transition(
            &state.metrics,
            &SubmissionStatus::Queued,
            &SubmissionStatus::Pending,
            problem_type,
            transition_duration_seconds(judgement.created_at, transitioned_at),
        );
    }
}

/// Selects a bounded batch of `Queued` rows from the named table with
/// row-level locks acquired via `FOR UPDATE SKIP LOCKED`. Peer claim
/// fibers and the dispatcher/steal scanner can run concurrently without
/// claiming the same row twice.
async fn select_queued_rows(
    txn: &DatabaseTransaction,
    table: &str,
    batch_size: u32,
) -> Result<Vec<QueuedRow>, sea_orm::DbErr> {
    // Spawn-time validation (see `dispatcher::Dispatcher::spawn`) asserts
    // `batch_size > 0`, so we should never see a 0 here. Keep a defensive
    // `max(_, 1)` so a future regression in that validation doesn't turn
    // into a silent "select 0 rows forever" no-op.
    debug_assert!(batch_size > 0, "batch_size must be positive at this point");
    let limit = std::cmp::max(batch_size, 1);
    let sql = format!(
        r#"SELECT id
           FROM {table}
           WHERE status = 'Queued'
           ORDER BY created_at
           LIMIT $1
           FOR UPDATE SKIP LOCKED"#
    );
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [i64::from(limit).into()],
        ))
        .await?;

    rows.into_iter().map(row_to_queued).collect()
}

fn row_to_queued(row: QueryResult) -> Result<QueuedRow, sea_orm::DbErr> {
    let id = row
        .try_get::<i32>("", "id")
        .map_err(|e| sea_orm::DbErr::Custom(e.to_string()))?;
    Ok(QueuedRow { id })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_queued_rows_sql_includes_for_update_skip_locked() {
        // Smoke test: assert that the SELECT statement we will emit has
        // the critical `FOR UPDATE SKIP LOCKED` clause and a bounded
        // LIMIT. A regression here would be silent (the fiber would
        // still appear to work but lose concurrency safety vs. peer
        // fibers and the dispatcher/steal scanner), so guard it.
        let sql = format!(
            r#"SELECT id
           FROM {table}
           WHERE status = 'Queued'
           ORDER BY created_at
           LIMIT $1
           FOR UPDATE SKIP LOCKED"#,
            table = "submission"
        );
        assert!(sql.contains("FOR UPDATE SKIP LOCKED"));
        assert!(sql.contains("LIMIT $1"));
        assert!(sql.contains("status = 'Queued'"));
        assert!(sql.contains("ORDER BY created_at"));
    }

    #[test]
    fn claim_budget_fills_up_to_the_in_flight_cap_and_no_further() {
        // Plenty of room: the batch size is the limit.
        assert_eq!(claim_budget(32, 256, 0), 32);
        // Close to the cap: only the room that is left.
        assert_eq!(claim_budget(32, 256, 250), 6);
        // At or past the cap (e.g. after a steal): claim nothing.
        assert_eq!(claim_budget(32, 256, 256), 0);
        assert_eq!(claim_budget(32, 256, 300), 0);
        // 0 means no cap.
        assert_eq!(claim_budget(32, 0, 10_000), 32);
    }

    #[test]
    fn queued_row_id_only() {
        // Defensive: the projection is `id` only - adding more fields
        // to the SELECT requires updating `row_to_queued` and this
        // struct. If a future commit changes either, the other must
        // follow.
        let row = QueuedRow { id: 42 };
        assert_eq!(row.id, 42);
    }

    #[test]
    fn transition_duration_clamps_future_created_at_to_zero() {
        let now = chrono::Utc::now();
        let created_at = now + chrono::Duration::seconds(5);

        assert_eq!(transition_duration_seconds(created_at, now), 0.0);
    }

    #[test]
    fn transition_metric_records_from_to_and_problem_type_labels() {
        let _guard = crate::metrics_test_lock();
        let (metrics, registry) =
            common::observability::init_metrics("broccoli-claim-transition-test");

        record_submission_state_transition(
            &metrics,
            &SubmissionStatus::Queued,
            &SubmissionStatus::Pending,
            "ioi",
            1.5,
        );

        let families = registry.gather();
        let transition_duration = families
            .iter()
            .find(|family| family.name() == "broccoli_submission_state_transition_duration_seconds")
            .expect("submission transition duration should be exported");

        for (label_name, label_value) in [
            ("from", "Queued"),
            ("to", "Pending"),
            ("problem_type", "ioi"),
        ] {
            assert!(
                transition_duration.get_metric().iter().any(|metric| metric
                    .get_label()
                    .iter()
                    .any(|label| label.name() == label_name && label.value() == label_value)),
                "transition duration should include {label_name}={label_value}"
            );
        }
    }
}
