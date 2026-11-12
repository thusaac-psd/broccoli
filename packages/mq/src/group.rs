//! Consumer-group delivery: read, refresh, reclaim, acknowledge, dead-letter.
//!
//! An entry a consumer has read sits in the group's pending entries list (PEL)
//! until it is acknowledged. The PEL is the claim: its idle time, kept by
//! Redis, says how long ago the owner last showed it was alive. An owner
//! [`refresh`](GroupConsumer::refresh)es what it holds every
//! [`REFRESH_INTERVAL`]; anything idle for [`CLAIM_IDLE`] belongs to a dead (or
//! wedged) consumer and any consumer may [`reclaim`](GroupConsumer::reclaim)
//! it.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use redis::streams::StreamReadReply;
use serde::de::DeserializeOwned;

use crate::error::MqError;
use crate::scripts;
use common::redis_conn::{COMMAND_TIMEOUT, Conn, has_code};

/// An entry idle this long has lost its owner and may be reclaimed.
pub const CLAIM_IDLE: Duration = Duration::from_secs(30);
/// How often an owner refreshes its entries, and how often consumers look for
/// idle ones. Well under [`CLAIM_IDLE`], so a live owner never loses an entry.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
/// How long a blocking read waits for new entries.
pub const BLOCK: Duration = Duration::from_secs(2);
/// Deliveries after which an entry is a poison message: it took down or wedged
/// every consumer it reached.
pub const MAX_DELIVERIES: u32 = 3;

/// One entry of one stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryRef {
    pub stream: Arc<str>,
    pub id: String,
}

/// The outcome of acknowledging, dead-lettering or requeueing an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub enum Settled {
    Done,
    /// Another consumer took the entry over (this one stalled past the claim
    /// threshold) and settles it; nothing was changed.
    HandedOver,
}

/// A payload that did not decode as the stream's type.
#[derive(Debug, Clone, thiserror::Error)]
#[error("undecodable entry payload: {reason}")]
pub struct DecodeError {
    pub reason: String,
    /// The payload as published.
    pub raw: String,
}

impl DecodeError {
    /// Decode `raw` (the `p` field, if any) as `T`.
    pub(crate) fn decode<T: DeserializeOwned>(raw: Option<&str>) -> Result<T, Self> {
        let Some(raw) = raw else {
            return Err(Self {
                reason: "entry has no payload field".into(),
                raw: String::new(),
            });
        };
        serde_json::from_str(raw).map_err(|e| Self {
            reason: e.to_string(),
            raw: raw.to_string(),
        })
    }
}

/// An entry handed to this consumer.
#[derive(Debug)]
pub struct Delivery<T> {
    pub entry: EntryRef,
    /// Times the group has handed this entry out, this delivery included.
    pub deliveries: u32,
    pub payload: Result<T, DecodeError>,
    /// Time from publish to this delivery, on the Redis clock.
    pub queued_for: Duration,
    raw: String,
}

impl<T> Delivery<T> {
    /// The payload as published, for dead-letter records.
    pub fn raw_payload(&self) -> &str {
        &self.raw
    }

    /// Whether the group has handed this entry out more than [`MAX_DELIVERIES`]
    /// times.
    pub fn is_poison(&self) -> bool {
        self.deliveries > MAX_DELIVERIES
    }
}

struct Inner {
    conn: Arc<Conn>,
    streams: Vec<Arc<str>>,
    group: &'static str,
    consumer: String,
    dead_letter: String,
}

/// One consumer of one group over a fixed set of streams. Cheap to clone.
pub struct GroupConsumer<T> {
    inner: Arc<Inner>,
    claim_idle: Duration,
    _payload: PhantomData<fn() -> T>,
}

impl<T> Clone for GroupConsumer<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            claim_idle: self.claim_idle,
            _payload: PhantomData,
        }
    }
}

