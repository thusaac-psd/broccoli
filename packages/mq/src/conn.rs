//! Redis connections with a deadline on every command.
//!
//! A command that gets no answer within its deadline fails with a timeout
//! instead of hanging, which is how a half-open socket surfaces. A timeout or
//! I/O error drops the connection so the next command reconnects.

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
pub(crate) struct Conn {
    client: Client,
    response_timeout: Duration,
    cached: Mutex<Option<MultiplexedConnection>>,
}

impl Conn {
    pub(crate) fn new(client: Client, response_timeout: Duration) -> Self {
        Self {
            client,
            response_timeout,
            cached: Mutex::new(None),
        }
    }

    pub(crate) fn client(&self) -> &Client {
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
    pub(crate) async fn run<T, F, Fut>(&self, op: F) -> Result<T, RedisError>
    where
        F: FnOnce(MultiplexedConnection) -> Fut,
        Fut: Future<Output = Result<T, RedisError>>,
    {
        let result = op(self.get().await?).await;
        if let Err(e) = &result
            && breaks_connection(e)
        {
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
pub(crate) fn has_code(e: &RedisError, code: &str) -> bool {
    e.code() == Some(code) || e.to_string().contains(code)
}
