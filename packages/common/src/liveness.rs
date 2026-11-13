//! Node liveness: the one definition every server and worker reads.
//!
//! A node is live while its liveness key `broccoli:{role}:heartbeat:{id}`
//! exists. The node writes it with a [`TTL`] and renews it every
//! [`RENEW_INTERVAL`] (see [`crate::node_identity`], where the same key is the
//! node's claim on its ID), so the key expires on the Redis clock once the node
//! stops renewing it. Nothing compares a timestamp written by one node with
//! another node's clock: a key's age is its TTL minus its remaining `PTTL`.
//!
//! The message queue's Lua scripts take the same key (via [`NodeRole::key`]) to
//! route pinned tasks, drain a dead worker's private stream and drop replies
//! for a dead server.

use std::time::Duration;

use redis::aio::ConnectionLike;

/// How long a liveness key lives without renewal: how long a crashed node still
/// looks alive.
pub const TTL: Duration = Duration::from_secs(15);
/// How often a live node renews its liveness key: five renewals per TTL, so
/// a few slow or failed renewals (a fork for `BGSAVE`, an AOF fsync stall)
/// never let a live node's key expire.
pub const RENEW_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Server,
    Worker,
}

impl NodeRole {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Worker => "worker",
        }
    }

    /// Prefix of this role's liveness keys; the id follows it.
    pub const fn key_prefix(self) -> &'static str {
        match self {
            Self::Server => "broccoli:server:heartbeat:",
            Self::Worker => "broccoli:worker:heartbeat:",
        }
    }

    /// The liveness key of node `id`.
    pub fn key(self, id: &str) -> String {
        format!("{}{id}", self.key_prefix())
    }
}

/// How long ago a liveness key was last renewed, from its remaining TTL in
/// milliseconds (`PTTL`). A key without a TTL (`-1`, which only a hand-made
/// key has) reads as just renewed. Whether a node is live is only whether its
/// key exists; this is how long ago it was renewed.
pub fn age_from_pttl(pttl_ms: i64) -> Duration {
    match u64::try_from(pttl_ms) {
        Ok(remaining) => Duration::from_millis((TTL.as_millis() as u64).saturating_sub(remaining)),
        Err(_) => Duration::ZERO,
    }
}

/// One live node: its id, the payload it last wrote and the age of that write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveNode {
    pub id: String,
    pub payload: String,
    pub age: Duration,
}

/// Every live node of `role`, sorted by id. One `SCAN` pass plus one pipelined
/// round trip for the payloads and TTLs; a key that expires in between is left
/// out.
pub async fn live_nodes<C: ConnectionLike>(
    conn: &mut C,
    role: NodeRole,
) -> redis::RedisResult<Vec<LiveNode>> {
    let pattern = format!("{}*", role.key_prefix());
    let mut keys = Vec::new();
    let mut cursor = 0_u64;
    loop {
        let (next, batch): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(256_u32)
            .query_async(conn)
            .await?;
        keys.extend(batch);
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
    keys.sort();
    keys.dedup();
    if keys.is_empty() {
        return Ok(Vec::new());
    }

    let mut pipe = redis::pipe();
    for key in &keys {
        pipe.cmd("GET").arg(key).cmd("PTTL").arg(key);
    }
    let replies: Vec<(Option<String>, i64)> = pipe.query_async(conn).await?;
    Ok(keys
        .iter()
        .zip(replies)
        .filter_map(|(key, (payload, pttl))| {
            Some(LiveNode {
                id: key.strip_prefix(role.key_prefix())?.to_string(),
                payload: payload?,
                age: age_from_pttl(pttl),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_is_the_ttl_already_spent() {
        let full = TTL.as_millis() as i64;
        assert_eq!(age_from_pttl(full), Duration::ZERO);
        assert_eq!(age_from_pttl(full - 4_000), Duration::from_secs(4));
        assert_eq!(age_from_pttl(-1), Duration::ZERO);
    }

    #[test]
    fn keys_carry_the_role_and_id() {
        assert_eq!(NodeRole::Worker.key("w1"), "broccoli:worker:heartbeat:w1");
        assert_eq!(NodeRole::Server.key("s1"), "broccoli:server:heartbeat:s1");
    }

    #[tokio::test]
    async fn live_nodes_lists_every_existing_key_of_the_role() {
        use testcontainers::runners::AsyncRunner;
        let redis = testcontainers_modules::redis::Redis::default()
            .start()
            .await
            .expect("start redis");
        let port = redis.get_host_port_ipv4(6379).await.unwrap();
        let client = redis::Client::open(format!("redis://127.0.0.1:{port}")).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::pipe()
            .cmd("SET")
            .arg(NodeRole::Worker.key("b"))
            .arg("{}")
            .arg("PX")
            .arg(TTL.as_millis() as u64 - 3_000)
            .ignore()
            .cmd("SET")
            .arg(NodeRole::Worker.key("a"))
            .arg("{}")
            .arg("PX")
            .arg(TTL.as_millis() as u64)
            .ignore()
            .cmd("SET")
            .arg(NodeRole::Worker.key("no-ttl"))
            .arg("{}")
            .ignore()
            .cmd("SET")
            .arg(NodeRole::Server.key("s"))
            .arg("{}")
            .arg("PX")
            .arg(TTL.as_millis() as u64)
            .ignore()
            .query_async(&mut conn)
            .await
            .unwrap();

        let live = live_nodes(&mut conn, NodeRole::Worker).await.unwrap();
        let ids: Vec<&str> = live.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "no-ttl"]);
        assert!(live[0].age < Duration::from_secs(2));
        assert!(live[1].age >= Duration::from_secs(3));
        assert_eq!(live[2].age, Duration::ZERO);
    }
}
