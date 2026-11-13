//! Broccoli's message queue on Redis Streams.
//!
//! Three flows and nothing else: operation tasks from servers to workers
//! ([`Mq::publish_task`], [`Mq::task_consumer`]), replies from a worker to the
//! one server waiting for them ([`Mq::publish_reply`], [`Mq::reply_inbox`]),
//! and dead letters from either to the servers that record them
//! ([`GroupConsumer::dead_letter`], [`Mq::publish_dead_letter`],
//! [`Mq::dead_letter_consumer`]). The design, with its tradeoffs, is in the
//! "Message queue" internals page of the docs site.

mod error;
mod group;
mod keys;
mod reply;
mod scripts;

use std::sync::Arc;

use common::DlqEnvelope;
use common::liveness::NodeRole;
use common::worker::{Task, TaskReply};

pub use error::MqError;
pub use group::{
    BLOCK, CLAIM_IDLE, DecodeError, Delivery, EntryRef, GroupConsumer, GroupReader, MAX_DELIVERIES,
    REFRESH_INTERVAL, Settled,
};
pub use keys::{SERVERS_GROUP, StreamRole, Topology, WORKERS_GROUP};
pub use reply::{REPLY_BATCH, ReplyInbox};

use common::redis_conn::{COMMAND_TIMEOUT, Conn};

/// Oldest Redis major version the queue runs on.
pub const MIN_REDIS_MAJOR: u32 = 7;

/// `redis_version` from an `INFO server` reply.
fn redis_version(info: &str) -> Option<&str> {
    info.lines()
        .find_map(|line| line.strip_prefix("redis_version:"))
        .map(str::trim)
}

/// Entries one drain step moves out of a dead worker's private stream.
const DRAIN_BATCH: usize = 64;

/// Where a published task went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The shared stream: any worker may run it.
    Shared,
    /// The pinned worker's private stream.
    Pinned,
}

/// How long a server's reply stream outlives its last renewal. Much longer
/// than the liveness TTL, so a server that stalls (or loses Redis) for a while
/// keeps its unread replies, while a dead server's stream still expires.
pub const REPLY_STREAM_TTL: std::time::Duration = std::time::Duration::from_secs(600);

/// Approximate cap on a reply stream's length. A server reads its replies as
/// they arrive, so only a stuck or dead server's stream gets near it; the cap
/// keeps that stream from filling Redis (and stopping every write under
/// `noeviction`) while it waits to expire.
pub const REPLY_STREAM_MAXLEN: u64 = 100_000;

/// Approximate cap on the dead-letter stream's length, so a long database
/// outage cannot fill Redis. Past it the oldest unrecorded dead letters go.
pub const DEAD_LETTER_MAXLEN: u64 = 100_000;

/// What happened to a published reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyOutcome {
    Delivered,
    /// The server's reply stream is gone: the server is gone, or lost the
    /// stream and failed every waiter. Nobody is waiting for this reply.
    ServerGone,
}

/// Length and pending count of one stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamDepth {
    pub key: String,
    /// Entries no consumer has taken yet.
    pub unread: u64,
    /// Entries a consumer holds and has not acknowledged (the PEL).
    pub pending: u64,
}

/// A handle on the queue. Cheap to clone; every clone shares one connection.
#[derive(Clone)]
pub struct Mq {
    conn: Arc<Conn>,
    topology: Topology,
}

impl Mq {
    /// Connect to `url` and check that Redis answers and is at least
    /// [`MIN_REDIS_MAJOR`]. `base` is `mq.operation_queue_name`, the prefix of
    /// every key.
    pub async fn connect(url: &str, base: &str) -> Result<Self, MqError> {
        let client = redis::Client::open(url)?;
        let mq = Self {
            conn: Arc::new(Conn::new(client, COMMAND_TIMEOUT)),
            topology: Topology::new(base),
        };
        let info: String = mq
            .conn
            .run(|mut c| async move { redis::cmd("INFO").arg("server").query_async(&mut c).await })
            .await?;
        let found = redis_version(&info).unwrap_or("unknown").to_string();
        let major = found.split('.').next().and_then(|m| m.parse::<u32>().ok());
        if major.is_none_or(|m| m < MIN_REDIS_MAJOR) {
            return Err(MqError::RedisTooOld { found });
        }
        let t = &mq.topology;
        mq.check_key_types(&[
            (t.shared(), "stream"),
            (t.dead_letter(), "stream"),
            (t.pinned_set(), "set"),
        ])
        .await?;
        Ok(mq)
    }

