//! Quotas counted in fixed windows in Redis: the one admission rule behind
//! the per-address quotas ([`super::ip_quota`]) and the per-user ones
//! ([`crate::utils::rate_limit`]). The failed sign-in quota counts outcomes,
//! not requests, and keeps a sliding log instead
//! ([`super::failed_sign_ins`]).
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

use std::time::Duration;

use crate::error::AppError;

/// Count a request against window `key` and refuse it with `RATE_LIMITED`
/// (and the seconds until the window closes) once the window holds more than
/// `limit` requests.
pub async fn admit(mq: &mq::Mq, key: &str, limit: u64, window: Duration) -> Result<(), AppError> {
    refuse_past(key, mq.count_in_window(key, window).await, limit)
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
