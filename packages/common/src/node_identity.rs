//! Node identity for servers and workers: a unique default ID per host, and a
//! Redis claim that makes two live processes with the same ID impossible.
//!
//! Every node writes a TTL'd liveness key, `broccoli:{role}:heartbeat:{id}`
//! (see [`NodeRole::heartbeat_key`]). That key doubles as the node's *claim* on
//! its ID: it is created with `SET NX` at boot ([`NodeClaim::claim_at_boot`]),
//! renewed only by the process that created it (the value carries a per-process
//! `instance` token), and deleted on graceful shutdown. A second process booting
//! with an ID that a live node holds therefore refuses to start instead of
//! silently sharing its queues, dedup claims and leases.
//!
//! The value is the role's JSON heartbeat payload plus the `instance` field, so
//! existing readers of the payload (which ignore unknown fields) are unaffected.

use std::time::Duration;

use redis::aio::MultiplexedConnection;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Prefix of a worker's heartbeat (and ID claim) key.
pub const WORKER_HEARTBEAT_KEY_PREFIX: &str = "broccoli:worker:heartbeat:";
/// Prefix of a server's heartbeat (and ID claim) key. The reply-queue sweeper
/// treats a server without one as dead.
pub const SERVER_HEARTBEAT_KEY_PREFIX: &str = "broccoli:server:heartbeat:";

/// How long a heartbeat key lives without renewal. Readers treat its presence
/// as liveness, so this is also how long a crashed node still looks alive.
pub const HEARTBEAT_TTL: Duration = Duration::from_secs(15);
/// How often a live node renews its heartbeat key.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

/// The JSON field holding the per-process token that owns a claim.
const INSTANCE_FIELD: &str = "instance";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Server,
    Worker,
}

impl NodeRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Worker => "worker",
        }
    }

    pub fn heartbeat_key_prefix(self) -> &'static str {
        match self {
            Self::Server => SERVER_HEARTBEAT_KEY_PREFIX,
            Self::Worker => WORKER_HEARTBEAT_KEY_PREFIX,
        }
    }

    pub fn heartbeat_key(self, id: &str) -> String {
        format!("{}{id}", self.heartbeat_key_prefix())
    }
}

/// Returns true if `id` is safe as a node ID: it is embedded in Redis queue
/// names, so alphanumeric plus `-_.`, 1 to 128 chars, and no suffix that the
/// queue's own bookkeeping keys use.
pub fn is_valid_node_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.ends_with("_processing")
        && !id.ends_with("_failed")
        && !id.ends_with("_fairness_set")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NodeIdError {
    #[error("{role}.id must be explicitly set to a valid value", role = .0.as_str())]
    ExplicitIdRequired(NodeRole),
}

/// Resolve the effective node ID from a configured value:
/// 1. a non-empty, valid `configured` value is used as is;
/// 2. with `require_explicit`, a missing or invalid value is an error;
/// 3. otherwise the host's [`default_node_id`].
pub fn resolve_node_id(
    role: NodeRole,
    configured: &str,
    require_explicit: bool,
) -> Result<String, NodeIdError> {
    let trimmed = configured.trim();
    if is_valid_node_id(trimmed) {
        info!(role = role.as_str(), node_id = %trimmed, "Node ID resolved from explicit configuration");
        return Ok(trimmed.to_string());
    }
    if require_explicit {
        return Err(NodeIdError::ExplicitIdRequired(role));
    }
    if !trimmed.is_empty() {
        warn!(role = role.as_str(), configured = %trimmed, "Configured node ID failed validation; using the host default");
    }
    Ok(default_node_id(role))
}

/// Where a default node ID came from, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdSource {
    MachineId,
    Hostname,
    Random,
}

