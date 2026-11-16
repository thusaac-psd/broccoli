//! Redis connections with a deadline on every command, for the message queue
//! and the node liveness keys alike.
//!
//! A command that gets no answer within its deadline fails with a timeout
//! instead of hanging, which is how a half-open socket surfaces. A timeout or
//! I/O error drops the connection so the next command reconnects.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use redis::aio::MultiplexedConnection;
use redis::{AsyncConnectionConfig, Client, RedisError};
use tokio::sync::Mutex;

/// Deadline for an ordinary command.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for opening a connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// A shared multiplexed connection for non-blocking commands, reopened after
/// a failure.
pub struct Conn {
    client: Client,
    response_timeout: Duration,
    cached: Mutex<Option<MultiplexedConnection>>,
    /// Whether the last command reached Redis: cleared by a connection-level
    /// failure, set by any reply (an error reply included).
    reachable: AtomicBool,
}

impl Conn {
    pub fn new(client: Client, response_timeout: Duration) -> Self {
        Self {
            client,
            response_timeout,
            cached: Mutex::new(None),
            reachable: AtomicBool::new(true),
        }
    }

    /// Whether the last command reached Redis. `true` until a command fails
    /// to.
    pub fn is_reachable(&self) -> bool {
        self.reachable.load(Ordering::Relaxed)
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    async fn get(&self) -> Result<MultiplexedConnection, RedisError> {
        let mut cached = self.cached.lock().await;
        if let Some(conn) = cached.as_ref() {
            return Ok(conn.clone());
        }
        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(CONNECT_TIMEOUT))
            .set_response_timeout(Some(self.response_timeout));
        let conn = self
            .client
            .get_multiplexed_async_connection_with_config(&config)
            .await?;
        *cached = Some(conn.clone());
        Ok(conn)
    }

    /// Run `op` on the connection. A connection-level failure drops it, so the
    /// next call reconnects; the error is returned either way.
    pub async fn run<T, F, Fut>(&self, op: F) -> Result<T, RedisError>
    where
        F: FnOnce(MultiplexedConnection) -> Fut,
        Fut: Future<Output = Result<T, RedisError>>,
    {
        let conn = match self.get().await {
            Ok(conn) => conn,
            Err(e) => {
                self.reachable.store(false, Ordering::Relaxed);
                return Err(e);
            }
        };
        let result = op(conn).await;
        let broken = result.as_ref().err().is_some_and(breaks_connection);
        self.reachable.store(!broken, Ordering::Relaxed);
        if broken {
            *self.cached.lock().await = None;
        }
        result
    }
}

/// Whether `e` leaves the connection unusable (timeout, I/O, dropped), as
/// opposed to a reply error that the connection survives.
fn breaks_connection(e: &RedisError) -> bool {
    e.is_timeout() || e.is_io_error() || e.is_connection_dropped() || e.is_unrecoverable_error()
}

/// The error code of a server reply error (`NOGROUP`, `BUSYGROUP`, ...).
/// Errors raised by `redis.call` inside a script keep their code.
pub fn has_code(e: &RedisError, code: &str) -> bool {
    e.code() == Some(code) || e.to_string().contains(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_command_that_cannot_reach_redis_marks_it_unreachable() {
        // Nothing listens on port 1, so the connect is refused at once.
        let conn = Conn::new(
            Client::open("redis://127.0.0.1:1").unwrap(),
            COMMAND_TIMEOUT,
        );
        assert!(conn.is_reachable());
        let ping = conn
            .run(|mut c| async move { redis::cmd("PING").query_async::<String>(&mut c).await })
            .await;
        assert!(ping.is_err());
        assert!(!conn.is_reachable());
    }
}
