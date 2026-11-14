---
title: Message queue
sidebar_label: Message queue
sidebar_position: 3
---

# Message queue

Servers hand sandbox work to workers, and workers hand results back, through
Redis Streams. The queue lives in `packages/mq`. It is not a general broker: its
API names Broccoli's three flows (operation tasks, replies, dead letters) and
nothing else. Key names, Lua scripts and timing constants live in that crate
alone, so a server and a worker cannot disagree about them.

## The flows it serves

| Flow | Producer | Consumer | What it carries |
| --- | --- | --- | --- |
| Operation task | server (`services/operation_batch.rs`) | any worker, or one pinned worker | `common::worker::Task` |
| Reply | the worker that ran the task | the server that published it | `common::worker::TaskReply` (`Started`, `Completed`) |
| Dead letter | worker (poison task, exhausted retries), server (timed out batch) | any server, which writes the `dead_letter_message` table | `common::DlqEnvelope` |

Everything else Broccoli keeps in Redis (node liveness keys, cancel keys, the
warm pub/sub channel, plugin state) is outside the queue.

What the flows need from it:

- At least once delivery of operation tasks. A task that a dead worker took
  must run again, and a task that a live worker is running must not.
- A reply reaches the one server replica that waits for it. A reply for a
  server that is gone is dropped, not stored forever.
- No priorities and no delayed delivery. No plugin sets an operation priority,
  and retry backoff is a sleep inside the worker that holds the task. Both were
  dropped rather than built.
- No ordering between tasks. Operations of one batch run in parallel and the
  server matches replies by task id.

## Streams and keys

`<q>` is `mq.operation_queue_name` (default `operation_tasks`). Node ids never
contain `:`, so every key below parses one way.

| Key | Type | Group | Lifetime |
| --- | --- | --- | --- |
| `<q>` | stream | `workers` | permanent |
| `<q>:worker:<worker_id>` | stream | `workers` | until drained after its worker dies |
| `<q>:pinned` | set of worker ids | | members removed when drained |
| `<q>:reply:<server_id>` | stream | none | TTL of the server's liveness key |
| `<q>:outstanding:<server_id>` | sorted set of task ids | | 10 minutes after the server's last publish or check |
| `<q>:dlq` | stream | `servers` | permanent |
| `<q>:heartbeat:<role>:<id>` | string | | liveness TTL, 15 s (see Liveness) |

Every entry has one field, `p`, holding the JSON payload. An entry moved from a
dead worker's private stream also carries `n`, the deliveries it already had.

### Work streams and consumer groups

Workers read the shared stream and their own private stream in one
`XREADGROUP GROUP workers <worker_id> COUNT <free slots> BLOCK 2000 STREAMS
<q>:worker:<id> <q> > >`. The consumer name is the worker id, which the node
claim (`common::node_identity`) makes unique, so a restarted worker finds its
own pending entries again. Each worker creates the group at boot with
`XGROUP CREATE ... 0 MKSTREAM` and treats `BUSYGROUP` as success; the `0` start
means entries published before any worker existed are still delivered.

A delivered entry sits in the group's pending entries list (PEL) until the
worker settles it. Every settle (acknowledge, dead-letter, requeue) is one
script that first checks with `XPENDING <stream> <group> <id> <id> 1 <consumer>`
that this consumer still owns the entry: a worker that stalled past the claim
threshold finds the entry handed over and changes nothing. The `XDEL` keeps a
stream as long as its backlog plus its in flight work, so no `MAXLEN` trimming
is needed, and trimming a work stream would drop unread tasks.

### Pinned tasks

An admin rejudge can pin a submission to a worker (`target_worker_id`). The
server publishes a pinned task with one script that checks the worker's
liveness key and then either adds the task to `<q>:worker:<id>` and the worker
id to `<q>:pinned`, or adds it to the shared stream. The liveness check and the
write are one atomic step, so a task is never routed to a worker that is
already known dead, and the publish costs one round trip whether it was pinned
or not.

