# Vendored crate patches

We vendor third-party crates under `third_party/` only when an upstream release
has a bug we cannot work around from the outside, and wire them in via
`[patch.crates-io]` in the root `Cargo.toml`. Each patch is documented here and
marked in-source with a `BROCCOLI VENDOR PATCH` comment so it survives a
re-vendor.

## `broccoli_queue` 0.4.6 (`third_party/broccoli_queue`)

Patches 1, 2 and 4 to 6 fix bugs in the Redis broker path; patch 3 is a lint
fix. The first two are message-loss bugs. Both silently drop a message
that the queue is supposed to retry or dead-letter, and neither is reachable
from our own code (they live inside `reject` / `process_messages`), so they can
only be fixed in the crate itself.

### 1. `reject` loses a message on its first retry

`src/brokers/redis/broker.rs`, `RedisBroker::reject`.

`reject` first `LREM`s the message out of the `<queue>_processing` list, then —
for a message that still has retry budget (`attempts < 3`) — tries to re-publish
it, reading the publish priority from `message.metadata["priority"]`. But
`InternalBrokerMessage::metadata` is `#[serde(skip)]`, so after a message has
been consumed (`HGETALL`) that metadata is **always** `None`. Upstream then
`?`-returns `"Missing priority"` — _after_ the `LREM` — so the re-publish never
happens and the message is **silently lost on the very first rejection**. This
hits every consumer's error path (e.g. a transient DB failure while persisting a
DLQ envelope drops that dead-letter record).

Patch: when the priority metadata is absent, fall back to the lowest priority
(`5`) instead of erroring, so a rejected message is always re-queued for retry.

### 2. `process_messages` leaks an undeserializable message

`src/queue.rs`, the `Some(concurrency)` branch of `process_messages` (and the
identical block in `process_messages_with_handlers`).

When a consumed message fails to deserialize into the handler's type
(`into_message()` errors — e.g. a schema drift / version skew), upstream logs
and `continue`s **without acking or rejecting it**. The message was already
popped off the main queue and pushed onto `<queue>_processing` at consume time,
so it leaks there forever (with an orphaned payload hash) and its operation
never completes — recovered only by a downstream waiter timeout.

Patch: on a deserialize failure, `acknowledge` the message (drop the poison
message, matching how a poison message would otherwise be dead-lettered) and log
loudly, instead of leaking a processing-queue entry. Both the
`Some(concurrency)` and the `None` (single-threaded) branches of
`process_messages` are patched (the `None` branch previously `?`-propagated the
deser error, killing the consumer loop). `process_messages_with_handlers` has
the same latent gap but is **not called anywhere in broccoli** (only
`process_messages` is used), so it is left unpatched rather than modifying dead
code; if it ever gets used, apply the same fix to its two branches.

### 3. `MetadataTypes::U64` dead-code lint under a redis-only build

`src/brokers/broker.rs`, the `MetadataTypes` enum.

The `U64` variant is constructed only in the rabbitmq broker
(`src/brokers/rabbitmq/broker.rs`). Our workspace pulls the crate with
`features = ["redis"]` only, so that module — and every `MetadataTypes::U64(..)`
constructor — is compiled out; the redis broker merely _matches_ the variant
(`U64(_) => None`), which does not count as a construction. A newer rustc
dead-code pass (which no longer treats the derived `Clone`/`Debug` impls as
uses) therefore reports the variant as never constructed, and our CI's
`cargo clippy --workspace -- -D warnings` promotes that warning to a hard error.

Patch: annotate the variant with
`#[cfg_attr(not(feature = "rabbitmq"), allow(dead_code))]`. This silences the
lint precisely when the constructor is absent and keeps warning if a future
build enables `rabbitmq` and the variant is genuinely dead there. The variant is
not removed because the redis broker still pattern-matches it. Not a behavior
change.

### 4. `process_messages` naps `consume_wait` even when messages are waiting

`src/queue.rs`, the spawned per-task loop in the `Some(concurrency)` branch of
`process_messages` (and the identical loop in `process_messages_with_handlers`).

