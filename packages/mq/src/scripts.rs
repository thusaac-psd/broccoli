//! The Lua scripts behind the steps that must be atomic. Each runs as one
//! `EVALSHA` (`redis::Script` falls back to `EVAL` after a Redis restart).
//!
//! Entries carry the payload in field `p`; `n` is the number of deliveries an
//! entry had before it was moved to the stream it is in now.

use std::sync::LazyLock;

use redis::Script;

/// Publish a pinned task: to the worker's private stream if the worker is
/// live, else to the shared stream.
///
/// KEYS: worker liveness key, private stream, shared stream, pinned set.
/// ARGV: worker id, payload. Returns 1 when pinned, 0 when shared.
pub(crate) static PUBLISH_PINNED: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
  redis.call('SADD', KEYS[4], ARGV[1])
  redis.call('XADD', KEYS[2], '*', 'p', ARGV[2])
  return 1
end
redis.call('XADD', KEYS[3], '*', 'p', ARGV[2])
return 0
"#,
    )
});

/// Publish a reply if its server's reply stream exists. A missing stream
/// means its server is gone (or lost the stream, and with it every waiter).
///
/// KEYS: reply stream. ARGV: payload, length cap. Returns 1 (added) or 0
/// (dropped).
pub(crate) static PUBLISH_REPLY: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
  redis.call('XADD', KEYS[1], 'MAXLEN', '~', ARGV[2], '*', 'p', ARGV[1])
  return 1
end
return 0
"#,
    )
});

/// Take over entries idle for at least the threshold.
///
/// KEYS: stream. ARGV: group, consumer, min idle ms, count.
/// Returns `{time, entries}`: Redis `TIME`, and per entry
/// `{id, deliveries, fields}` where deliveries counts this one.
pub(crate) static RECLAIM: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
local now = redis.call('TIME')
local out = {}
local pending = redis.call('XPENDING', KEYS[1], ARGV[1], 'IDLE', ARGV[3], '-', '+', ARGV[4])
for _, p in ipairs(pending) do
  local claimed = redis.call('XCLAIM', KEYS[1], ARGV[1], ARGV[2], ARGV[3], p[1])
  local entry = claimed[1]
  if entry and entry[2] then
    table.insert(out, {entry[1], p[4] + 1, entry[2]})
  end
end
return {now, out}
"#,
    )
});

/// Reset the idle time of entries this consumer still owns, without counting
/// a delivery. Entries another consumer took are left alone and returned.
///
/// KEYS: stream. ARGV: group, consumer, ids... Returns the ids no longer owned.
pub(crate) static REFRESH: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
local lost = {}
for i = 3, #ARGV do
  local p = redis.call('XPENDING', KEYS[1], ARGV[1], ARGV[i], ARGV[i], 1, ARGV[2])
  if #p == 1 then
    redis.call('XCLAIM', KEYS[1], ARGV[1], ARGV[2], 0, ARGV[i], 'JUSTID')
  else
    table.insert(lost, ARGV[i])
  end
end
return lost
"#,
    )
});

/// Move a dead worker's private stream to the shared stream, keeping each
/// entry's delivery count in `n`. Does nothing while the worker is live.
///
/// KEYS: worker liveness key, private stream, shared stream, pinned set.
/// ARGV: worker id, group, batch size. Returns the entries moved, or -1 when
/// the worker is live.
pub(crate) static DRAIN_PRIVATE: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
if redis.call('EXISTS', KEYS[1]) == 1 then
  return -1
end
local entries = redis.call('XRANGE', KEYS[2], '-', '+', 'COUNT', ARGV[3])
for _, e in ipairs(entries) do
  local id, fields = e[1], e[2]
  local payload, prior = nil, 0
  for i = 1, #fields, 2 do
    if fields[i] == 'p' then
      payload = fields[i + 1]
    elseif fields[i] == 'n' then
      prior = tonumber(fields[i + 1]) or 0
    end
  end
  local ok, p = pcall(redis.call, 'XPENDING', KEYS[2], ARGV[2], id, id, 1)
  if ok and #p == 1 then
    prior = prior + p[1][4]
  end
  if payload then
    redis.call('XADD', KEYS[3], '*', 'p', payload, 'n', prior)
  end
  redis.call('XDEL', KEYS[2], id)
end
if redis.call('XLEN', KEYS[2]) == 0 then
  redis.call('DEL', KEYS[2])
  redis.call('SREM', KEYS[4], ARGV[1])
end
return #entries
"#,
    )
});

/// Settle an entry, but only if this consumer still owns it: a consumer
/// that stalled past the claim threshold may find another one running it.
/// `ack` acknowledges and deletes it; `dead_letter` first adds `ARGV[5]` to
/// the dead-letter stream; `requeue` first adds a copy (payload `ARGV[5]`,
/// earlier deliveries `ARGV[6]`) to the end of the entry's own stream.
///
/// KEYS: stream, dead-letter stream. ARGV: group, consumer, id, mode,
/// payload, prior deliveries, dead-letter length cap. Returns 1 when settled, 0 when the entry is not
/// this consumer's (any more).
pub(crate) static SETTLE: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
local ok, p = pcall(redis.call, 'XPENDING', KEYS[1], ARGV[1], ARGV[3], ARGV[3], 1, ARGV[2])
if not ok or #p == 0 then
  return 0
end
if ARGV[4] == 'dead_letter' then
  redis.call('XADD', KEYS[2], 'MAXLEN', '~', ARGV[7], '*', 'p', ARGV[5])
elseif ARGV[4] == 'requeue' then
  redis.call('XADD', KEYS[1], '*', 'p', ARGV[5], 'n', ARGV[6])
end
redis.call('XACK', KEYS[1], ARGV[1], ARGV[3])
redis.call('XDEL', KEYS[1], ARGV[3])
return 1
"#,
    )
});

/// Length and PEL size of each stream, from the first of the ARGV groups the
/// stream has; 0 pending for a stream without any.
///
/// KEYS: streams. ARGV: groups. Returns `{len, pending}` per key.
pub(crate) static DEPTHS: LazyLock<Script> = LazyLock::new(|| {
    Script::new(
        r#"
local out = {}
for i, key in ipairs(KEYS) do
  local len = redis.call('XLEN', key)
  local pending = 0
  for _, group in ipairs(ARGV) do
    local ok, summary = pcall(redis.call, 'XPENDING', key, group)
    if ok then
      pending = summary[1]
      break
    end
  end
  out[i] = {len, pending}
end
return out
"#,
    )
});
