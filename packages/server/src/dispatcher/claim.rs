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

use std::convert::Infallible;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::SubmissionStatus;
use opentelemetry::KeyValue;
use sea_orm::sqlx::postgres::PgListener;

use crate::services::plugin_config_cache::PluginConfigCache;
use tokio::sync::{Notify, watch};
use tracing::{error, info, warn};

use crate::dispatcher::permits::DispatcherReservation;
use crate::services::judging;
use crate::state::AppState;

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
                // Sized from the live worker slots so the cap scales with
                // the cluster; see [`in_flight_cap`].
                let cap = in_flight_cap(&state.worker_slots);
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
    // Outside `common::db::DatabaseConnection`: the listener only LISTENs
    // and binds no values.
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
    /// A pass that claimed nothing because nothing could run the work now:
    /// the queue is unreachable, or no live worker takes work. Held back with
    /// no room, so the fiber stops this round and tries again on its next wake
    /// or tick.
    pub const CANNOT_RUN: Self = Self {
        judgements: (0, 0),
        code_runs: (0, 0),
        held_back: true,
    };

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
/// Nothing is claimed while the message queue is unreachable, or while live
/// workers are known and none of them takes work (see
/// [`crate::dispatcher::fanout::WorkerSlots::none_accepting`]): the work stays
/// `Queued`, visibly waiting, instead of waiting unseen at the queue.
/// Errors are returned to the caller so the fiber loop can log; whatever is
/// still `Queued` is claimed by the next pass.
///
/// Each kind is claimed by one statement that returns everything its
/// dispatch needs, and the dispatch starts right after it with nothing
/// fallible in between (see [`start_claimed`]).
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
    // Claimed work is dispatched through the queue: while Redis is down it
    // would only fail and burn retries, so it stays `Queued` for a server
    // that can reach the queue, or for this one once it can again.
    if state.worker_slots.none_accepting() || !state.mq.check_reachable().await {
        return Ok(ClaimPass::CANNOT_RUN);
    }
    // The contest types this replica can judge right now; see
    // `dispatcher::prefer_local_handler`.
    let contest_types = super::local_contest_types(state).await;
    let in_flight_cap = (max_in_flight > 0).then_some(max_in_flight);

    // Judgements and code runs share the in-flight cap; judgements go first.
    // The judgement claim applies the cap, counting what is in flight in the
    // same statement, and says what room the cap left.
    let slots = reserve_slots(state, batch_size);
    let mut held_back = (slots.len() as u32) < batch_size;
    if slots.is_empty() {
        return Ok(ClaimPass {
            judgements: (0, 0),
            code_runs: (0, 0),
            held_back,
        });
    }
    let claimed = judging::claim_judgements(
        &state.db,
        server_id,
        judging::ClaimLimit {
            rows: slots.len() as u32,
            in_flight_cap,
        },
        &contest_types,
    )
    .await?;
    // The room the cap left for this whole pass.
    let budget = claimed
        .cap_room
        .map_or(batch_size, |room| room.min(batch_size));
    held_back |= budget < batch_size;
    let judgement_room = budget.min(slots.len() as u32);
    let claimed_judgements = claimed.units.len() as u32;
    start_claimed(
        state,
        server_id,
        "dispatch_judgement",
        claimed,
        slots,
        crate::services::submission_dispatch::dispatch_judgement,
    );

    // Without a cap each kind has a batch of its own.
    let code_run_budget = match in_flight_cap {
        Some(_) => budget.saturating_sub(claimed_judgements),
        None => batch_size,
    };
    let slots = reserve_slots(state, code_run_budget);
    held_back |= (slots.len() as u32) < code_run_budget;
    let code_run_room = slots.len() as u32;
    let claimed = judging::claim_code_runs(
        &state.db,
        server_id,
        judging::ClaimLimit {
            rows: code_run_room,
            // Counted by the judgement claim: `code_run_budget` is in it.
            in_flight_cap: None,
        },
        &contest_types,
    )
    .await?;
    let claimed_code_runs = claimed.units.len() as u32;
    start_claimed(
        state,
        server_id,
        "dispatch_code_run",
        claimed,
        slots,
        crate::services::code_run_dispatch::dispatch_code_run_to_plugin,
    );

    Ok(ClaimPass {
        judgements: (claimed_judgements, judgement_room),
        code_runs: (claimed_code_runs, code_run_room),
        held_back,
    })
}