`consume_wait` does two jobs upstream: it is the broker's nap after an empty
poll (default 500 ms when unset), and it is also slept before every single
consume in these loops. So a consumer cannot shorten the empty-queue nap without
also capping its throughput at one message per `consume_wait` per task. The
server's operation-result consumer needs exactly that knob: with the 500 ms
default, every test-case result waited ~0.25 s on average before the server saw
it, and contest plugins judge cases one at a time.

Patch: the per-task loop yields instead of sleeping. `consume` awaits Redis I/O
and naps on an empty queue, so the task is still abortable. With `consume_wait`
unset (every other consumer) behavior is unchanged: upstream slept zero there.

### 5. Message visibility depends on publisher vs consumer wall clocks

`src/brokers/redis/broker.rs` (`RedisBroker::publish`) and
`src/brokers/redis/utils.rs` (`RedisBroker::get_task_id`, both consume
scripts).

Upstream scores a published message as `priority x publisher's now()` and the
consume script pops the minimum but pushes it back while
`score > 5 x consumer's now()`. Both clocks were the hosts' own wall clocks, so
at priority 5 (the default for worker results and server op tasks) a message
from a host whose clock ran ahead of the consumer by D stayed invisible for D.
On an offline LAN without NTP that added the skew to every operation
round-trip, and with hours of skew results were effectively lost.

Patch: both scripts read Redis `TIME` instead of taking a client timestamp, so
the only clock involved is Redis's. The publisher passes only a relative delay
(0 unless scheduling is enabled and a delay was requested) or, for
`scheduled_at`, the explicit absolute instant the caller asked for. The score
format (`priority x unix nanoseconds`) and so the priority encoding are
unchanged, which keeps upgrades safe: a message already in Redis from the old
code is consumed normally (it is still gated by its old publisher's clock that
one time). `TIME` before a write is fine on Redis >= 5, which replicates script
effects rather than script bodies.

### 6. A Redis outage blocks every MQ call silently, for up to 300 s past recovery

`src/brokers/redis/utils.rs` (`RedisBroker::get_redis_connection`, new
`TimeoutRedisConnectionManager`), `src/brokers/redis/broker.rs` (the pool
type), `src/queue.rs` (the `Some(concurrency)` branch of `process_messages`).

When a pool check-out failed, upstream rebuilt the pool (bb8's `build` is lazy,
so that never fails), constructed a `BroccoliError` and discarded it unlogged,
then slept 1, 2, 4, ... up to 300 s and retried forever. Publish, consume, ack
and reject therefore blocked instead of erroring, callers' own backoff and
health logic never ran, and after an 80 s Redis restart consumers resumed only
at about 141 s (271 s for a 150 s outage). Separately, `bb8_redis`'s manager
opens connections with no response timeout, so a command in flight on a
half-open socket (the Redis host lost power) waited for TCP retransmission,
about 15 minutes.

Patch:
- `get_redis_connection` makes at most 3 check-out attempts (250 ms, then
  500 ms backoff, each attempt bounded by the pool's 2 s connection timeout),
  logs every failure, and returns the error. The pointless pool rebuild is
  gone.
- The pool uses `TimeoutRedisConnectionManager`, identical to
  `bb8_redis::RedisConnectionManager` except that connections carry a 2 s
  connect timeout and a 5 s response timeout, and the check-out `PING` is
  bounded at 1 s. That bound must stay below the pool's 2 s connection
  timeout: bb8 puts a connection whose validation was cancelled by that timeout
  back as healthy, so an unbounded `PING` on a dead socket would wedge every
  check-out on it, whereas a `PING` that errors marks it invalid and bb8
  replaces it.
- Because consume now errors instead of blocking, the per-task loop of
  `process_messages` sleeps 1 s after a consume error instead of spinning.
  `process_messages_with_handlers` is unused and left unpatched, as in patch 2.

### Re-vendoring

To pull a new upstream version: replace `third_party/broccoli_queue`, bump the
version here and in `[patch.crates-io]`, and re-apply all six patches (grep for
`BROCCOLI VENDOR PATCH`). If upstream fixes the message-loss, clock and outage
bugs, drop the vendor and the `[patch.crates-io]` entry (the lint patch is only
needed while we vendor).
