//! Waiting for a required dependency at boot.
//!
//! A server or worker cannot do anything useful without Postgres and Redis, so
//! boot waits for each instead of starting half-connected. A dependency that
//! is still starting (refused connection, timeout) is retried with a capped
//! backoff and one warning per attempt naming it; one that answers with a
//! refusal no retry can fix (bad credentials, a missing database, a server
//! that is too old) fails boot at once with the reason.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

/// The outcome of one failed attempt to reach a dependency.
#[derive(Debug)]
pub enum Unreachable<E> {
    /// Not answering yet; worth retrying.
    Transient(E),
    /// Answered with a refusal that retrying cannot fix.
    Fatal(E),
}

/// The first wait between attempts; it doubles up to [`MAX_DELAY`].
const FIRST_DELAY: Duration = Duration::from_millis(500);
const MAX_DELAY: Duration = Duration::from_secs(10);

/// Run `attempt` until it succeeds. `what` names the dependency and `target`
/// says where it is (a URL with its password redacted), for the log lines and
/// the error.
pub async fn wait_for<T, E, F, Fut>(what: &str, target: &str, mut attempt: F) -> anyhow::Result<T>
where
    E: Display,
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Unreachable<E>>>,
{
    let mut delay = FIRST_DELAY;
    for attempts in 1u64.. {
        match attempt().await {
            Ok(value) => {
                if attempts > 1 {
                    tracing::info!(what, target, attempts, "Reached {what}");
                }
                return Ok(value);
            }
            Err(Unreachable::Fatal(e)) => {
                anyhow::bail!("{what} at {target}: {e}")
            }
            Err(Unreachable::Transient(e)) => {
                tracing::warn!(
                    what,
                    target,
                    attempts,
                    retry_in_ms = delay.as_millis() as u64,
                    error = %e,
                    "Waiting for {what}: it is not reachable yet"
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(MAX_DELAY);
            }
        }
    }
    unreachable!("the attempt counter does not overflow")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[tokio::test(start_paused = true)]
    async fn retries_a_transient_failure_until_it_succeeds() {
        let calls = Cell::new(0);
        let value = wait_for("redis", "redis://r", || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n < 4 {
                    Err(Unreachable::Transient("connection refused"))
                } else {
                    Ok(n)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(value, 4);
    }

    #[tokio::test(start_paused = true)]
    async fn backs_off_up_to_the_cap() {
        let start = tokio::time::Instant::now();
        let calls = Cell::new(0);
        wait_for("db", "postgres://d", || {
            calls.set(calls.get() + 1);
            let n = calls.get();
            async move {
                if n <= 8 {
                    Err(Unreachable::Transient("refused"))
                } else {
                    Ok(())
                }
            }
        })
        .await
        .unwrap();
        // 0.5 + 1 + 2 + 4 + 8, then the 10 s cap three times.
        assert_eq!(start.elapsed(), Duration::from_millis(45_500));
    }

    #[tokio::test(start_paused = true)]
    async fn a_fatal_refusal_fails_at_once_naming_the_target() {
        let calls = Cell::new(0);
        let err = wait_for::<(), _, _, _>("the database", "postgres://u:***@db/b", || {
            calls.set(calls.get() + 1);
            async { Err(Unreachable::Fatal("password authentication failed")) }
        })
        .await
        .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(
            err.to_string(),
            "the database at postgres://u:***@db/b: password authentication failed"
        );
    }
}
