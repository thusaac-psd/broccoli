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

use crate::error::MqError;
use crate::group::{BLOCK, DecodeError};
use common::redis_conn::{COMMAND_TIMEOUT, Conn};

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
    /// How many times the cursor restarted from the beginning: the stream
    /// went missing or came back with ids behind the cursor. An entry id
    /// names an entry only within one generation.
    generation: u64,
    /// Entries returned by the last read, deleted by the next one.
    read: Vec<String>,
}

/// A position in a reply stream as one [`ReplyInbox`] reads it: an entry id
/// and the inbox generation the id belongs to (see [`ReplyInbox::mark`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadMark {
    generation: u64,
    id: Option<String>,
}

impl ReplyInbox {
    pub(crate) fn new(client: redis::Client, key: String) -> Self {
        Self {
            key,
            conn: Conn::new(client, BLOCK + COMMAND_TIMEOUT),
            cursor: "0-0".into(),
            generation: 0,
            read: Vec::new(),
        }
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// A mark at entry `id` of the stream as this inbox reads it now; `None`
    /// for a stream that had no entry.
    pub fn mark(&self, id: Option<String>) -> ReadMark {
        ReadMark {
            generation: self.generation,
            id,
        }
    }

    /// Whether every reply up to and including `mark` has been returned by
    /// [`Self::next_batch`]; a mark without an entry is read through at once.
    /// `None` when the inbox found its stream missing or behind since the
    /// mark was taken: the mark's entry belonged to a stream that is gone, so
    /// nothing this inbox reads can pass it.
    pub fn read_through(&self, mark: &ReadMark) -> Option<bool> {
        let Some(id) = mark.id.as_deref() else {
            return Some(true);
        };
        (mark.generation == self.generation).then(|| parse_id(&self.cursor) >= parse_id(id))
    }

    /// Wait up to the block time for replies after the cursor and return them
    /// in publish order, deleting the ones the previous call returned.
    ///
    /// A stream recreated after it was lost starts its ids from the Redis
    /// clock, which may be behind the cursor. The stream's last generated id
    /// is read in the same round trip, and a cursor ahead of it, or a missing
    /// stream, restarts from the beginning, so no reply is skipped. Either
    /// starts a new generation, whatever the cursor was, even before the
    /// first read: a mark taken earlier names an entry of the lost stream (see
    /// [`Self::read_through`]).
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
        if last_id.is_none_or(|last| parse_id(&last) < parse_id(&self.cursor)) {
            self.cursor = "0-0".into();
            self.generation += 1;
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
    use super::{ReplyInbox, parse_id};

    #[test]
    fn an_inbox_has_read_through_a_mark_once_its_cursor_reaches_it() {
        let mut inbox = ReplyInbox::new(
            redis::Client::open("redis://127.0.0.1:1").unwrap(),
            "r".into(),
        );
        let mark = |inbox: &ReplyInbox, id: &str| inbox.mark(Some(id.into()));
        assert_eq!(
            inbox.read_through(&inbox.mark(None)),
            Some(true),
            "an empty stream is read"
        );
        assert_eq!(inbox.read_through(&mark(&inbox, "5-0")), Some(false));
        inbox.cursor = "5-0".into();
        assert_eq!(inbox.read_through(&mark(&inbox, "5-0")), Some(true));
        assert_eq!(inbox.read_through(&mark(&inbox, "4-9")), Some(true));
        assert_eq!(inbox.read_through(&mark(&inbox, "5-1")), Some(false));
    }

    #[test]
    fn ids_order_by_time_then_sequence() {
        assert!(parse_id("1700-9") < parse_id("1701-0"));
        assert!(parse_id("1700-1") < parse_id("1700-10"));
        assert_eq!(parse_id("0-0"), (0, 0));
    }
}
