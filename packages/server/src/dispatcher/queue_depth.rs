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
//! current judgement). When that depth exceeds
//! `server.max_queued_submissions`, accepting more work would only
//! grow the buffer between ingress and dispatch - degrading p95
//! POST->Pending latency without changing throughput. Better to shed
//! load at the door with `503 Service Unavailable` + `Retry-After`.
//!
//! The check is best-effort: there is no transactional coupling
//! between the COUNT and the subsequent INSERT, so a small overshoot
//! is possible under concurrent POSTs. The cap is for steady-state
//! load shedding, not strict admission, so racing past the threshold
//! by a few dozen rows is acceptable.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter};

use common::SubmissionStatus;

use crate::entity::{code_run, submission_judgement};
use crate::error::AppError;
use crate::state::AppState;

/// `Retry-After` seconds returned to clients when the durable queue
/// is full.
///
/// The default `server.claim_poll_interval_ms` is `1000` (1s); 2s is
/// one claim tick plus a small jitter buffer so retried requests are
/// likely to land after at least one drain pass has run. Hard-coded
/// rather than configurable to match the existing
/// `permits::RETRY_AFTER_SECS = 1` convention - the latency horizon
/// of a single claim tick is the right unit, and adding a knob to
/// turn what's effectively "wait one tick" into "wait N ticks" would
/// just invite operators to set it too high.
pub const RETRY_AFTER_SECS: u64 = 2;

/// Counts queued dispatch units: judgements (current or deferred) and code
/// runs in `status='Queued'`. The judgement scan uses the index on
/// `submission_judgement.status`; `code_run.status` carries none, which is
/// fine at the default cap of a few thousand live rows.
pub async fn count_queued_rows<C>(db: &C) -> Result<u64, AppError>
where
    C: ConnectionTrait,
{
    let code_runs = code_run::Entity::find()
        .filter(code_run::Column::Status.eq(SubmissionStatus::Queued))
        .count(db)
        .await?;
    let judgements = submission_judgement::Entity::find()
        .filter(submission_judgement::Column::Status.eq(SubmissionStatus::Queued))
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
    let cap = state.config.server.max_queued_submissions;
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

/// `RETRY_AFTER_SECS` is part of the API contract - clients
/// implementing exponential backoff over Retry-After expect a
/// positive integer. A 0 would tell them to retry immediately,
/// which defeats the purpose of the back-off. Compile-time check so
/// a future "default Retry-After to 0" change doesn't ship silently.
const _: () = assert!(
    RETRY_AFTER_SECS > 0,
    "Retry-After must be > 0 so clients actually back off"
);
