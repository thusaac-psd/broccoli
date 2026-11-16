//! Quotas counted in fixed windows in Redis: the one admission rule behind
//! the per-address quotas ([`super::ip_quota`]) and the per-user ones
//! ([`crate::utils::rate_limit`]).
//!
//! Each request reserves its place in the window before it does any work
//! (see [`mq::Mq::count_in_window`]), so a burst of parallel requests is
//! admitted exactly up to the limit, and the count holds across the whole
//! fleet behind a round-robin gateway. The window's clock is Redis's, so
//! replicas' clocks never matter. A window opens at a key's first request
//! and closes a fixed time later; requests past the limit count but never
//! extend it, so a client retrying in a loop waits out the same window as one
//! that stopped. One round trip per request. When Redis cannot answer, the
//! request is admitted: these quotas guard against abuse, and an outage must
//! not lock a venue out.
//!
//! A quota that counts only some outcomes of a request (failed sign-ins, not
//! every sign-in) cannot reserve a place up front without counting what a
//! venue does, so it splits the rule in two: [`check`] refuses once the window
//! is full, before the work, and [`settle`] decides the outcome once the work
//! is done. An outcome that counts takes its place then and is refused past
//! the limit; one that does not is refused while the window is full. Requests
//! that all passed [`check`] in parallel therefore learn at most `limit`
//! counted outcomes between them, however many ran at once.

use std::time::Duration;

use crate::error::AppError;

/// Count a request against window `key` and refuse it with `RATE_LIMITED`
/// (and the seconds until the window closes) once the window holds more than
/// `limit` requests.
pub async fn admit(mq: &mq::Mq, key: &str, limit: u64, window: Duration) -> Result<(), AppError> {
    refuse_past(key, mq.count_in_window(key, window).await, limit)
}

/// Refuse a request with `RATE_LIMITED` once window `key` holds `limit`
/// events, without counting it; the caller [`settle`]s its outcome.
pub async fn check(mq: &mq::Mq, key: &str, limit: u64) -> Result<(), AppError> {
    refuse_past(key, mq.window_count(key).await, limit.saturating_sub(1))
}

/// Whether the outcome of a [`check`]ed request counts toward its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Counted (a failed sign-in).
    Counts,
    /// Not counted (a successful sign-in).
    Free,
}

/// Decide a [`check`]ed request by its outcome: one that [`Outcome::Counts`]
/// takes its place in window `key` and is refused past `limit`; a
/// [`Outcome::Free`] one is refused while the window holds `limit` events.
/// Either way the caller answers the refusal instead of the outcome, so no
/// request past the limit learns it.
pub async fn settle(
    mq: &mq::Mq,
    key: &str,
    limit: u64,
    window: Duration,
    outcome: Outcome,
) -> Result<(), AppError> {
    match outcome {
        Outcome::Counts => admit(mq, key, limit, window).await,
        Outcome::Free => check(mq, key, limit).await,
    }
}

/// Refuse when the window holds more than `allowed` events; admit when Redis
/// could not answer.
fn refuse_past(
    key: &str,
    window: Result<(u64, Duration), mq::MqError>,
    allowed: u64,
) -> Result<(), AppError> {
    match window {
        Ok((count, left)) if count > allowed => Err(AppError::RateLimited {
            retry_after: left.as_millis().div_ceil(1000).max(1) as u64,
        }),
        Ok(_) => Ok(()),
        Err(e) => {
            tracing::warn!(quota = key, error = %e, "Rate limit check failed; admitting");
            Ok(())
        }
    }
}
