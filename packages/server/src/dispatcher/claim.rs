//! Per-server background fiber that moves queued work into flight. The api
//! persists new work as `Queued` and returns; every recovery requeues it
//! (see [`crate::services::judging`]). This fiber is the only dispatcher:
//! it claims queued judgements and code runs (`Queued` → `Pending`, owned by
//! this server, lease stamped with the database clock) and spawns their
//! dispatch, within the server's in-flight cap and dispatcher admission.
//!
//! The fiber lives for the lifetime of the api process. When
//! `server.claim_fiber_enabled` is `false`, it never starts - meaning
//! `Queued` rows have no one to claim them and will accumulate until
//! the flag is flipped back on. That escape hatch exists only to stop
//! a server from dispatching new work during incident response;
//! production should always run with the fiber enabled.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::SubmissionStatus;
use opentelemetry::KeyValue;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::dispatcher::permits::DispatcherReservation;
use crate::entity::problem;
use crate::services::judging::{self, ClaimedCodeRun, ClaimedJudgement};
use crate::state::AppState;

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

    // Live worker slots, refreshed every `SLOTS_REFRESH` from heartbeats
    // (a Redis SCAN, too costly to repeat every tick). Sizes the in-flight
    // cap so it scales with the cluster; see [`effective_cap`].
    let mut live_slots: u64 = 0;
    let mut slots_checked: Option<std::time::Instant> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                common::deadline::bounded("dispatcher.claim", common::deadline::BACKGROUND_STEP, async {
                    if max_in_flight > 0
                        && slots_checked.is_none_or(|t| t.elapsed() >= SLOTS_REFRESH)
                    {
                        live_slots = live_worker_slots(&state).await;
                        slots_checked = Some(std::time::Instant::now());
                    }
                    let cap = effective_cap(max_in_flight, live_slots);
                    if let Err(e) = claim_once(&state, &server_id, batch_size, cap).await {
                        error!(server_id = %server_id, error = %e, "Claim fiber tick failed");
                    }
                })
                .await;
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

/// One claim cycle: judgements within the in-flight cap, then code runs.
/// Errors are returned to the caller so the fiber loop can log; whatever is
/// still `Queued` is claimed next tick.
///
/// **Crash-recovery contract:** dispatch runs detached after the claim
/// committed. If this server crashes in between, the claimed rows stay in
/// flight owned by a dead server. They are requeued when this `server_id`
/// boots again ([`judging::release_leases`]), or, with lease/steal on, by a
/// peer's steal scanner once their lease lapses, or by the stuck detector.
///
/// `pub` so integration tests can drive one cycle directly, like
/// `plugin_timer::tick_once`.
pub async fn claim_once(
    state: &AppState,
    server_id: &str,
    batch_size: u32,
    max_in_flight: u32,
) -> anyhow::Result<()> {
    let in_flight = if max_in_flight == 0 {
        0
    } else {
        judging::count_in_flight(&state.db, server_id).await?
    };
    // The contest types this replica can judge right now; see
    // `dispatcher::prefer_local_handler`.
    let contest_types = super::local_contest_types(state).await;

    let slots = reserve_slots(state, claim_budget(batch_size, max_in_flight, in_flight));
    if !slots.is_empty() {
        let claimed =
            judging::claim_judgements(&state.db, server_id, slots.len() as u32, &contest_types)
                .await?;
        record_judgement_claim_transitions(state, &claimed, Utc::now()).await;
        if !claimed.is_empty() {
            info!(
                server_id,
                claimed = claimed.len(),
                "Claimed queued judgements"
            );
        }
        for (claimed, slot) in claimed.into_iter().zip(slots) {
            let state = state.clone();
            slot.spawn("dispatch_judgement", async move {
                crate::services::submission_dispatch::dispatch_judgement(state, claimed).await;
            });
        }
    }

    let slots = reserve_slots(state, batch_size);
    if !slots.is_empty() {
        let claimed =
            judging::claim_code_runs(&state.db, server_id, slots.len() as u32, &contest_types)
                .await?;
        record_code_run_claim_transitions(state, &claimed, Utc::now()).await;
        if !claimed.is_empty() {
            info!(
                server_id,
                claimed = claimed.len(),
                "Claimed queued code runs"
            );
        }
        for (code_run, slot) in claimed.into_iter().zip(slots) {
            let state = state.clone();
            slot.spawn("dispatch_code_run", async move {
                crate::services::code_run_dispatch::dispatch_code_run_to_plugin(state, code_run)
                    .await;
            });
        }
    }

    Ok(())
}

