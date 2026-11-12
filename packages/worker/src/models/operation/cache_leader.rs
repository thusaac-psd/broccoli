//! Redis-backed leader election around a cache miss.
//!
//! When N workers compute the same `cache_key`, this module ensures that at
//! most one of them actually runs the expensive step (e.g. compile). The
//! follower returns and is expected to poll the durable `task_cache` until
//! the leader stores its output - or until a max-wait elapses and the
//! follower falls back to executing on its own.
//!
//! Design notes:
//! - `acquire` uses `SET key token NX EX ttl`. On NX-success we are leader.
//!   On NX-conflict we are follower. On any Redis transport error we return
//!   `Err(...)` and the caller treats it the same as leader (we never block
//!   the submission pipeline on Redis being down).
//! - The `LeaderLease` RAII guard owns a background heartbeat task that
//!   refreshes the TTL via a Lua CAS-extend script every `heartbeat_interval`.
//! - On `Drop`, the lease aborts the heartbeat task and fires-and-forgets a
//!   Lua CAS-delete (only deletes if the token still matches ours). This
//!   prevents the rare race where our token was stolen after a TTL expiry.
//! - `NoopCacheLeaderElector` is used in tests and when Redis MQ is disabled.

use std::time::Duration;

use async_trait::async_trait;
use redis::aio::MultiplexedConnection;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, warn};
use uuid::Uuid;

/// Redis key prefix for cache-leader locks.
pub const KEY_PREFIX: &str = "broccoli:cache-leader:";

/// Lua: extend TTL only if the current value equals our token.
/// Returns 1 on success, 0 if the key is missing or held by another token.
const EXTEND_SCRIPT: &str = r#"
if redis.call("get", KEYS[1]) == ARGV[1] then
  return redis.call("expire", KEYS[1], ARGV[2])
else
  return 0
end
"#;

/// Lua: delete the key only if the current value equals our token.
/// Returns 1 on delete, 0 otherwise.
const RELEASE_SCRIPT: &str = r#"
if redis.call("get", KEYS[1]) == ARGV[1] then
  return redis.call("del", KEYS[1])
else
  return 0
end
"#;

/// Configuration for a Redis-backed cache leader elector.
#[derive(Debug, Clone, Copy)]
pub struct CacheLeaderOpts {
    pub ttl_secs: u64,
    pub heartbeat_interval_secs: u64,
}

impl Default for CacheLeaderOpts {
    fn default() -> Self {
        Self {
            ttl_secs: 60,
            heartbeat_interval_secs: 5,
        }
    }
}

impl CacheLeaderOpts {
    /// Panics if `heartbeat_interval_secs >= ttl_secs`, which would let the
    /// TTL expire between heartbeats and is almost certainly a misconfig.
    pub fn validate(&self) {
        assert!(
            self.ttl_secs > 0,
            "cache_leader ttl_secs must be > 0 (got {})",
            self.ttl_secs
        );
        assert!(
            self.heartbeat_interval_secs > 0,
            "cache_leader heartbeat_interval_secs must be > 0 (got {})",
            self.heartbeat_interval_secs
        );
        assert!(
            self.heartbeat_interval_secs < self.ttl_secs,
            "cache_leader heartbeat_interval_secs ({}) must be < ttl_secs ({})",
            self.heartbeat_interval_secs,
            self.ttl_secs
        );
    }
}

/// Role assignment returned by the elector for a given cache key.
//
// `Leader` carries the full `LeaderLease` (heartbeat handle, redis client, two
// tokens) while `Follower` is empty, so clippy flags the size gap. This value is
// returned once per `acquire()` and immediately destructured — never stored in
// bulk — so boxing would only add a heap alloc behind the RAII lease for no real
// saving.
#[allow(clippy::large_enum_variant)]
pub enum LeaderRole {
    /// We won the race. The caller owns `LeaderLease` for the duration of the
    /// expensive step; drop releases the lock.
    Leader(LeaderLease),
    /// Another worker already holds the lock; the caller should poll the
    /// durable cache.
    Follower,
}

#[async_trait]
pub trait CacheLeaderElector: Send + Sync {
    async fn acquire(&self, cache_key: &str) -> Result<LeaderRole, String>;
}

