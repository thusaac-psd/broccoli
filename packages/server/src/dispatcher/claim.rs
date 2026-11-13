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
use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use common::SubmissionStatus;
use opentelemetry::KeyValue;
use sea_orm::sqlx::postgres::PgListener;

use crate::services::plugin_config_cache::PluginConfigCache;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use tokio::sync::{Notify, watch};
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

/// Least time between two claim passes that a notification starts, so a burst
/// of submissions is claimed in batches instead of one query per submission.
const COALESCE: Duration = Duration::from_millis(10);

/// Longest wait before listening again after the notification connection
/// fails; the poll tick keeps claiming meanwhile.
const LISTEN_RETRY_MAX: Duration = Duration::from_secs(30);

/// Long-running fiber loop until the cancel channel fires.
///
/// A claim pass runs when a claim notification arrives (see
/// `judging::ClaimWake`) or, as a fallback, `interval_ms` after the last
/// pass. Wakes that arrive during a pass fold into one more pass, and passes
/// repeat while each fills its batch and room is left, so a backlog drains at
/// claim speed rather than one batch per tick. Replicas all wake on the same
/// notification; `SKIP LOCKED` in the claim splits the work between them.
pub async fn run(state: AppState, server_id: String, cancel: watch::Receiver<bool>) {
    claim_loop(state, server_id, FALLBACK_TICK, cancel).await
}

/// [`run`] with another fallback tick, so a test can show that notifications
/// alone drive claims.
#[cfg(any(test, feature = "test-hooks"))]
pub async fn run_with_fallback_tick(
    state: AppState,
    server_id: String,
    fallback_tick: Duration,
    cancel: watch::Receiver<bool>,
) {
    claim_loop(state, server_id, fallback_tick, cancel).await
}

async fn claim_loop(
    state: AppState,
    server_id: String,
    fallback_tick: Duration,
    mut cancel: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(fallback_tick);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!(server_id = %server_id, "Claim fiber started");

    let wake = Arc::new(Notify::new());
    let saturated = Arc::new(AtomicBool::new(false));
    tokio::spawn(listen(
        state.config.database.url.clone(),
        server_id.clone(),
        state.plugin_config_cache.clone(),
        wake.clone(),
        saturated.clone(),
        cancel.clone(),
    ));

    // Live worker slots, refreshed every `SLOTS_REFRESH` from heartbeats
    // (a Redis SCAN, too costly to repeat every pass). Sizes the in-flight
    // cap so it scales with the cluster; see [`effective_cap`].
    let mut live_slots: u64 = 0;
    let mut slots_checked: Option<Instant> = None;
    let mut last_pass: Option<Instant> = None;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = wake.notified() => {
                if let Some(at) = last_pass {
                    tokio::time::sleep_until((at + COALESCE).into()).await;
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    info!(server_id = %server_id, "Claim fiber shutting down");
                    return;
                }
                continue;
            }
        }
        common::deadline::bounded(
            "dispatcher.claim",
            common::deadline::BACKGROUND_STEP,
            async {
                if slots_checked.is_none_or(|t| t.elapsed() >= SLOTS_REFRESH) {
                    live_slots = live_worker_slots(&state).await;
                    state.live_worker_slots.store(live_slots, Ordering::Relaxed);
                    slots_checked = Some(Instant::now());
                }
                let cap = effective_cap(live_slots);
                loop {
                    match claim_once(&state, &server_id, CLAIM_BATCH, cap).await {
                        Ok(pass) => {
                            saturated.store(pass.waits_for_room(), Ordering::Relaxed);
                            if !pass.more_may_wait() || pass.held_back {
                                break;
                            }
                        }
                        Err(e) => {
                            error!(server_id = %server_id, error = %e, "Claim pass failed");
                            break;
                        }
                    }
                }
            },
        )
        .await;
        last_pass = Some(Instant::now());
        interval.reset();
    }
}