When a pinned worker dies after the publish, its private stream is drained by
whichever live worker runs the drain script next (every reclaim tick, below).
The script returns at once if the owner's liveness key exists; otherwise it
moves up to 64 entries to the shared stream, carrying their delivery counts in
`n`, and deletes the private stream and its `<q>:pinned` member once it is
empty. A worker that stalled past its liveness TTL and comes back finds its
private stream gone, gets `NOGROUP` on the next read, and recreates the group.

### Reply streams

A server reads its reply stream with plain `XREAD`, not a consumer group:
replies are only useful to the process holding the matching waiter in memory,
so redelivering one to a restarted server would only reach the `no_waiter`
path. Each read is one pipelined round trip:

```text
XDEL <q>:reply:<server_id> <ids the previous read returned>
XREAD COUNT 512 BLOCK 2000 STREAMS <q>:reply:<server_id> <last read id>
EVAL <last-generated-id of the stream>
```

The `XDEL` removes exactly what the previous read returned, so the stream
holds only replies the server has not seen, and nothing unread is ever
deleted. `BLOCK` wakes on the first `XADD`, so a reply waits no polling
interval. A stream recreated after it was lost takes its ids from the Redis
clock, which can be behind the cursor; when the stream's last generated id is
behind the cursor, or the stream is missing, the reader starts again from the
beginning.

The server's node claim owns the reply stream (`NodeClaim::own_stream`): the
same script that renews the liveness key creates the stream empty
(`XADD ... MAXLEN 0`) when it is missing and gives it its own TTL,
`REPLY_STREAM_TTL` (10 minutes); the first renewal runs as the heartbeat
starts, before the server publishes anything. Graceful shutdown deletes both.
A crashed server's reply stream therefore expires 10 minutes after its last
renewal, on the Redis clock, and nothing has to sweep it. The long TTL means a
server that stalls, or loses Redis, for longer than the 15 s liveness TTL
keeps every reply published meanwhile. A server that restarts within the TTL
reads the replies its previous run left behind; they reach no waiter.

Workers publish replies with a script that adds the reply only if the stream
exists, and otherwise drops it: a missing stream means the server is gone, or
lost the stream. A reply never creates a stream, so no stream exists without a
TTL. A server whose stream disappears while it runs (a Redis that lost its
data, or a stall longer than 10 minutes) learns it from the renewal that
recreates the stream, and fails every outstanding operation at once, so its
plugins see failed operations instead of waiting out their timeouts.

### The outstanding ledger

A reply stream that survives does not prove the operations behind it are
alive: a Redis that restarts from a snapshot taken a few seconds earlier
keeps the stream but loses every task published after the snapshot. So each
server has a ledger, `<q>:outstanding:<server_id>`, of the tasks it published
and has no reply for, scored by their publish time on the Redis clock.

- The script that publishes a task adds it to the ledger, so the task and its
  ledger entry are written in one step: a Redis that loses one loses both.
- The script that publishes a completed reply removes the task from the
  ledger, also in one step. A start notification leaves it.
- Nothing else removes a task: a task waiting in a stream, held in a worker's
  pending list, requeued, or moved off a dead worker's private stream stays in
  the ledger.

Every 10 seconds (`OUTSTANDING_CHECK_INTERVAL`) the server's result consumer
checks its published waiters against the ledger, 1024 ids per round trip, and
notes the id of the reply stream's last entry in the same script. A missing
task either has its reply at or before that entry, or was lost. The consumer
keeps reading replies, and once its reads have passed the noted entry, any
of those waiters still waiting fails at once with "the operation was lost
from the queue". The plugin sees a failed operation, which its judging
handles like any other (a `SystemError`, then the system-error retry). A
waiter counts as published only once its publish returned, so a task still
being published is never mistaken for a lost one.

The check also renews the ledger's TTL (10 minutes, so a dead server's ledger
expires with its reply stream) and drops entries older than two hours: tasks
the host cancelled, which a worker settles without a reply. A server that
cannot reach Redis stops claiming new work (see "Failure behaviour") until a
command gets through again, which the next check does within its interval.