/// RAII guard for a held cache-leader lock. Drop spawns a detached release.
pub struct LeaderLease {
    inner: Option<LeaseInner>,
}

struct LeaseInner {
    redis_key: String,
    token: String,
    cancel: oneshot::Sender<()>,
    heartbeat: JoinHandle<()>,
    client: redis::Client,
}

impl LeaderLease {
    /// Test-only helper: a no-op lease that does nothing on drop.
    pub fn noop() -> Self {
        Self { inner: None }
    }
}

impl Drop for LeaderLease {
    fn drop(&mut self) {
        let Some(inner) = self.inner.take() else {
            return;
        };
        let LeaseInner {
            redis_key,
            token,
            cancel,
            heartbeat,
            client,
        } = inner;

        // Stop the heartbeat first; ignore send/abort errors. The release
        // script below is CAS - it only DELs when the token still matches -
        // so even an in-flight EXTEND racing the release cannot corrupt
        // another leader's lock; worst case it logs `lock lost`.
        let _ = cancel.send(());
        heartbeat.abort();

        // Fire-and-forget the CAS-release. If we're being dropped outside a
        // tokio runtime (e.g., synchronous shutdown after the runtime has
        // started teardown), `tokio::spawn` would panic - guard with
        // `Handle::try_current` and fall back to the TTL safety net so the
        // worker shutdown path stays panic-free. The Redis key will then
        // expire naturally after at most `opts.ttl_secs`.
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move {
                    match client.get_multiplexed_async_connection().await {
                        Ok(mut conn) => {
                            let result: Result<i64, _> = redis::cmd("EVAL")
                                .arg(RELEASE_SCRIPT)
                                .arg(1)
                                .arg(&redis_key)
                                .arg(&token)
                                .query_async(&mut conn)
                                .await;
                            if let Err(e) = result {
                                warn!(redis_key = %redis_key, error = %e, "cache-leader release failed");
                            } else {
                                debug!(redis_key = %redis_key, "cache-leader lease released");
                            }
                        }
                        Err(e) => {
                            warn!(redis_key = %redis_key, error = %e, "cache-leader release: failed to get redis conn");
                        }
                    }
                });
            }
            Err(_) => {
                debug!(
                    redis_key = %redis_key,
                    "cache-leader lease dropped outside tokio runtime; relying on TTL safety net"
                );
            }
        }
    }
}

/// A real Redis-backed cache leader elector.
pub struct RedisCacheLeaderElector {
    client: redis::Client,
    conn: Mutex<Option<MultiplexedConnection>>,
    opts: CacheLeaderOpts,
}

impl RedisCacheLeaderElector {
    pub fn new(client: redis::Client, opts: CacheLeaderOpts) -> Self {
        opts.validate();
        Self {
            client,
            conn: Mutex::new(None),
            opts,
        }
    }

    pub fn from_url(url: &str, opts: CacheLeaderOpts) -> Result<Self, redis::RedisError> {
        let client = redis::Client::open(url)?;
        Ok(Self::new(client, opts))
    }

    async fn get_conn(&self) -> Result<MultiplexedConnection, redis::RedisError> {
        let mut guard = self.conn.lock().await;
        if let Some(ref conn) = *guard {
            return Ok(conn.clone());
        }
        let conn = self.client.get_multiplexed_async_connection().await?;
        *guard = Some(conn.clone());
        Ok(conn)
    }

    async fn invalidate_conn(&self) {
        let mut guard = self.conn.lock().await;
        *guard = None;
    }
}

#[async_trait]
impl CacheLeaderElector for RedisCacheLeaderElector {
    async fn acquire(&self, cache_key: &str) -> Result<LeaderRole, String> {
        let redis_key = format!("{KEY_PREFIX}{cache_key}");
        let token = Uuid::new_v4().to_string();

        let mut conn = match self.get_conn().await {
            Ok(c) => c,
            Err(e) => {
                self.invalidate_conn().await;
                return Err(format!("cache-leader: redis connect failed: {e}"));
            }
        };

        // SET key token NX EX ttl. On success returns "OK"; on NX conflict returns nil.
        let set_result: Result<Option<String>, _> = redis::cmd("SET")
            .arg(&redis_key)
            .arg(&token)
            .arg("NX")
            .arg("EX")
            .arg(self.opts.ttl_secs)
            .query_async(&mut conn)
            .await;

        match set_result {
            Ok(Some(_)) => {
                debug!(redis_key = %redis_key, "cache-leader acquired");
                let lease = spawn_heartbeat(
                    self.client.clone(),
                    redis_key,
                    token,
                    self.opts.ttl_secs,
                    self.opts.heartbeat_interval_secs,
                );
                Ok(LeaderRole::Leader(lease))
            }
            Ok(None) => {
                debug!(redis_key = %redis_key, "cache-leader: already held; follower");
                Ok(LeaderRole::Follower)
            }
            Err(e) => {
                self.invalidate_conn().await;
                Err(format!("cache-leader: redis SET NX failed: {e}"))
            }
        }
    }
}