impl<T: DeserializeOwned> GroupConsumer<T> {
    pub(crate) fn new(
        conn: Arc<Conn>,
        streams: Vec<String>,
        group: &'static str,
        consumer: &str,
        dead_letter: String,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                conn,
                streams: streams.into_iter().map(Arc::from).collect(),
                group,
                consumer: consumer.to_string(),
                dead_letter,
            }),
            claim_idle: CLAIM_IDLE,
            _payload: PhantomData,
        }
    }

    /// Reclaim entries idle for `idle` instead of [`CLAIM_IDLE`]. Tests use a
    /// short threshold to play a dead consumer without waiting for it.
    pub fn with_claim_idle(mut self, idle: Duration) -> Self {
        self.claim_idle = idle;
        self
    }

    /// The streams this consumer reads, in read order.
    pub fn streams(&self) -> &[Arc<str>] {
        &self.inner.streams
    }

    /// Create the group on every stream (and the stream itself) if missing.
    /// The group starts at the beginning, so entries published before any
    /// consumer existed are delivered.
    pub async fn ensure_groups(&self) -> Result<(), MqError> {
        for stream in &self.inner.streams {
            let created: Result<(), _> = self
                .inner
                .conn
                .run(|mut c| async move {
                    redis::cmd("XGROUP")
                        .arg("CREATE")
                        .arg(&**stream)
                        .arg(self.inner.group)
                        .arg("0")
                        .arg("MKSTREAM")
                        .query_async(&mut c)
                        .await
                })
                .await;
            match created {
                Ok(()) => {}
                Err(e) if has_code(&e, "BUSYGROUP") => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// A reader with its own connection for blocking reads.
    pub fn reader(&self) -> GroupReader<T> {
        GroupReader {
            consumer: self.clone(),
            conn: Conn::new(self.inner.conn.client().clone(), BLOCK + COMMAND_TIMEOUT),
        }
    }

    /// Take over up to `max` entries that have been idle for [`CLAIM_IDLE`]
    /// (see [`Self::with_claim_idle`]), across all streams.
    pub async fn reclaim(&self, max: usize) -> Result<Vec<Delivery<T>>, MqError> {
        let mut out = Vec::new();
        for stream in &self.inner.streams {
            let room = max.saturating_sub(out.len());
            if room == 0 {
                break;
            }
            let reply: (RedisTime, Vec<ReclaimedEntry>) = self
                .run_or_regroup(|mut c| async move {
                    scripts::RECLAIM
                        .key(&**stream)
                        .arg(self.inner.group)
                        .arg(&self.inner.consumer)
                        .arg(self.claim_idle.as_millis() as u64)
                        .arg(room)
                        .invoke_async(&mut c)
                        .await
                })
                .await?
                .unwrap_or_default();
            let now_ms = redis_time_ms(&reply.0);
            for (id, deliveries, fields) in reply.1 {
                let fields = pairs(fields);
                out.push(decode(stream, id, deliveries, &fields, now_ms));
            }
        }
        Ok(out)
    }

    /// Reset the idle time of `held` entries this consumer still owns. Returns
    /// the entries another consumer has taken over.
    pub async fn refresh(&self, held: &[EntryRef]) -> Result<Vec<EntryRef>, MqError> {
        let mut lost = Vec::new();
        for stream in &self.inner.streams {
            let ids: Vec<&str> = held
                .iter()
                .filter(|e| e.stream == *stream)
                .map(|e| e.id.as_str())
                .collect();
            if ids.is_empty() {
                continue;
            }
            let ids = &ids;
            let gone: Vec<String> = self
                .run_or_regroup(|mut c| async move {
                    scripts::REFRESH
                        .key(&**stream)
                        .arg(self.inner.group)
                        .arg(&self.inner.consumer)
                        .arg(ids)
                        .invoke_async(&mut c)
                        .await
                })
                .await?
                .unwrap_or_else(|| ids.iter().map(|id| id.to_string()).collect());
            lost.extend(gone.into_iter().map(|id| EntryRef {
                stream: Arc::clone(stream),
                id,
            }));
        }
        Ok(lost)
    }

    /// Acknowledge and delete an entry this consumer owns.
    pub async fn ack(&self, entry: &EntryRef) -> Result<Settled, MqError> {
        self.settle(entry, "ack", "", 0).await
    }

    /// Move an entry this consumer owns to the dead-letter stream as
    /// `envelope`, in one step.
    pub async fn dead_letter(
        &self,
        entry: &EntryRef,
        envelope: &common::DlqEnvelope,
    ) -> Result<Settled, MqError> {
        let envelope = serde_json::to_string(envelope)?;
        self.settle(entry, "dead_letter", &envelope, 0).await
    }

    /// Give back an entry this consumer owns but never started: a copy goes
    /// to the end of its stream with its delivery count, and the original is
    /// acknowledged, in one step.
    pub async fn requeue(&self, delivery: &Delivery<T>) -> Result<Settled, MqError> {
        let prior = delivery.deliveries.saturating_sub(1);
        self.settle(&delivery.entry, "requeue", &delivery.raw, prior)
            .await
    }

    async fn settle(
        &self,
        entry: &EntryRef,
        mode: &str,
        payload: &str,
        prior: u32,
    ) -> Result<Settled, MqError> {
        let settled: i64 = self
            .inner
            .conn
            .run(|mut c| async move {
                scripts::SETTLE
                    .key(&*entry.stream)
                    .key(&self.inner.dead_letter)
                    .arg(self.inner.group)
                    .arg(&self.inner.consumer)
                    .arg(&entry.id)
                    .arg(mode)
                    .arg(payload)
                    .arg(prior)
                    .invoke_async(&mut c)
                    .await
            })
            .await?;
        Ok(if settled == 1 {
            Settled::Done
        } else {
            Settled::HandedOver
        })
    }

    /// Run a command that needs the group; `None` when the group was missing
    /// (a stream deleted, or a Redis restart that lost it) and has been
    /// recreated.
    async fn run_or_regroup<R, F, Fut>(&self, op: F) -> Result<Option<R>, MqError>
    where
        F: FnOnce(redis::aio::MultiplexedConnection) -> Fut,
        Fut: Future<Output = Result<R, redis::RedisError>>,
    {
        match self.inner.conn.run(op).await {
            Ok(r) => Ok(Some(r)),
            Err(e) if has_code(&e, "NOGROUP") => {
                self.ensure_groups().await?;
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
}

/// Blocking reads for one [`GroupConsumer`], on a connection of its own.
pub struct GroupReader<T> {
    consumer: GroupConsumer<T>,
    conn: Conn,
}

impl<T: DeserializeOwned> GroupReader<T> {
    /// Wait up to [`BLOCK`] for new entries and return them, at most `max` per
    /// stream. Empty on timeout, and after recreating a missing group.
    pub async fn read(&mut self, max: usize) -> Result<Vec<Delivery<T>>, MqError> {
        let inner = &self.consumer.inner;
        let streams: Vec<&str> = inner.streams.iter().map(|s| &**s).collect();
        let result: Result<(Option<StreamReadReply>, (String, String)), _> = self
            .conn
            .run(|mut c| async move {
                redis::pipe()
                    .cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(inner.group)
                    .arg(&inner.consumer)
                    .arg("COUNT")
                    .arg(max.max(1))
                    .arg("BLOCK")
                    .arg(BLOCK.as_millis() as u64)
                    .arg("STREAMS")
                    .arg(&streams)
                    .arg(vec![">"; streams.len()])
                    // Redis time right after the read returned, for queue age.
                    .cmd("TIME")
                    .query_async(&mut c)
                    .await
            })
            .await;
        let (reply, time) = match result {
            Ok(r) => r,
            Err(e) if has_code(&e, "NOGROUP") => {
                self.consumer.ensure_groups().await?;
                return Ok(Vec::new());
            }
            Err(e) => return Err(e.into()),
        };
        let now_ms = redis_time_ms(&time);
        let mut out = Vec::new();
        for key in reply.map(|r| r.keys).unwrap_or_default() {
            let Some(stream) = inner.streams.iter().find(|s| ***s == key.key) else {
                continue;
            };
            for entry in key.ids {
                let fields: Vec<(String, String)> = entry
                    .map
                    .into_iter()
                    .filter_map(|(k, v)| Some((k, redis::from_redis_value::<String>(v).ok()?)))
                    .collect();
                out.push(decode(stream, entry.id, 1, &fields, now_ms));
            }
        }
        Ok(out)
    }
}

/// Build a delivery from an entry's fields. `deliveries` counts deliveries in
/// this stream; `n` adds those it had before it was moved here.
fn decode<T: DeserializeOwned>(
    stream: &Arc<str>,
    id: String,
    deliveries: u32,
    fields: &[(String, String)],
    now_ms: u64,
) -> Delivery<T> {
    let field = |name: &str| fields.iter().find(|(k, _)| k == name).map(|(_, v)| v);
    let prior = field("n").and_then(|n| n.parse::<u32>().ok()).unwrap_or(0);
    let raw = field("p").cloned().unwrap_or_default();
    let payload = DecodeError::decode(field("p").map(String::as_str));
    let queued_for = Duration::from_millis(now_ms.saturating_sub(entry_id_ms(&id)));
    Delivery {
        entry: EntryRef {
            stream: Arc::clone(stream),
            id,
        },
        deliveries: deliveries.saturating_add(prior),
        payload,
        queued_for,
        raw,
    }
}

/// Redis `TIME`: seconds and microseconds.
type RedisTime = (String, String);
/// One entry the reclaim script took over: id, deliveries, flat fields.
type ReclaimedEntry = (String, u32, Vec<String>);

/// Flat `[field, value, ...]` into pairs.
pub(crate) fn pairs(flat: Vec<String>) -> Vec<(String, String)> {
    let mut it = flat.into_iter();
    std::iter::from_fn(|| Some((it.next()?, it.next()?))).collect()
}

/// The millisecond part of a stream entry id (`<ms>-<seq>`): its Redis time of
/// publish.
pub(crate) fn entry_id_ms(id: &str) -> u64 {
    id.split('-')
        .next()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(0)
}

/// Redis `TIME` (`[seconds, microseconds]`) in milliseconds.
fn redis_time_ms((secs, micros): &RedisTime) -> u64 {
    let secs: u64 = secs.parse().unwrap_or(0);
    let micros: u64 = micros.parse().unwrap_or(0);
    secs * 1000 + micros / 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_ids_carry_their_publish_time() {
        assert_eq!(entry_id_ms("1794475204880-3"), 1_794_475_204_880);
        assert_eq!(entry_id_ms("garbage"), 0);
    }

    #[test]
    fn moved_entries_add_their_earlier_deliveries() {
        let stream: Arc<str> = Arc::from("s");
        let fields = vec![
            ("p".to_string(), "42".to_string()),
            ("n".to_string(), "2".to_string()),
        ];
        let d: Delivery<u32> = decode(&stream, "1000-0".into(), 1, &fields, 1_500);
        assert_eq!(d.deliveries, 3);
        assert_eq!(*d.payload.as_ref().unwrap(), 42);
        assert_eq!(d.queued_for, Duration::from_millis(500));
        assert!(
            !d.is_poison(),
            "{MAX_DELIVERIES} deliveries are still allowed"
        );
        let d: Delivery<u32> = decode(&stream, "1000-0".into(), 2, &fields, 1_500);
        assert!(d.is_poison());
    }

    #[test]
    fn a_payload_of_the_wrong_shape_is_a_decode_error() {
        let stream: Arc<str> = Arc::from("s");
        let fields = vec![("p".to_string(), "{\"x\":1}".to_string())];
        let d: Delivery<u32> = decode(&stream, "1-0".into(), 1, &fields, 1);
        assert!(d.payload.is_err());
        assert_eq!(d.raw_payload(), "{\"x\":1}");
    }
}
