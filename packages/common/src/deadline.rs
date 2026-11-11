//! Client-side upper bounds for calls into shared infrastructure.
//!
//! A query in flight when the database host vanishes (power loss, a partition
//! that outlasts TCP retransmission) never returns on its own: sqlx exposes no
//! socket keepalive or read timeout, and server-side `statement_timeout` needs
//! a live server to fire. Loops and per-task calls that must keep making
//! progress wrap their step in [`bounded`]: the step's future is dropped at the
//! deadline (sqlx rolls back an open transaction and discards the connection),
//! and the caller moves on to its next iteration.

use std::future::Future;
use std::time::Duration;

/// Budget for one iteration of a background loop (claim, lease, sweeper,
/// stuck-job detector, ...). Comfortably above the default 60 s
/// `statement_timeout`, so a merely slow statement fails server-side with a
/// real error first; this only fires when the peer is gone.
pub const BACKGROUND_STEP: Duration = Duration::from_secs(120);

/// Run `fut` for at most `limit`. Returns `None` (after a WARN naming `step`)
/// when the deadline elapsed and the future was abandoned.
pub async fn bounded<F: Future>(step: &'static str, limit: Duration, fut: F) -> Option<F::Output> {
    match tokio::time::timeout(limit, fut).await {
        Ok(out) => Some(out),
        Err(_) => {
            tracing::warn!(
                step,
                limit_ms = limit.as_millis() as u64,
                "call into shared infrastructure exceeded its deadline; abandoned it"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_the_output_within_the_deadline() {
        assert_eq!(
            bounded("t", Duration::from_secs(1), async { 7 }).await,
            Some(7)
        );
    }

    #[tokio::test]
    async fn abandons_a_future_that_never_completes() {
        let hung = std::future::pending::<()>();
        assert_eq!(bounded("t", Duration::from_millis(10), hung).await, None);
    }
}