/// Up to `n` dispatcher admission slots; fewer when the dispatcher queue is
/// full, so a claim never takes work it cannot start.
fn reserve_slots(state: &AppState, n: u32) -> Vec<DispatcherReservation> {
    let mut slots = Vec::with_capacity(n as usize);
    for _ in 0..n {
        match state.dispatcher_permits.reserve() {
            Ok(slot) => slots.push(slot),
            Err(e) => {
                warn!(
                    reserved = slots.len(),
                    retry_after_secs = e.retry_after_secs,
                    "Claim limited because the dispatcher queue is full"
                );
                break;
            }
        }
    }
    slots
}

/// How often the claim loop re-reads live worker capacity.
const SLOTS_REFRESH: Duration = Duration::from_secs(15);

/// In-flight submissions allowed per live worker slot. A submission keeps at
/// most a few operations in flight, so more than one per busy slot adds no
/// throughput; 8 leaves ample slack for claim latency and callbacks, so the
/// cap can never become the bottleneck before the workers do.
const IN_FLIGHT_PER_WORKER_SLOT: u64 = 8;

/// The in-flight cap in force: the configured value is a floor that scales up
/// with the cluster's live worker slots, so a large deployment is never
/// throttled by a default nobody tuned. `configured == 0` disables the cap.
fn effective_cap(configured: u32, live_worker_slots: u64) -> u32 {
    if configured == 0 {
        return 0;
    }
    let scaled = live_worker_slots.saturating_mul(IN_FLIGHT_PER_WORKER_SLOT);
    u32::try_from(std::cmp::max(scaled, u64::from(configured))).unwrap_or(u32::MAX)
}

/// Sum of `max_concurrency` over live (non-stale) workers, from heartbeats.
/// 0 when heartbeats are unavailable, which leaves the configured floor.
async fn live_worker_slots(state: &AppState) -> u64 {
    crate::handlers::system::read_workers(state)
        .await
        .iter()
        .filter(|w| !w.stale)
        .map(|w| u64::from(w.max_concurrency.unwrap_or(1)))
        .sum()
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

async fn record_code_run_claim_transitions(
    state: &AppState,
    models: &[ClaimedCodeRun],
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
    claimed: &[ClaimedJudgement],
    transitioned_at: DateTime<Utc>,
) {
    if claimed.is_empty() {
        return;
    }
    let problem_types =
        problem_types_by_problem_id(state, claimed.iter().map(|c| c.submission.problem_id)).await;

    for c in claimed {
        let problem_type = problem_types
            .get(&c.submission.problem_id)
            .map(String::as_str)
            .unwrap_or("unknown");
        record_submission_state_transition(
            &state.metrics,
            &SubmissionStatus::Queued,
            &SubmissionStatus::Pending,
            problem_type,
            transition_duration_seconds(c.judgement.created_at, transitioned_at),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_cap_grows_with_live_worker_capacity_and_never_below_the_floor() {
        // Small cluster (4 workers x 1 slot): the configured floor applies.
        assert_eq!(effective_cap(256, 4), 256);
        // No heartbeat data: still the floor.
        assert_eq!(effective_cap(256, 0), 256);
        // Large cluster (100 workers x 8 slots): scales past the floor, so an
        // untuned default cannot throttle it.
        assert_eq!(effective_cap(256, 800), 6_400);
        // Disabled stays disabled.
        assert_eq!(effective_cap(0, 800), 0);
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