/// A default ID for this host that is unique across machines and stable across
/// restarts: `{role}-` plus a hash of `/etc/machine-id`. Hostnames are only a
/// fallback because cloned and live-boot machines share them (every Ubuntu live
/// session is `ubuntu`). Inside a container the machine ID is ignored (images
/// can bake one in) and the hostname, i.e. the container ID, is used. Duplicates that slip through are still caught by
/// [`NodeClaim::claim_at_boot`].
pub fn default_node_id(role: NodeRole) -> String {
    // Inside a container the machine ID is absent or baked into the image (so
    // shared by every container from it), while the hostname is the unique
    // container ID: skip the machine ID there.
    let in_container = ["/.dockerenv", "/run/.containerenv"]
        .iter()
        .any(|p| std::path::Path::new(p).exists());
    let machine_id = (!in_container)
        .then(|| {
            ["/etc/machine-id", "/var/lib/dbus/machine-id"]
                .iter()
                .find_map(|path| std::fs::read_to_string(path).ok())
        })
        .flatten();
    let hostname = hostname::get()
        .ok()
        .map(|h| h.to_string_lossy().into_owned());
    let (id, source) = derive_default_node_id(role, machine_id.as_deref(), hostname.as_deref());
    match source {
        IdSource::MachineId => {
            info!(role = role.as_str(), node_id = %id, "Node ID not configured; derived from the machine ID")
        }
        IdSource::Hostname => warn!(
            role = role.as_str(),
            node_id = %id,
            "Node ID not configured and no usable machine ID (none, or inside a container); derived from the hostname. \
             Hosts with identical hostnames will refuse to start; set the ID explicitly."
        ),
        IdSource::Random => warn!(
            role = role.as_str(),
            node_id = %id,
            "Node ID not configured and neither a machine ID nor a usable hostname was found; \
             using a random ID that changes on every restart. Set the ID explicitly."
        ),
    }
    id
}

fn derive_default_node_id(
    role: NodeRole,
    machine_id: Option<&str>,
    hostname: Option<&str>,
) -> (String, IdSource) {
    let role = role.as_str();
    if let Some(machine_id) = machine_id.map(str::trim).filter(|m| !m.is_empty()) {
        // systemd asks that the machine ID not be exposed verbatim, so hash it
        // with an application-specific prefix. 48 bits is plenty for a fleet.
        let digest = Sha256::digest(format!("broccoli-node-id:{machine_id}"));
        return (
            format!("{role}-{}", &hex::encode(digest)[..12]),
            IdSource::MachineId,
        );
    }
    if let Some(hostname) = hostname {
        let sanitized: String = hostname
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .take(100)
            .collect();
        let id = format!("{role}-{sanitized}");
        if !sanitized.is_empty() && is_valid_node_id(&id) {
            return (id, IdSource::Hostname);
        }
    }
    let random = uuid::Uuid::new_v4().simple().to_string();
    (format!("{role}-{}", &random[..12]), IdSource::Random)
}

#[derive(Debug, thiserror::Error)]
pub enum NodeClaimError {
    #[error(
        "{role} ID '{id}' is held by another live process (heartbeat key {key}: {holder}). \
         Two nodes must never share an ID; give this one a unique ID and restart"
    , role = .role.as_str())]
    Held {
        role: NodeRole,
        id: String,
        key: String,
        holder: String,
    },
}

/// Outcome of one heartbeat renewal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenewOutcome {
    /// We held the claim and refreshed it.
    Renewed,
    /// The key had expired (a stall longer than the TTL, a Redis restart, or a
    /// boot while Redis was unreachable) and we took it again.
    Reacquired,
    /// Another process holds the ID now.
    Lost,
}

/// Renew iff the key still carries our instance token; take it again if it is
/// gone. Returns 1 (renewed), 2 (reacquired) or 0 (held by someone else).
const RENEW_LUA: &str = r#"
local cur = redis.call('GET', KEYS[1])
if not cur then
  redis.call('SET', KEYS[1], ARGV[2], 'PX', ARGV[3])
  return 2
end
local ok, doc = pcall(cjson.decode, cur)
if ok and type(doc) == 'table' and doc.instance == ARGV[1] then
  redis.call('SET', KEYS[1], ARGV[2], 'PX', ARGV[3])
  return 1
end
return 0
"#;

/// Delete the key iff it still carries our instance token.
const RELEASE_LUA: &str = r#"
local cur = redis.call('GET', KEYS[1])
if not cur then return 0 end
local ok, doc = pcall(cjson.decode, cur)
if ok and type(doc) == 'table' and doc.instance == ARGV[1] then
  return redis.call('DEL', KEYS[1])
end
return 0
"#;

/// This process's claim on its node ID. See the module docs.
pub struct NodeClaim {
    client: redis::Client,
    role: NodeRole,
    id: String,
    key: String,
    instance: String,
    ttl: Duration,
    conn: Mutex<Option<MultiplexedConnection>>,
}