/// Starts the dispatch of every unit of a claim, each on one of `slots`
/// (the claim took no more units than there are slots), after recording the
/// claim's metrics. Nothing here can fail or wait: the claim committed, and
/// every unit it took must reach its dispatch.
fn start_claimed<T, F, Fut>(
    state: &AppState,
    server_id: &str,
    label: &'static str,
    claimed: judging::Claimed<T>,
    slots: Vec<DispatcherReservation>,
    dispatch: F,
) where
    T: Send + 'static,
    F: Fn(AppState, T) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    // Before any claimed unit is judged: config committed before its
    // submission is then visible (see `services::plugin_config_cache`).
    if let Some(epoch) = claimed.plugin_config_epoch {
        state.plugin_config_cache.observe_epoch(epoch);
    }
    if claimed.units.is_empty() {
        return;
    }
    info!(
        server_id,
        label,
        claimed = claimed.units.len(),
        "Claimed queued work"
    );
    debug_assert!(claimed.units.len() <= slots.len());
    for (claim, slot) in claimed.units.into_iter().zip(slots) {
        record_submission_state_transition(
            &state.metrics,
            &SubmissionStatus::Queued,
            &SubmissionStatus::Pending,
            &claim.problem_type,
            claim.queued_secs,
        );
        slot.spawn(label, dispatch(state.clone(), claim.unit));
    }
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

/// In-flight submissions allowed per live worker slot. A submission keeps at
/// most a few operations in flight, so more than one per busy slot adds no
/// throughput; 8 leaves ample slack for claim latency and callbacks, so the
/// cap can never become the bottleneck before the workers do.
const IN_FLIGHT_PER_WORKER_SLOT: u64 = 8;

/// The in-flight cap before any worker heartbeat is seen, and the least it
/// ever is on one server.
const IN_FLIGHT_FLOOR: u64 = 256;

/// The claim fiber's fallback tick. Claims are driven by notifications; this
/// only covers ones lost while the listening connection was down.
const FALLBACK_TICK: Duration = Duration::from_secs(1);

/// Rows one claim statement takes per table. Passes repeat while each fills
/// its batch and room is left, so this bounds a statement, not how fast a
/// backlog drains.
const CLAIM_BATCH: u32 = 32;

/// How many judgements and code runs this server judges at once (claimed,
/// not finished): its share of 8 per live worker slot across the fleet, at
/// least [`IN_FLIGHT_FLOOR`]. N replicas together take no more than the
/// fleet's workers can serve, and a burst past the cap stays `Queued` in the
/// database, claimable by any server.
pub(crate) fn in_flight_cap(slots: &crate::dispatcher::fanout::WorkerSlots) -> u32 {
    effective_cap(slots.get(), slots.servers())
}

fn effective_cap(live_worker_slots: u64, live_servers: u64) -> u32 {
    let scaled = live_worker_slots
        .saturating_mul(IN_FLIGHT_PER_WORKER_SLOT)
        .div_ceil(live_servers.max(1));
    u32::try_from(std::cmp::max(scaled, IN_FLIGHT_FLOOR)).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Fixtures are stamped with real time.
mod tests {
    use super::*;

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
        assert_eq!(effective_cap(4, 1), 256);
        // No heartbeat data: still the floor.
        assert_eq!(effective_cap(0, 1), 256);
        // Large cluster (800 slots): scales past the floor.
        assert_eq!(effective_cap(800, 1), 6_400);
    }

    #[test]
    fn replicas_split_the_fleet_in_flight_cap() {
        // Four servers share 800 slots: 1600 each, not 6400 each.
        assert_eq!(effective_cap(800, 4), 1_600);
        // The floor is per server.
        assert_eq!(effective_cap(40, 4), 256);
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