fn spawn_heartbeat(
    client: redis::Client,
    redis_key: String,
    token: String,
    ttl_secs: u64,
    heartbeat_interval_secs: u64,
) -> LeaderLease {
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
    let key_for_task = redis_key.clone();
    let token_for_task = token.clone();
    let client_for_task = client.clone();

    let join = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(heartbeat_interval_secs));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First tick fires immediately; skip it - the SET already set TTL.
        interval.tick().await;

        let mut conn: Option<MultiplexedConnection> = None;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if conn.is_none() {
                        match client_for_task.get_multiplexed_async_connection().await {
                            Ok(c) => conn = Some(c),
                            Err(e) => {
                                warn!(redis_key = %key_for_task, error = %e, "cache-leader heartbeat: redis connect failed");
                                continue;
                            }
                        }
                    }
                    let c = conn.as_mut().expect("conn just initialized");
                    let result: Result<i64, _> = redis::cmd("EVAL")
                        .arg(EXTEND_SCRIPT)
                        .arg(1)
                        .arg(&key_for_task)
                        .arg(&token_for_task)
                        .arg(ttl_secs)
                        .query_async(c)
                        .await;
                    match result {
                        Ok(1) => {
                            debug!(redis_key = %key_for_task, "cache-leader heartbeat: extended");
                        }
                        Ok(_) => {
                            warn!(redis_key = %key_for_task, "cache-leader heartbeat: lock lost (token mismatch or key missing)");
                            return;
                        }
                        Err(e) => {
                            warn!(redis_key = %key_for_task, error = %e, "cache-leader heartbeat: EVAL failed");
                            conn = None;
                        }
                    }
                }
                _ = &mut cancel_rx => {
                    debug!(redis_key = %key_for_task, "cache-leader heartbeat: cancelled");
                    return;
                }
            }
        }
    });

    LeaderLease {
        inner: Some(LeaseInner {
            redis_key,
            token,
            cancel: cancel_tx,
            heartbeat: join,
            client,
        }),
    }
}

/// No-op elector: always returns `Leader` with a do-nothing lease. Used in
/// tests and when MQ is disabled.
pub struct NoopCacheLeaderElector;

#[async_trait]
impl CacheLeaderElector for NoopCacheLeaderElector {
    async fn acquire(&self, _cache_key: &str) -> Result<LeaderRole, String> {
        Ok(LeaderRole::Leader(LeaderLease::noop()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn noop_elector_always_returns_leader() {
        let elector = NoopCacheLeaderElector;
        for _ in 0..5 {
            let role = elector.acquire("any-key").await.unwrap();
            match role {
                LeaderRole::Leader(_) => {}
                LeaderRole::Follower => panic!("NoopCacheLeaderElector must always lead"),
            }
        }
    }

    #[test]
    fn opts_validation_rejects_heartbeat_geq_ttl() {
        let opts = CacheLeaderOpts {
            ttl_secs: 10,
            heartbeat_interval_secs: 10,
        };
        let result = std::panic::catch_unwind(|| opts.validate());
        assert!(
            result.is_err(),
            "heartbeat_interval >= ttl must be rejected"
        );
    }

    #[test]
    fn opts_validation_rejects_zero_ttl() {
        let opts = CacheLeaderOpts {
            ttl_secs: 0,
            heartbeat_interval_secs: 0,
        };
        let result = std::panic::catch_unwind(|| opts.validate());
        assert!(result.is_err(), "ttl_secs == 0 must be rejected");
    }

    #[test]
    fn opts_default_is_valid() {
        CacheLeaderOpts::default().validate();
    }
}