impl NodeClaim {
    /// Claim `id` for `role` before the node starts acting under it.
    ///
    /// A key left by a crashed predecessor expires within the TTL, so a held key
    /// is retried for one TTL plus one renew interval before giving up with
    /// [`NodeClaimError::Held`]. If Redis cannot be reached at all the node
    /// still boots (Redis-dependent work is unavailable anyway) and the claim is
    /// established by the first successful heartbeat; a duplicate found then is
    /// reported as [`RenewOutcome::Lost`].
    pub async fn claim_at_boot(
        client: redis::Client,
        role: NodeRole,
        id: &str,
        payload: serde_json::Value,
    ) -> Result<Self, NodeClaimError> {
        Self::claim_with_timing(client, role, id, payload, HEARTBEAT_TTL, HEARTBEAT_INTERVAL).await
    }

    async fn claim_with_timing(
        client: redis::Client,
        role: NodeRole,
        id: &str,
        payload: serde_json::Value,
        ttl: Duration,
        interval: Duration,
    ) -> Result<Self, NodeClaimError> {
        let claim = Self {
            client,
            role,
            id: id.to_string(),
            key: role.heartbeat_key(id),
            instance: uuid::Uuid::new_v4().to_string(),
            ttl,
            conn: Mutex::new(None),
        };
        let value = claim.value(payload);
        let deadline = tokio::time::Instant::now() + ttl + interval;
        let mut logged_wait = false;
        loop {
            match claim.try_set_nx(&value).await {
                Ok(true) => {
                    info!(role = role.as_str(), node_id = %id, "Claimed node ID");
                    return Ok(claim);
                }
                Ok(false) => {
                    let holder = claim.holder().await;
                    if tokio::time::Instant::now() >= deadline {
                        return Err(NodeClaimError::Held {
                            role,
                            id: id.to_string(),
                            key: claim.key.clone(),
                            holder,
                        });
                    }
                    if !logged_wait {
                        warn!(
                            role = role.as_str(),
                            node_id = %id,
                            holder = %holder,
                            "Node ID is held; waiting for it to expire in case it belongs to a crashed predecessor"
                        );
                        logged_wait = true;
                    }
                    tokio::time::sleep(Duration::from_secs(1).min(interval)).await;
                }
                Err(e) => {
                    warn!(
                        role = role.as_str(),
                        node_id = %id,
                        error = %e,
                        "Could not reach Redis to claim the node ID; booting unclaimed, the heartbeat will claim it"
                    );
                    return Ok(claim);
                }
            }
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Refresh the claim with a new payload. See [`RenewOutcome`].
    pub async fn renew(
        &self,
        payload: serde_json::Value,
    ) -> Result<RenewOutcome, redis::RedisError> {
        let value = self.value(payload);
        let code: i64 = self
            .with_conn(|mut conn| async move {
                redis::Script::new(RENEW_LUA)
                    .key(&self.key)
                    .arg(&self.instance)
                    .arg(&value)
                    .arg(self.ttl.as_millis() as u64)
                    .invoke_async(&mut conn)
                    .await
            })
            .await?;
        Ok(match code {
            1 => RenewOutcome::Renewed,
            2 => RenewOutcome::Reacquired,
            _ => RenewOutcome::Lost,
        })
    }

    /// Give the ID up (graceful shutdown). A key that another process holds is
    /// left alone.
    pub async fn release(&self) -> Result<(), redis::RedisError> {
        let _: i64 = self
            .with_conn(|mut conn| async move {
                redis::Script::new(RELEASE_LUA)
                    .key(&self.key)
                    .arg(&self.instance)
                    .invoke_async(&mut conn)
                    .await
            })
            .await?;
        Ok(())
    }

    /// Renew the claim every [`HEARTBEAT_INTERVAL`] with a fresh `payload()`
    /// until [`NodeHeartbeat::shutdown`], which then releases it.
    pub fn spawn_heartbeat<F>(self, payload: F) -> NodeHeartbeat
    where
        F: Fn() -> serde_json::Value + Send + 'static,
    {
        self.spawn_heartbeat_every(HEARTBEAT_INTERVAL, payload)
    }

    fn spawn_heartbeat_every<F>(self, interval: Duration, payload: F) -> NodeHeartbeat
    where
        F: Fn() -> serde_json::Value + Send + 'static,
    {
        let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();
        let join = tokio::spawn(async move {
            let role = self.role.as_str();
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = ticker.tick() => match self.renew(payload()).await {
                        Ok(RenewOutcome::Renewed) => {}
                        Ok(RenewOutcome::Reacquired) => warn!(
                            role,
                            node_id = %self.id,
                            "Heartbeat key had expired; claimed the node ID again"
                        ),
                        Ok(RenewOutcome::Lost) => error!(
                            role,
                            node_id = %self.id,
                            key = %self.key,
                            "Another process now holds this node ID; two nodes are running with the same ID. \
                             Stop one of them and give it a unique ID"
                        ),
                        Err(e) => warn!(role, node_id = %self.id, error = %e, "Heartbeat write failed"),
                    },
                    _ = &mut cancel_rx => {
                        match self.release().await {
                            Ok(()) => info!(role, node_id = %self.id, "Released node ID on shutdown"),
                            Err(e) => warn!(role, node_id = %self.id, error = %e, "Releasing node ID on shutdown failed"),
                        }
                        return;
                    }
                }
            }
        });
        NodeHeartbeat {
            cancel: Some(cancel_tx),
            join: Some(join),
        }
    }

