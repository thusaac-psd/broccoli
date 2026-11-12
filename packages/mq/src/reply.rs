//! A server's reply stream.
//!
//! Replies are only useful to the process holding the matching waiter, so the
//! stream has no consumer group: the server reads it with `XREAD` from its own
//! cursor and trims what it has read in the same round trip. The stream lives
//! and dies with the server's liveness key (the server's node claim owns it,
//! see `common::node_identity::NodeClaim::own_stream`), so a dead server's
//! stream expires on its own.

use common::worker::TaskReply;
use redis::streams::StreamReadReply;

use crate::conn::{COMMAND_TIMEOUT, Conn};
use crate::error::MqError;
use crate::group::{BLOCK, DecodeError};

/// Replies returned by one read at most.
pub const REPLY_BATCH: usize = 512;

/// Reads one server's reply stream.
pub struct ReplyInbox {
    key: String,
    conn: Conn,
    /// Id of the last entry read; everything up to it is trimmed next read.
    cursor: String,
    trim: bool,
}

impl ReplyInbox {
    pub(crate) fn new(client: redis::Client, key: String) -> Self {
        Self {
            key,
            conn: Conn::new(client, BLOCK + COMMAND_TIMEOUT),
            cursor: "0-0".into(),
            trim: false,
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Wait up to the block time for replies after the cursor and return them
    /// in publish order, dropping the ones the previous call returned.
    pub async fn next_batch(&mut self) -> Result<Vec<Result<TaskReply, DecodeError>>, MqError> {
        let key = &self.key;
        let cursor = &self.cursor;
        let trim_below = self.trim.then(|| next_id(cursor));
        let reply: (Option<StreamReadReply>,) = self
            .conn
            .run(|mut c| async move {
                let mut pipe = redis::pipe();
                if let Some(min_id) = &trim_below {
                    pipe.cmd("XTRIM").arg(key).arg("MINID").arg(min_id).ignore();
                }
                pipe.cmd("XREAD")
                    .arg("COUNT")
                    .arg(REPLY_BATCH)
                    .arg("BLOCK")
                    .arg(BLOCK.as_millis() as u64)
                    .arg("STREAMS")
                    .arg(key)
                    .arg(cursor)
                    .query_async(&mut c)
                    .await
            })
            .await?;
        self.trim = false;
        let mut out = Vec::new();
        for stream in reply.0.map(|r| r.keys).unwrap_or_default() {
            for entry in stream.ids {
                self.cursor.clone_from(&entry.id);
                self.trim = true;
                let payload = entry
                    .map
                    .get("p")
                    .cloned()
                    .and_then(|v| redis::from_redis_value::<String>(v).ok());
                out.push(DecodeError::decode(payload.as_deref()));
            }
        }
        Ok(out)
    }
}

/// The smallest entry id after `id`.
fn next_id(id: &str) -> String {
    let (ms, seq) = id.split_once('-').unwrap_or((id, "0"));
    let seq: u64 = seq.parse().unwrap_or(0);
    format!("{ms}-{}", seq.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::next_id;

    #[test]
    fn next_id_is_the_following_sequence_number() {
        assert_eq!(next_id("1700-0"), "1700-1");
        assert_eq!(next_id("1700-41"), "1700-42");
    }
}