/// Turns claim notifications into wakes of the claim fiber until `cancel`
/// fires, and passes plugin config changes to `config_cache`. A `Queued` notification always wakes it; a freed one only when it
/// is for this server and the last pass was held back for room. Notifications
/// sent while this is not listening are lost, so every (re)connect wakes the
/// fiber too.
async fn listen(
    url: String,
    server_id: String,
    config_cache: PluginConfigCache,
    wake: Arc<Notify>,
    saturated: Arc<AtomicBool>,
    mut cancel: watch::Receiver<bool>,
) {
    const LISTEN_RETRY_MIN: Duration = Duration::from_millis(500);
    let mut retry = LISTEN_RETRY_MIN;
    loop {
        let started = Instant::now();
        tokio::select! {
            result = listen_session(&url, &server_id, &config_cache, &wake, &saturated) => {
                let Err(error) = result;
                warn!(
                    server_id = %server_id,
                    error = %error,
                    retry_secs = retry.as_secs_f64(),
                    "Claim notifications unavailable; claiming on the poll interval until they return"
                );
            }
            _ = cancelled(&mut cancel) => return,
        }
        tokio::select! {
            _ = tokio::time::sleep(retry) => {}
            _ = cancelled(&mut cancel) => return,
        }
        // A session that held up for a while starts the backoff over.
        retry = if started.elapsed() > LISTEN_RETRY_MAX {
            LISTEN_RETRY_MIN
        } else {
            std::cmp::min(retry * 2, LISTEN_RETRY_MAX)
        };
    }
}

/// One listening connection; ends only when it fails and cannot reconnect.
async fn listen_session(
    url: &str,
    server_id: &str,
    config_cache: &PluginConfigCache,
    wake: &Notify,
    saturated: &AtomicBool,
) -> Result<Infallible, sea_orm::sqlx::Error> {
    let mut listener = PgListener::connect(url).await?;
    listener.listen(judging::CLAIM_CHANNEL).await?;
    // Config changes may have gone unannounced while nobody listened.
    config_cache.clear();
    wake.notify_one();
    loop {
        let Some(notification) = listener.try_recv().await? else {
            // Reconnected after a lost connection.
            config_cache.clear();
            wake.notify_one();
            continue;
        };
        if config_cache.observe_notification(notification.payload()) {
            continue;
        }
        let wanted = match judging::ClaimWake::parse(notification.payload()) {
            Some(judging::ClaimWake::Queued) => true,
            Some(judging::ClaimWake::Freed { server_id: owner }) => {
                owner == server_id && saturated.load(Ordering::Relaxed)
            }
            None => false,
        };
        if wanted {
            wake.notify_one();
        }
    }
}