## Delivery, redelivery and liveness of in flight work

The PEL is the claim on an in flight task. Its idle time, kept by Redis, is how
long ago the owner last showed it was alive.

- Every 5 s a worker resets the idle time of every task it is running with
  `XCLAIM <stream> workers <self> 0 <ids> JUSTID`, inside a script that skips ids
  no longer owned by itself. `JUSTID` does not count a delivery.
- Every 5 s a worker with free slots runs the reclaim script on both of its
  streams: `XPENDING <stream> workers IDLE 30000 - + <free slots>` followed by
  `XCLAIM` with the same 30 s minimum idle time. The `XCLAIM` minimum makes two
  workers reclaiming at once safe: only one gets each entry.
- A long sandbox job is never reclaimed while its worker lives, however long it
  runs, because the refresh keeps it under 30 s idle. A crashed worker's tasks
  are redelivered 30 to 35 s after its last refresh.

The PEL is the only claim on a task: there are no per-task dedup keys and no
reaper for dead workers' operations. The group delivers each entry to one
consumer at a time, and redelivery happens only
through the idle threshold, so a worker that is alive keeps its task and a
worker that is not loses it. Duplicate execution remains possible in two cases,
both harmless: a worker stalled for more than 30 s (its late result reaches no
waiter), and a publish retried after a timeout whose first attempt had landed.

A handler error (the result could not be published within its 10 minute retry
window) leaves the task unacknowledged and stops refreshing it, so it is
reclaimed like a crashed worker's task. A worker that shuts down stops reading
at once, finishes what it is running within the 30 s drain, and puts tasks it
had read but not started back with a requeue script (`XADD` a copy, `XACK`,
`XDEL`) so they do not wait out the idle threshold.

## Retries and poison messages

Two different limits apply.

- Execution retries (`mq.dlq.max_retries`, default 3) are the worker's own: a
  failed execution is retried in place with backoff by `common::retry`, and the
  last failure publishes a failed `TaskReply::Completed` and a
  `MaxRetriesExceeded` dead letter, then acknowledges.
- Deliveries (`MAX_DELIVERIES`, 3) count how often the group handed the entry
  out, from the PEL delivery counter plus `n`. A task over the limit killed or
  stalled its worker every time. The reclaiming worker does not run it: it
  publishes a failed reply so the server stops waiting, and dead letters it
  with `RedeliveryExhausted`.

A task whose payload does not decode is dead lettered at once
(`DeserializationError`); retrying cannot fix it.

Dead lettering is one script: the ownership check, then `XADD <q>:dlq * p
<envelope>`, `XACK`, `XDEL` on the source entry. A poison entry therefore never stays in a work stream and is
never lost between the two. Servers consume `<q>:dlq` in the `servers` group,
write the envelope to `dead_letter_message` in a transaction, and then
acknowledge it. A failed write leaves the entry pending; any server reclaims it
after 30 s idle, so a database outage of up to about ten minutes delays dead
letters without losing them. A dead letter still unwritten after 20 deliveries
(its write fails deterministically, or panics) is logged in full at error
level and acknowledged, so it cannot loop forever. An envelope that does not
decode is logged and acknowledged, since no other consumer could read it
either. The admin DLQ UI reads the table, not the
stream.

## Batching

- One `XREADGROUP` per worker loop iteration returns up to the worker's free
  slots across both streams. Entries beyond the free slots (each stream applies
  `COUNT` on its own) wait in a local buffer and stay refreshed.
- One `XREAD` returns up to 512 replies. The server handles a reply in
  microseconds (a map lookup and a channel send), so a single reader per server
  keeps up with any burst the workers can produce.
- Publishing is one round trip per task (`XADD`, or the routing script). The
  operation batch path issues up to 32 of them at once per batch
  (`PUBLISH_WINDOW` in `services/operation_batch.rs`).

## Failure behaviour

- Every command has a deadline. The command connection uses a 5 s response
  timeout; a blocking read uses its block time plus 5 s. A half open socket
  therefore surfaces as a timeout, not a hang.