    fn value(&self, payload: serde_json::Value) -> String {
        let mut doc = match payload {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        doc.insert(
            INSTANCE_FIELD.to_string(),
            serde_json::Value::String(self.instance.clone()),
        );
        serde_json::Value::Object(doc).to_string()
    }

    async fn try_set_nx(&self, value: &str) -> Result<bool, redis::RedisError> {
        let set: Option<String> = self
            .with_conn(|mut conn| async move {
                redis::cmd("SET")
                    .arg(&self.key)
                    .arg(value)
                    .arg("NX")
                    .arg("PX")
                    .arg(self.ttl.as_millis() as u64)
                    .query_async(&mut conn)
                    .await
            })
            .await?;
        Ok(set.is_some())
    }

    /// The current holder's payload, for the operator-facing error.
    async fn holder(&self) -> String {
        let value: Result<Option<String>, _> = self
            .with_conn(|mut conn| async move {
                redis::cmd("GET")
                    .arg(&self.key)
                    .query_async(&mut conn)
                    .await
            })
            .await;
        match value {
            Ok(Some(v)) => v,
            Ok(None) => "expired just now".into(),
            Err(e) => format!("unreadable: {e}"),
        }
    }

    /// Run `op` on the cached connection, dropping it on error so the next call
    /// reconnects.
    async fn with_conn<T, F, Fut>(&self, op: F) -> Result<T, redis::RedisError>
    where
        F: FnOnce(MultiplexedConnection) -> Fut,
        Fut: std::future::Future<Output = Result<T, redis::RedisError>>,
    {
        let conn = {
            let mut guard = self.conn.lock().await;
            match guard.as_ref() {
                Some(conn) => conn.clone(),
                None => {
                    let conn = self.client.get_multiplexed_async_connection().await?;
                    *guard = Some(conn.clone());
                    conn
                }
            }
        };
        let result = op(conn).await;
        if result.is_err() {
            *self.conn.lock().await = None;
        }
        result
    }
}

/// Handle to a running [`NodeClaim::spawn_heartbeat`] task.
pub struct NodeHeartbeat {
    cancel: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl NodeHeartbeat {
    /// Stop renewing and release the claim, waiting for the release to finish.
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    #[test]
    fn default_id_prefers_a_hashed_machine_id_over_the_hostname() {
        let (a, source) =
            derive_default_node_id(NodeRole::Worker, Some("abc123\n"), Some("ubuntu"));
        assert_eq!(source, IdSource::MachineId);
        assert!(
            a.starts_with("worker-") && a.len() == "worker-".len() + 12,
            "{a}"
        );
        assert!(
            !a.contains("abc123"),
            "the machine ID must not be exposed verbatim"
        );
        assert!(is_valid_node_id(&a));
        // Stable for the same machine, distinct across machines and roles.
        assert_eq!(
            derive_default_node_id(NodeRole::Worker, Some("abc123"), None).0,
            a
        );
        assert_ne!(
            derive_default_node_id(NodeRole::Worker, Some("def456"), None).0,
            a
        );
        let server = derive_default_node_id(NodeRole::Server, Some("abc123"), None).0;
        assert_eq!(server.strip_prefix("server-"), a.strip_prefix("worker-"));
    }

    #[test]
    fn default_id_falls_back_to_hostname_then_random() {
        assert_eq!(
            derive_default_node_id(NodeRole::Server, Some("  \n"), Some("lab pc:1")),
            ("server-lab-pc-1".to_string(), IdSource::Hostname)
        );
        let (id, source) = derive_default_node_id(NodeRole::Worker, None, None);
        assert_eq!(source, IdSource::Random);
        assert!(is_valid_node_id(&id), "{id}");
    }

    #[test]
    fn resolve_uses_valid_explicit_ids_and_enforces_require_explicit() {
        assert_eq!(
            resolve_node_id(NodeRole::Worker, "  w-1 ", false).unwrap(),
            "w-1"
        );
        assert!(is_valid_node_id(
            &resolve_node_id(NodeRole::Worker, "", false).unwrap()
        ));
        assert!(is_valid_node_id(
            &resolve_node_id(NodeRole::Worker, "bad id", false).unwrap()
        ));
        assert_eq!(
            resolve_node_id(NodeRole::Server, "", true),
            Err(NodeIdError::ExplicitIdRequired(NodeRole::Server))
        );
        assert!(resolve_node_id(NodeRole::Server, "bad id", true).is_err());
    }

    #[test]
    fn heartbeat_keys_follow_one_scheme() {
        assert_eq!(
            NodeRole::Server.heartbeat_key("s1"),
            "broccoli:server:heartbeat:s1"
        );
        assert_eq!(
            NodeRole::Worker.heartbeat_key("w1"),
            "broccoli:worker:heartbeat:w1"
        );
    }

    async fn redis() -> (redis::Client, testcontainers::ContainerAsync<Redis>) {
        let container = Redis::default().start().await.expect("redis");
        let port = container.get_host_port_ipv4(6379).await.expect("port");
        let client = redis::Client::open(format!("redis://127.0.0.1:{port}")).unwrap();
        (client, container)
    }

    async fn get(client: &redis::Client, key: &str) -> Option<String> {
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_live_duplicate_is_refused_and_a_released_id_can_be_reclaimed() {
        let (client, _c) = redis().await;
        let ttl = Duration::from_secs(2);
        let tick = Duration::from_millis(100);
        let payload = serde_json::json!({ "id": "w1", "pid": 1 });

        let first = NodeClaim::claim_with_timing(
            client.clone(),
            NodeRole::Worker,
            "w1",
            payload.clone(),
            ttl,
            tick,
        )
        .await
        .expect("the first claim succeeds");
        let stored: serde_json::Value =
            serde_json::from_str(&get(&client, "broccoli:worker:heartbeat:w1").await.unwrap())
                .unwrap();
        assert_eq!(
            stored["pid"], 1,
            "the payload is stored for heartbeat readers"
        );
        assert_eq!(stored["instance"], first.instance.as_str());

        // Kept alive by its heartbeat, the ID cannot be claimed by a second
        // process, even after waiting longer than one TTL.
        let mut heartbeat = first.spawn_heartbeat_every(Duration::from_millis(300), {
            let payload = payload.clone();
            move || payload.clone()
        });
        let err = NodeClaim::claim_with_timing(
            client.clone(),
            NodeRole::Worker,
            "w1",
            payload.clone(),
            ttl,
            tick,
        )
        .await
        .err()
        .expect("a live duplicate must be refused");
        assert!(matches!(err, NodeClaimError::Held { .. }), "{err}");

        // Graceful shutdown releases the claim, so a successor boots at once.
        heartbeat.shutdown().await;
        assert!(get(&client, "broccoli:worker:heartbeat:w1").await.is_none());
        NodeClaim::claim_with_timing(client.clone(), NodeRole::Worker, "w1", payload, ttl, tick)
            .await
            .expect("a released ID is free");
    }

    #[tokio::test]
    async fn a_crashed_predecessor_is_waited_out_and_renewal_never_steals() {
        let (client, _c) = redis().await;
        let ttl = Duration::from_millis(800);
        let tick = Duration::from_millis(100);
        let payload = serde_json::json!({});

        // A predecessor that crashed without releasing: its key just expires.
        let crashed = NodeClaim::claim_with_timing(
            client.clone(),
            NodeRole::Server,
            "s1",
            payload.clone(),
            ttl,
            tick,
        )
        .await
        .unwrap();
        let successor = NodeClaim::claim_with_timing(
            client.clone(),
            NodeRole::Server,
            "s1",
            payload.clone(),
            ttl,
            tick,
        )
        .await
        .expect("the successor claims the ID once the stale key expires");

        // The stale holder's renewal and release must not touch the new claim.
        assert_eq!(
            crashed.renew(payload.clone()).await.unwrap(),
            RenewOutcome::Lost
        );
        crashed.release().await.unwrap();
        assert_eq!(
            successor.renew(payload.clone()).await.unwrap(),
            RenewOutcome::Renewed
        );

        // A key that vanished (e.g. Redis restarted empty) is taken again.
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::cmd("FLUSHALL").query_async(&mut conn).await.unwrap();
        assert_eq!(
            successor.renew(payload).await.unwrap(),
            RenewOutcome::Reacquired
        );
    }
}