/// Resolves once `cancel` fires or its sender is gone.
async fn cancelled(cancel: &mut watch::Receiver<bool>) {
    while !*cancel.borrow_and_update() {
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

/// What one claim pass did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClaimPass {
    /// Judgements claimed, and the room they had.
    pub judgements: (u32, u32),
    /// Code runs claimed, and the room they had.
    pub code_runs: (u32, u32),
    /// The in-flight cap or dispatcher admission gave the pass less room than
    /// a full batch.
    pub held_back: bool,
}

impl ClaimPass {
    /// Some kind of unit filled all of its room, so more of it may be queued.
    pub fn more_may_wait(&self) -> bool {
        let filled = |(claimed, room): (u32, u32)| room > 0 && claimed == room;
        filled(self.judgements) || filled(self.code_runs)
    }

    /// Queued work may be waiting for in-flight room: the pass was held back
    /// and either had no room at all or used all it had.
    pub fn waits_for_room(&self) -> bool {
        let no_room = self.judgements.1 == 0 && self.code_runs.1 == 0;
        self.held_back && (no_room || self.more_may_wait())
    }
}

/// One claim pass: judgements within the in-flight cap, then code runs.
/// Errors are returned to the caller so the fiber loop can log; whatever is
/// still `Queued` is claimed by the next pass.
///
/// **Crash-recovery contract:** dispatch runs detached after the claim
/// committed. If this server crashes in between, the claimed rows stay in
/// flight owned by a dead server. They are requeued when this `server_id`
/// boots again ([`judging::release_leases`]), or, with lease/steal on, by a
/// peer's steal scanner once their lease lapses, or by the stuck detector.
///
/// `pub` so integration tests can drive one pass directly, like
/// `plugin_timer::tick_once`.
pub async fn claim_once(
    state: &AppState,
    server_id: &str,
    batch_size: u32,
    max_in_flight: u32,
) -> anyhow::Result<ClaimPass> {
    let in_flight = if max_in_flight == 0 {
        0
    } else {
        judging::count_in_flight(&state.db, server_id).await?
    };
    // The contest types this replica can judge right now; see
    // `dispatcher::prefer_local_handler`.
    let contest_types = super::local_contest_types(state).await;

    // Judgements and code runs share the in-flight cap; judgements go first.
    let budget = claim_budget(batch_size, max_in_flight, in_flight);
    let mut held_back = budget < batch_size;
    let slots = reserve_slots(state, budget);
    held_back |= (slots.len() as u32) < budget;
    let judgement_room = slots.len() as u32;
    let mut claimed_judgements = 0;
    if !slots.is_empty() {
        let claimed =
            judging::claim_judgements(&state.db, server_id, judgement_room, &contest_types).await?;
        // Before any claimed unit is judged: config committed before its
        // submission is then visible (see `services::plugin_config_cache`).
        if let Some(epoch) = claimed.plugin_config_epoch {
            state.plugin_config_cache.observe_epoch(epoch);
        }
        let claimed = claimed.units;
        claimed_judgements = claimed.len() as u32;
        record_judgement_claim_transitions(state, &claimed, state.clock.now()).await;
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

    let code_run_budget = if max_in_flight == 0 {
        batch_size
    } else {
        budget.saturating_sub(claimed_judgements)
    };
    let slots = reserve_slots(state, code_run_budget);
    held_back |= (slots.len() as u32) < code_run_budget;
    let code_run_room = slots.len() as u32;
    let mut claimed_code_runs = 0;
    if !slots.is_empty() {
        let claimed =
            judging::claim_code_runs(&state.db, server_id, code_run_room, &contest_types).await?;
        if let Some(epoch) = claimed.plugin_config_epoch {
            state.plugin_config_cache.observe_epoch(epoch);
        }
        let claimed = claimed.units;
        claimed_code_runs = claimed.len() as u32;
        record_code_run_claim_transitions(state, &claimed, state.clock.now()).await;
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

    Ok(ClaimPass {
        judgements: (claimed_judgements, judgement_room),
        code_runs: (claimed_code_runs, code_run_room),
        held_back,
    })
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

/// The in-flight cap before any worker heartbeat is seen.
const IN_FLIGHT_FLOOR: u64 = 256;

/// The claim fiber's fallback tick. Claims are driven by notifications; this
/// only covers ones lost while the listening connection was down.
const FALLBACK_TICK: Duration = Duration::from_secs(1);

/// Rows one claim statement takes per table. Passes repeat while each fills
/// its batch and room is left, so this bounds a statement, not how fast a
/// backlog drains.
const CLAIM_BATCH: u32 = 32;

/// How many judgements and code runs this server judges at once (claimed,
/// not finished): 8 per live worker slot, at least [`IN_FLIGHT_FLOOR`]. A
/// burst past it stays `Queued` in the database, claimable by any server.
fn effective_cap(live_worker_slots: u64) -> u32 {
    let scaled = live_worker_slots.saturating_mul(IN_FLIGHT_PER_WORKER_SLOT);
    u32::try_from(std::cmp::max(scaled, IN_FLIGHT_FLOOR)).unwrap_or(u32::MAX)
}

/// Sum of `max_concurrency` over live workers. 0 when Redis is unavailable,
/// which leaves the floor.
async fn live_worker_slots(state: &AppState) -> u64 {
    crate::handlers::system::read_workers(state)
        .await
        .iter()
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
#[allow(clippy::disallowed_methods)] // Fixtures are stamped with real time.
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
    fn a_pass_repeats_while_it_fills_its_room_and_waits_for_room_when_held_back() {
        let pass = |judgements, code_runs, held_back| ClaimPass {
            judgements,
            code_runs,
            held_back,
        };
        // A full batch of judgements: more may be queued.
        assert!(pass((32, 32), (0, 32), false).more_may_wait());
        // Code runs filled theirs.
        assert!(pass((3, 32), (32, 32), false).more_may_wait());
        // Both kinds left room: the queue is drained.
        assert!(!pass((3, 32), (0, 32), false).more_may_wait());
        // Held back by the cap with all its room used, or with none at all:
        // waiting for room.
        assert!(pass((6, 6), (0, 0), true).waits_for_room());
        assert!(pass((0, 0), (0, 0), true).waits_for_room());
        // Held back but drained, or not held back: not waiting.
        assert!(!pass((2, 6), (0, 4), true).waits_for_room());
        assert!(!pass((32, 32), (0, 0), false).waits_for_room());
    }

    #[test]
    fn the_cap_grows_with_live_worker_capacity_and_never_below_the_floor() {
        // Small cluster (4 workers x 1 slot): the floor applies.
        assert_eq!(effective_cap(4), 256);
        // No heartbeat data: still the floor.
        assert_eq!(effective_cap(0), 256);
        // Large cluster (800 slots): scales past the floor.
        assert_eq!(effective_cap(800), 6_400);
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
