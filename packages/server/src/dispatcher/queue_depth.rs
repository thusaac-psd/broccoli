//! Backpressure on POST: counts and admission-control for the
//! durable `Queued` row depth.
//!
//! The API does not dispatch submissions in-process at the request
//! boundary; instead, each POST handler inserts a row with
//! `status='Queued'` and returns 201 immediately. The per-server
//! claim fiber (`dispatcher/claim.rs`) drains these rows by
//! flipping them to `Pending` and handing them off to the existing
//! plugin path.
//!
//! The natural backpressure signal is therefore **the live count of
//! queued dispatch units**: `submission_judgement` and `code_run` rows
//! in `status='Queued'` (a queued submission is counted through its
//! current judgement). When that depth exceeds the cap (derived from live
//! worker capacity, see [`derived_queue_cap`]), accepting more work would only
//! grow the buffer between ingress and dispatch - degrading p95
//! POST->Pending latency without changing throughput. Better to shed
//! load at the door with `503 Service Unavailable` + `Retry-After`.
//!
//! The check is best-effort: there is no transactional coupling
//! between the COUNT and the subsequent INSERT, so a small overshoot
//! is possible under concurrent POSTs. The cap is for steady-state
//! load shedding, not strict admission, so racing past the threshold
//! by a few dozen rows is acceptable.

use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter};

use crate::entity::{code_run, submission_judgement};
use crate::error::AppError;
use crate::state::AppState;

/// `Retry-After` seconds returned to clients when the durable queue
/// is full.
///
/// A full queue drains as fast as in-flight room frees up, and the
/// claim fiber's 1 s fallback tick bounds how long freed room can go unnoticed; 2s is one tick
/// plus a small buffer so a retried request is likely to land after room
/// was taken. Hard-coded rather than configurable to match the existing
/// `permits::RETRY_AFTER_SECS = 1` convention; a knob would just invite
/// operators to set it too high.
pub const RETRY_AFTER_SECS: u64 = 2;

/// Counts queued dispatch units: judgements (current or deferred) and code
/// runs in `status='Queued'`. Each COUNT reads the table's partial index over
/// its `Queued` rows (`idx_submission_judgement_claim`, `idx_code_run_claim`),
/// which stays as small as the cap. The filter is the inline
/// `QUEUED_PREDICATE` so the index stays usable under a generic plan.
pub async fn count_queued_rows<C>(db: &C) -> Result<u64, AppError>
where
    C: ConnectionTrait,
{
    let code_runs = code_run::Entity::find()
        .filter(super::is_queued())
        .count(db)
        .await?;
    let judgements = submission_judgement::Entity::find()
        .filter(super::is_queued())
        .count(db)
        .await?;
    Ok(code_runs + judgements)
}

/// Returns `Err(AppError::Overloaded)` if the live queued-depth is
/// at or above the configured cap; returns `Ok(())` otherwise.
///
/// A cap of `0` disables the check entirely (used by integration
/// tests that do not exercise backpressure) - the function returns
/// `Ok(())` without touching the DB.
///
/// This is the single ingress check called by every POST site that
/// inserts a `Queued` row. Callers must invoke it **before** the
/// INSERT, otherwise the row they just wrote could push the count
/// past the cap and the next concurrent POST would still squeak in
/// before observing the new state.
///
/// Status-code choice: 503 + Retry-After. Per RFC 7231 §6.6.4, 503 means "the
/// server is currently unable to handle the request due to a
/// temporary overload" - the exact semantics of durable-queue
/// backpressure (the service as a whole is past capacity, not "this
/// client is too chatty"). 429 (RFC 6585 §4) would put the blame on
/// the client, which is wrong here: a single well-behaved client can
/// trip the cap if the fleet is already saturated by other traffic.
/// `AppError::Overloaded` (status 503, code `QUEUE_OVERLOADED`)
/// carries the `Retry-After` hint the same way `RateLimited` does.
pub async fn enforce_queue_depth_admission(state: &AppState) -> Result<(), AppError> {
    let cap = state
        .config
        .server
        .max_queued_submissions
        .unwrap_or_else(|| {
            derived_queue_cap(
                state
                    .live_worker_slots
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
        });
    if cap == 0 {
        return Ok(());
    }
    let depth = count_queued_rows(&state.db).await?;
    if depth >= cap as u64 {
        tracing::warn!(
            queue_depth = depth,
            cap,
            "Rejecting POST: durable Queued depth at or above cap (backpressure)"
        );
        return Err(AppError::Overloaded {
            retry_after: RETRY_AFTER_SECS,
        });
    }
    Ok(())
}

/// Queued units allowed per live worker slot. A slot judges a test case in
/// about a second, so a few hundred queued per slot is a backlog the fleet
/// still drains within minutes; past that, refusing at the door keeps latency
/// honest instead of growing an hour-long queue.
const QUEUED_PER_WORKER_SLOT: u64 = 200;

/// The cap before any worker heartbeat is seen, and the least it ever is.
const QUEUE_CAP_FLOOR: u64 = 5000;

/// The queue cap for `live_worker_slots` (the claim fiber's view of the
/// fleet): [`QUEUED_PER_WORKER_SLOT`] per slot, at least [`QUEUE_CAP_FLOOR`].
pub fn derived_queue_cap(live_worker_slots: u64) -> u32 {
    let scaled = live_worker_slots.saturating_mul(QUEUED_PER_WORKER_SLOT);
    u32::try_from(scaled.max(QUEUE_CAP_FLOOR)).unwrap_or(u32::MAX)
}

/// `RETRY_AFTER_SECS` is part of the API contract - clients
/// implementing exponential backoff over Retry-After expect a
/// positive integer. A 0 would tell them to retry immediately,
/// which defeats the purpose of the back-off. Compile-time check so
/// a future "default Retry-After to 0" change doesn't ship silently.
const _: () = assert!(
    RETRY_AFTER_SECS > 0,
    "Retry-After must be > 0 so clients actually back off"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_queue_cap_grows_with_live_workers_above_a_floor() {
        assert_eq!(derived_queue_cap(0), 5000);
        assert_eq!(derived_queue_cap(10), 5000);
        assert_eq!(derived_queue_cap(100), 20_000);
    }
}