- On a timeout or I/O error the connection is dropped and the next command
  reconnects, with a 2 s connect timeout. Errors go to the caller, which
  decides whether to retry: publishes return `MqError`, and consume loops log,
  back off for one second and read again.
- A Redis restart closes every connection. Readers reconnect and re-issue their
  read; a `NOGROUP` reply (the restart lost the group) makes the reader
  recreate its groups and read again. The server's next heartbeat recreates its
  reply stream.
- While the last command on a server's queue connection failed to reach
  Redis, its claim fiber claims nothing: the work would only fail its dispatch
  and spend its retries, so it stays `Queued` for a replica that can judge it.

### What a Redis crash loses

The release template runs Redis with `appendonly yes` and `appendfsync
everysec`, so a crash loses at most the last second of writes. For the queue
that means:

- A task published in that second is gone, and so is its ledger entry. The
  server's next ledger check, within about 10 seconds, fails its waiter, and
  the submission is judged again through the system-error retry.
- An acknowledgement lost in that second redelivers a task that already ran.
  Its second result reaches no waiter.
- A reply lost in that second puts its task back in the ledger with the
  snapshot, and the task's entry back in its stream (or its worker's pending
  list): the task runs again and replies again.
- A Redis that comes back without its data (or after its stored TTLs passed)
  has no reply streams: each server's next renewal recreates its stream and
  fails every operation it was waiting for, and the plugins see them fail at
  once.

### Memory under `noeviction`

`maxmemory-policy noeviction` is required. Every stream entry is either unread
work or in flight work, and eviction would drop either silently. When Redis is
full, `XADD` fails with `OOM`, the publish returns an error, and the server
fails the operation batch instead of losing its tasks. Acknowledged entries are
deleted at once, each read deletes the replies before it, reply streams expire
with their server, and the DLQ stream drains into Postgres, so memory tracks
the live backlog. Two streams can still grow while something is stuck: a
stuck server's reply stream, and the DLQ stream during a database outage.
Both are capped with `XADD ... MAXLEN ~ 100000` (`REPLY_STREAM_MAXLEN`,
`DEAD_LETTER_MAXLEN`), which drops their oldest entries rather than stop every
write. The shared stream is never trimmed, since that would drop queued work;
the server logs an error when its backlog crosses 50 000.

## Clock rule

Every time comparison is made on the Redis clock:

- liveness is the presence of a TTL key, and a heartbeat's age is its TTL minus
  `PTTL`;
- claim idle times are PEL idle times;
- a task's queue wait is Redis `TIME`, pipelined right after the blocking read
  that returned it, minus the millisecond part of its entry id.

The server measures an operation's end to end time on its own monotonic clock,
from its publish to the arrival of the reply, so no timestamp crosses a node
boundary.

## Liveness

`common::liveness` holds the only definition: a node is live while its key
`<q>:heartbeat:<role>:<id>` exists (under the queue base name, so deployments
sharing a Redis under different names never see each other's nodes). The key has
a 15 s TTL that the node renews every 3 s with a 5 s deadline per renewal, so a
few slow renewals never expire it; it doubles as the node's ID claim. The system
dashboard, the pin routing script and the private stream drain script all ask
that one question of that one key. A worker's in flight tasks do not use it at
all: their PEL idle time answers whether their owner is alive. Replies do not
use it either: the reply stream outlives the liveness key on purpose.

## Observability

The server's queue depth sampler (`handlers/system.rs`) reports every stream
through one script call: `unread` (entries no consumer has taken) and `pending`
(the PEL size) for the shared stream, the DLQ stream and each live worker's
private stream, and the length of the server's own reply stream. They feed
`broccoli_mq_queue_depth` and `GET /api/v1/system/queues`. Workers count
redeliveries (`broccoli_mq_redeliveries_total`, labeled by stream role) and
dead letters (`broccoli_dlq_messages_total`), and record queue wait from entry
ids (`broccoli_mq_message_age_seconds`).

## Not supported

The scripts touch several keys at once, so the queue needs a single Redis
instance or a primary with replicas, not Redis Cluster.
