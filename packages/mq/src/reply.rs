//! A server's reply stream.
//!
//! Replies are only useful to the process holding the matching waiter, so the
//! stream has no consumer group: the server reads it with `XREAD` from its own
//! cursor and deletes exactly the entries it has read in the next round trip.
//! The stream is owned by the server's node claim (see
//! `common::node_identity::NodeClaim::own_stream`), so a dead server's stream
//! expires on its own.

use common::worker::TaskReply;
use redis::streams::StreamReadReply;

use crate::conn::{COMMAND_TIMEOUT, Conn};
use crate::error::MqError;
use crate::group::{BLOCK, DecodeError};

/// Replies returned by one read at most.
pub const REPLY_BATCH: usize = 512;

/// The stream's last generated id, or nil when the stream is missing.
const LAST_ID: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return false
end
local info = redis.call('XINFO', 'STREAM', KEYS[1])
for i = 1, #info, 2 do
  if info[i] == 'last-generated-id' then
    return info[i + 1]
  end
end
return false
"#;

/// Reads one server's reply stream.
pub struct ReplyInbox {
    key: String,
    conn: Conn,
    /// Id of the last entry read; the next read returns what follows it.
    cursor: String,
    /// Entries returned by the last read, deleted by the next one.
    read: Vec<String>,
}

impl ReplyInbox {
    pub(crate) fn new(client: redis::Client, key: String) -> Self {
        Self {
            key,
            conn: Conn::new(client, BLOCK + COMMAND_TIMEOUT),
            cursor: "0-0".into(),
            read: Vec::new(),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Wait up to the block time for replies after the cursor and return them
    /// in publish order, deleting the ones the previous call returned.
    ///
    /// A stream recreated after it was lost starts its ids from the Redis
    /// clock, which may be behind the cursor. The stream's last generated id
    /// is read in the same round trip, and a cursor ahead of it (or a missing
    /// stream) restarts from the beginning, so no reply is skipped.
    pub async fn next_batch(&mut self) -> Result<Vec<Result<TaskReply, DecodeError>>, MqError> {
        let key = &self.key;
        let cursor = &self.cursor;
        let read = &self.read;
        let (reply, last_id): (Option<StreamReadReply>, Option<String>) = self
            .conn
            .run(|mut c| async move {
                let mut pipe = redis::pipe();
                if !read.is_empty() {
                    pipe.cmd("XDEL").arg(key).arg(read).ignore();
                }
                pipe.cmd("XREAD")
                    .arg("COUNT")
                    .arg(REPLY_BATCH)
                    .arg("BLOCK")
                    .arg(BLOCK.as_millis() as u64)
                    .arg("STREAMS")
                    .arg(key)
                    .arg(cursor)
                    .cmd("EVAL")
                    .arg(LAST_ID)
                    .arg(1)
                    .arg(key)
                    .query_async(&mut c)
                    .await
            })
            .await?;
        self.read.clear();
        let mut out = Vec::new();
        for stream in reply.map(|r| r.keys).unwrap_or_default() {
            for entry in stream.ids {
                self.cursor.clone_from(&entry.id);
                let payload = entry
                    .map
                    .get("p")
                    .cloned()
                    .and_then(|v| redis::from_redis_value::<String>(v).ok());
                out.push(DecodeError::decode(payload.as_deref()));
                self.read.push(entry.id);
            }
        }
        let behind = last_id.is_none_or(|last| parse_id(&last) < parse_id(&self.cursor));
        if behind {
            self.cursor = "0-0".into();
        }
        Ok(out)
    }
}

/// An entry id as `(ms, seq)`, ordered as Redis orders them.
fn parse_id(id: &str) -> (u64, u64) {
    let (ms, seq) = id.split_once('-').unwrap_or((id, "0"));
    (ms.parse().unwrap_or(0), seq.parse().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::parse_id;

    #[test]
    fn ids_order_by_time_then_sequence() {
        assert!(parse_id("1700-9") < parse_id("1701-0"));
        assert!(parse_id("1700-1") < parse_id("1700-10"));
        assert_eq!(parse_id("0-0"), (0, 0));
    }
}
