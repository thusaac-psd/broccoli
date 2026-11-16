//! Per-user quotas on the authenticated endpoints that create rows: abuse
//! guards nobody typing reaches.
//!
//! Each quota is a fixed window per user, counted in Redis by
//! [`crate::services::window_quota::admit`]. A request takes its place in the
//! window before it does any work, so parallel requests can never all pass a
//! check made before any of them is stored, and a request refused later (by a
//! plugin hook or validation) still spends its place.

use std::time::Duration;

use crate::error::AppError;
use crate::services::window_quota;

/// The window every per-user quota counts over.
const WINDOW: Duration = Duration::from_secs(60);

/// Clarifications one user may post per minute.
pub const CLARIFICATIONS_PER_MINUTE: u64 = 10;

/// Admit one submission or code run of `user_id`, or refuse it with
/// `RATE_LIMITED` past `limit_per_minute` of them together
/// (`submission.rate_limit_per_minute`; `0` turns the quota off).
pub async fn admit_work(mq: &mq::Mq, user_id: i32, limit_per_minute: u32) -> Result<(), AppError> {
    if limit_per_minute == 0 {
        return Ok(());
    }
    window_quota::admit(
        mq,
        &format!("work:user:{user_id}"),
        u64::from(limit_per_minute),
        WINDOW,
    )
    .await
}

/// Admit one clarification posted by `user_id`, or refuse it with
/// `RATE_LIMITED` past [`CLARIFICATIONS_PER_MINUTE`].
pub async fn admit_clarification(mq: &mq::Mq, user_id: i32) -> Result<(), AppError> {
    window_quota::admit(
        mq,
        &format!("clarification:user:{user_id}"),
        CLARIFICATIONS_PER_MINUTE,
        WINDOW,
    )
    .await
}