    pub fn topology(&self) -> &Topology {
        &self.topology
    }

    /// Check that each key is missing or of its expected type (`TYPE`), so a
    /// foreign key under the queue's name fails boot with its name instead of
    /// failing every command with `WRONGTYPE`.
    pub async fn check_key_types(&self, keys: &[(String, &'static str)]) -> Result<(), MqError> {
        let types: Vec<String> = self
            .conn
            .run(|mut c| async move {
                let mut pipe = redis::pipe();
                for (key, _) in keys {
                    pipe.cmd("TYPE").arg(key);
                }
                pipe.query_async(&mut c).await
            })
            .await?;
        for ((key, expected), found) in keys.iter().zip(types) {
            if found != "none" && found != *expected {
                return Err(MqError::WrongKeyType {
                    key: key.clone(),
                    expected,
                    found,
                });
            }
        }
        Ok(())
    }

    /// Publish an operation task: to `pin`'s private stream when that worker
    /// is live, else to the shared stream.
    pub async fn publish_task(&self, task: &Task, pin: Option<&str>) -> Result<Route, MqError> {
        let payload = serde_json::to_string(task)?;
        let t = &self.topology;
        let pinned: bool = match pin {
            None => {
                let key = t.shared();
                self.conn
                    .run(|mut c| async move {
                        redis::cmd("XADD")
                            .arg(&key)
                            .arg("*")
                            .arg("p")
                            .arg(&payload)
                            .query_async::<String>(&mut c)
                            .await
                    })
                    .await?;
                false
            }
            Some(worker_id) => {
                self.conn
                    .run(|mut c| async move {
                        scripts::PUBLISH_PINNED
                            .key(NodeRole::Worker.key(t.namespace(), worker_id))
                            .key(t.private(worker_id))
                            .key(t.shared())
                            .key(t.pinned_set())
                            .arg(worker_id)
                            .arg(&payload)
                            .invoke_async(&mut c)
                            .await
                    })
                    .await?
            }
        };
        Ok(if pinned { Route::Pinned } else { Route::Shared })
    }

    /// Publish a reply to server `server_id`.
    pub async fn publish_reply(
        &self,
        server_id: &str,
        reply: &TaskReply,
    ) -> Result<ReplyOutcome, MqError> {
        let payload = serde_json::to_string(reply)?;
        let code: i64 = self
            .conn
            .run(|mut c| async move {
                scripts::PUBLISH_REPLY
                    .key(self.topology.reply(server_id))
                    .arg(&payload)
                    .arg(REPLY_STREAM_MAXLEN)
                    .invoke_async(&mut c)
                    .await
            })
            .await?;
        Ok(if code == 1 {
            ReplyOutcome::Delivered
        } else {
            ReplyOutcome::ServerGone
        })
    }

    /// Publish a dead letter that has no source entry (a server-side timeout).
    pub async fn publish_dead_letter(&self, envelope: &DlqEnvelope) -> Result<(), MqError> {
        let payload = serde_json::to_string(envelope)?;
        let key = self.topology.dead_letter();
        self.conn
            .run(|mut c| async move {
                redis::cmd("XADD")
                    .arg(&key)
                    .arg("MAXLEN")
                    .arg("~")
                    .arg(DEAD_LETTER_MAXLEN)
                    .arg("*")
                    .arg("p")
                    .arg(&payload)
                    .query_async::<String>(&mut c)
                    .await
            })
            .await?;
        Ok(())
    }

    /// The reply stream of server `server_id`. The server's node claim must
    /// own [`Topology::reply`], with [`REPLY_STREAM_TTL`], for it to exist.
    pub fn reply_inbox(&self, server_id: &str) -> ReplyInbox {
        ReplyInbox::new(self.conn.client().clone(), self.topology.reply(server_id))
    }

    /// Worker `worker_id`'s consumer of its private stream and the shared
    /// stream, private first.
    pub fn task_consumer(&self, worker_id: &str) -> GroupConsumer<Task> {
        GroupConsumer::new(
            Arc::clone(&self.conn),
            vec![self.topology.private(worker_id), self.topology.shared()],
            WORKERS_GROUP,
            worker_id,
            self.topology.dead_letter(),
        )
    }

    /// Server `server_id`'s consumer of the dead-letter stream.
    pub fn dead_letter_consumer(&self, server_id: &str) -> GroupConsumer<DlqEnvelope> {
        GroupConsumer::new(
            Arc::clone(&self.conn),
            vec![self.topology.dead_letter()],
            SERVERS_GROUP,
            server_id,
            self.topology.dead_letter(),
        )
    }

    /// Workers that may have a private stream (live, or dead and not drained
    /// yet).
    pub async fn pinned_workers(&self) -> Result<Vec<String>, MqError> {
        let pinned_set = self.topology.pinned_set();
        Ok(self
            .conn
            .run(|mut c| async move {
                redis::cmd("SMEMBERS")
                    .arg(&pinned_set)
                    .query_async(&mut c)
                    .await
            })
            .await?)
    }

    /// Move the private streams of dead workers to the shared stream. Live
    /// workers' streams are left alone. Returns the entries moved.
    pub async fn drain_dead_private_streams(&self) -> Result<usize, MqError> {
        let t = &self.topology;
        let owners = self.pinned_workers().await?;
        let mut moved = 0;
        for worker_id in &owners {
            loop {
                let n: i64 = self
                    .conn
                    .run(|mut c| async move {
                        scripts::DRAIN_PRIVATE
                            .key(NodeRole::Worker.key(t.namespace(), worker_id))
                            .key(t.private(worker_id))
                            .key(t.shared())
                            .key(t.pinned_set())
                            .key(t.dead_letter())
                            .arg(worker_id)
                            .arg(WORKERS_GROUP)
                            .arg(DRAIN_BATCH)
                            .arg(DEAD_LETTER_MAXLEN)
                            .invoke_async(&mut c)
                            .await
                    })
                    .await?;
                let n = usize::try_from(n).unwrap_or(0);
                moved += n;
                if n < DRAIN_BATCH {
                    break;
                }
            }
        }
        Ok(moved)
    }

    /// Depth of each stream in `keys`, in one round trip. Pending counts are
    /// those of the stream's consumer group (none for a reply stream).
    pub async fn depths(&self, keys: &[String]) -> Result<Vec<StreamDepth>, MqError> {
        if keys.is_empty() {
            return Ok(Vec::new());
        }
        let counts: Vec<(u64, u64)> = self
            .conn
            .run(|mut c| async move {
                let mut invocation = scripts::DEPTHS.prepare_invoke();
                for key in keys {
                    invocation.key(key);
                }
                invocation
                    .arg(WORKERS_GROUP)
                    .arg(SERVERS_GROUP)
                    .invoke_async(&mut c)
                    .await
            })
            .await?;
        Ok(keys
            .iter()
            .zip(counts)
            .map(|(key, (len, pending))| StreamDepth {
                key: key.clone(),
                unread: len.saturating_sub(pending),
                pending,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::redis_version;

    #[test]
    fn redis_version_is_read_from_info_server() {
        let info = "# Server\r\nredis_version:7.4.12\r\nredis_mode:standalone\r\n";
        assert_eq!(redis_version(info), Some("7.4.12"));
        assert_eq!(redis_version("# Server\r\n"), None);
    }
}
