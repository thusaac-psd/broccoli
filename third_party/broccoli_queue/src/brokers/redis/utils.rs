//! Utility functions for Redis broker implementation.
//!
//! This module provides helper methods for managing Redis message operations,
//! including message parsing and manipulation of message metadata.

use std::{collections::HashMap, time::Duration};

use super::broker::{RedisBroker, RedisPool};
use crate::{
    brokers::broker::{BrokerConfig, InternalBrokerMessage, MetadataTypes},
    error::BroccoliError,
    queue::ConsumeOptions,
};
use redis::{FromRedisValue, aio::MultiplexedConnection};

impl RedisBroker {
    /// Creates a new `RedisBroker` instance with default configuration.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            redis_pool: None,
            config: None,
        }
    }

    /// Creates a new `RedisBroker` instance with the specified configuration.
    ///
    /// # Arguments
    /// * `config` - The broker configuration to use
    #[must_use]
    pub const fn new_with_config(config: BrokerConfig) -> Self {
        Self {
            redis_pool: None,
            config: Some(config),
        }
    }

    pub(crate) fn ensure_pool(&self) -> Result<RedisPool, BroccoliError> {
        match &self.redis_pool {
            Some(pool) => Ok(pool.clone()),
            None => Err(BroccoliError::Broker(
                "Redis pool not initialized".to_string(),
            )),
        }
    }

    pub(crate) async fn get_task_id(
        &self,
        queue_name: &str,
        redis_connection: &mut MultiplexedConnection,
        options: Option<ConsumeOptions>,
    ) -> Result<Option<String>, BroccoliError> {
        let popped_message: Option<String> = if options
            .as_ref()
            .is_some_and(|x| x.fairness.unwrap_or(false))
        {
            let script = redis::Script::new(
                r#"
                -- BROCCOLI VENDOR PATCH (see VENDOR-PATCHES.md): Redis clock,
                -- not the consumer's, so it matches the publish score's base.
                local now = redis.call('TIME')
                local current_time = tonumber(now[1]) * 1000000000 + tonumber(now[2]) * 1000
                local queue_to_process = redis.call('LPOP', KEYS[1])
                if not queue_to_process then
                return nil
                end

                local popped_message = redis.call('ZPOPMIN', string.format("%s_%s_queue", KEYS[2], queue_to_process), 1)
                if #popped_message == 0 then
                redis.call('SREM', KEYS[3], queue_to_process)
                return nil
                end

                local message = popped_message[1]
                local score = tonumber(popped_message[2])

                if score > (5.0 * current_time) then
                redis.call('ZADD', string.format("%s_%s_queue", KEYS[2], queue_to_process), score, message)
                redis.call('RPUSH', KEYS[1], queue_to_process)
                return nil
                end

                local does_subqueue_exist = redis.call('EXISTS', string.format("%s_%s_queue", KEYS[2], queue_to_process)) == 1
                if does_subqueue_exist then
                redis.call('RPUSH', KEYS[1], queue_to_process)
                else
                redis.call('SREM', KEYS[3], queue_to_process)
                end

                if ARGV[1] == "false" then
                    local processing_queue_name = string.format("%s_%s_processing", KEYS[2], queue_to_process)
                    redis.call('LPUSH', processing_queue_name, message)
                end

                return message
            "#,
            );

            script
                .arg(
                    options
                        .is_some_and(|x| x.auto_ack.unwrap_or(false))
                        .to_string(),
                )
                .key(format!("{queue_name}_fairness_round_robin"))
                .key(queue_name)
                .key(format!("{queue_name}_fairness_set"))
                .invoke_async(redis_connection)
                .await?
        } else {
            let script = redis::Script::new(
                r#"
                -- BROCCOLI VENDOR PATCH (see VENDOR-PATCHES.md): Redis clock,
                -- not the consumer's, so it matches the publish score's base.
                local now = redis.call('TIME')
                local current_time = tonumber(now[1]) * 1000000000 + tonumber(now[2]) * 1000
                local popped_message = redis.call('ZPOPMIN', KEYS[1], 1)
                if #popped_message == 0 then
                return nil
                end

                local message = popped_message[1]
                local score = tonumber(popped_message[2])

                if score > (5.0 * current_time) then
                redis.call('ZADD', KEYS[1], score, message)
                return nil
                end
                
                if ARGV[1] == "false" then
                    redis.call('LPUSH', KEYS[2], message)
                end
                return message
            "#,
            );

            script
                .arg(
                    options
                        .is_some_and(|x| x.auto_ack.unwrap_or(false))
                        .to_string(),
                )
                .key(queue_name)
                .key(format!("{queue_name}_processing"))
                .invoke_async(redis_connection)
                .await?
        };

        Ok(popped_message)
    }

    /// Retrieves a Redis connection from the pool, retrying a few times with a
    /// short, bounded backoff.
    ///
    /// BROCCOLI VENDOR PATCH (see VENDOR-PATCHES.md): upstream retried forever
    /// with a backoff growing to 300 s, rebuilt the (lazy, so never failing)
    /// pool on every miss and discarded the error unlogged, so during a Redis
    /// outage every publish/consume/ack silently blocked for minutes past the
    /// recovery. Each failure is now logged and the error is returned after
    /// [`GET_CONNECTION_ATTEMPTS`], so callers' own backoff and health logic run.
    ///
    /// # Returns
    /// A `Result` containing a `RedisConnection` on success, or a `BroccoliError` on failure.
    pub(crate) async fn get_redis_connection(
        &self,
    ) -> Result<MultiplexedConnection, BroccoliError> {
        let redis_pool = self.ensure_pool()?;
        let mut backoff = GET_CONNECTION_BACKOFF_START;
        let mut attempt = 1;
        loop {
            match redis_pool.get().await {
                Ok(redis_connection) => return Ok(redis_connection.clone()),
                Err(err) if attempt >= GET_CONNECTION_ATTEMPTS => {
                    log::error!("Failed to get redis connection after {attempt} attempts: {err:?}");
                    return Err(BroccoliError::Broker(format!(
                        "Failed to get redis connection after {attempt} attempts: {err:?}"
                    )));
                }
                Err(err) => {
                    log::warn!(
                        "Failed to get redis connection (attempt {attempt}/{GET_CONNECTION_ATTEMPTS}), retrying in {backoff:?}: {err:?}"
                    );
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = std::cmp::min(backoff * 2, GET_CONNECTION_BACKOFF_MAX);
            attempt += 1;
        }
    }
}

/// Attempts per [`RedisBroker::get_redis_connection`] call before the error is
/// returned. Each attempt is itself bounded by the pool's 2 s connection timeout.
const GET_CONNECTION_ATTEMPTS: u32 = 3;
const GET_CONNECTION_BACKOFF_START: Duration = Duration::from_millis(250);
const GET_CONNECTION_BACKOFF_MAX: Duration = Duration::from_secs(2);

/// Bound on connecting a new pooled connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Bound on waiting for the reply to any command. Without it a command in
/// flight on a half-open socket (e.g. the Redis host lost power) waits for TCP
/// retransmission to give up, about 15 minutes. It is deliberately generous: a
/// command that times out may still have run (a consume script that already
/// moved the message to `_processing`, a publish that already enqueued it), so
/// the bound must outlast ordinary Redis stalls such as a fork or fsync.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Bound on the check-out `PING`. It must stay below the pool's connection
/// timeout: bb8 puts a connection whose validation was cancelled by that
/// timeout back as healthy, so an unbounded `PING` on a dead socket would wedge
/// every check-out on the same connection. A `PING` that errors instead marks
/// the connection invalid and bb8 replaces it.
const VALIDATE_TIMEOUT: Duration = Duration::from_secs(1);

/// BROCCOLI VENDOR PATCH (see VENDOR-PATCHES.md): `bb8_redis::RedisConnectionManager`
/// opens connections with no connection or response timeout. This manager is
/// identical except that every connection carries [`CONNECT_TIMEOUT`] and
/// [`RESPONSE_TIMEOUT`], and validation is bounded by [`VALIDATE_TIMEOUT`].
#[derive(Clone, Debug)]
pub struct TimeoutRedisConnectionManager {
    client: redis::Client,
}

impl TimeoutRedisConnectionManager {
    pub(crate) fn new(url: &str) -> Result<Self, redis::RedisError> {
        Ok(Self {
            client: redis::Client::open(url)?,
        })
    }
}

impl bb8_redis::bb8::ManageConnection for TimeoutRedisConnectionManager {
    type Connection = MultiplexedConnection;
    type Error = redis::RedisError;

    async fn connect(&self) -> Result<Self::Connection, Self::Error> {
        let config = redis::AsyncConnectionConfig::new()
            .set_connection_timeout(CONNECT_TIMEOUT)
            .set_response_timeout(RESPONSE_TIMEOUT);
        self.client
            .get_multiplexed_async_connection_with_config(&config)
            .await
    }

    async fn is_valid(&self, conn: &mut Self::Connection) -> Result<(), Self::Error> {
        let ping = redis::cmd("PING");
        match tokio::time::timeout(VALIDATE_TIMEOUT, ping.query_async::<String>(conn)).await {
            Ok(Ok(pong)) if pong == "PONG" => Ok(()),
            Ok(Ok(_)) => Err((redis::ErrorKind::ResponseError, "ping request").into()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err((redis::ErrorKind::IoError, "ping timed out").into()),
        }
    }

    fn has_broken(&self, _: &mut Self::Connection) -> bool {
        false
    }
}

pub struct OptionalInternalBrokerMessage(pub Option<InternalBrokerMessage>);

impl FromRedisValue for OptionalInternalBrokerMessage {
    fn from_redis_value(v: &redis::Value) -> redis::RedisResult<Self> {
        let map: std::collections::HashMap<String, String> = redis::from_redis_value(v)?;
        if map.is_empty() {
            return Ok(Self(None));
        }

        let task_id = map.get("task_id").ok_or_else(|| {
            redis::RedisError::from((redis::ErrorKind::TypeError, "Missing field: task_id"))
        })?;

        let payload = map.get("payload").ok_or_else(|| {
            redis::RedisError::from((redis::ErrorKind::TypeError, "Missing field: payload"))
        })?;

        let attempts = map.get("attempts").ok_or_else(|| {
            redis::RedisError::from((redis::ErrorKind::TypeError, "Missing field: attempts"))
        })?;

        let priority = map.get("priority").ok_or_else(|| {
            redis::RedisError::from((redis::ErrorKind::TypeError, "Missing field: priority"))
        })?;

        let disambiguator = map.get("disambiguator");

        let mut metadata = HashMap::new();
        metadata.insert(
            "priority".to_string(),
            MetadataTypes::String(priority.to_string()),
        );

        Ok(Self(Some(InternalBrokerMessage {
            task_id: task_id.to_string(),
            payload: payload.to_string(),
            attempts: attempts.parse().unwrap_or_default(),
            disambiguator: disambiguator.cloned(),
            metadata: Some(metadata),
        })))
    }
}
