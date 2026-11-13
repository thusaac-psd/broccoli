---
title: Plugin execution
sidebar_label: Plugin execution
sidebar_position: 5
---

# Plugin execution

Every call into a plugin guest goes through one path,
`PluginInvoker::call_raw` in `packages/plugin-core/src/traits.rs`, and one
model in `packages/plugin-core/src/executor.rs`. A call is admitted, runs on a
guest thread with a deadline, and gives its CPU back whenever it waits.

## Why the model looks like this

An Extism call runs on the thread that makes it until the guest returns, and
host functions bridge back into the async runtime with `block_on`. So a call
in progress always owns an OS thread, and dropping the future that awaits it
stops nothing. Two consequences shape everything below:

- Anything that queues must queue before the thread is taken. A call that
  waits for capacity while holding a blocking thread exhausts tokio's blocking
  pool under a slow plugin, and with it everything else on that pool.
- Holding capacity across a host wait deadlocks judging. The contest
  `on_submission` call (`services/submission_dispatch.rs`) sits in
  `get_next_evaluate_result` and `get_next_operation_result`
  (`host_funcs/evaluate.rs`, `host_funcs/dispatch.rs`) for the whole
  judgement, while its evaluator calls are separate calls that need capacity
  to run.

## Admission

`packages/plugin-core/src/admission.rs` holds `C` CPU tokens per server,
`TOKENS_PER_CORE` (2) per available core unless `plugin.max_concurrent_calls`
says otherwise. A token means "running guest code", not "call in progress".

| Class | Calls | Share |
|---|---|---|
| `Interactive` | visibility queries, request hooks, plugin HTTP routes | `ceil(C/2)` |
| `Background` | judging, operations, detached callbacks, timers, background hooks | `floor(C/2)` |

- A class may borrow the other's idle tokens. A token freed while both
  classes wait goes to the class below its share, and within that class to a
  resuming call first, so neither class starves the other under a flood.
- A queued waiter is an async future holding no thread. Dropping it, as a
  client that went away does, leaves the queue.

Every admitted call holds one guest thread until it returns, parked or not,
so each `CallKind` has its own cap on admitted calls:

| Kind | Calls | Cap |
|---|---|---|
| `Request` | interactive calls | `REQUESTS_PER_TOKEN` (8) per token |
| `Judging` | `on_submission`, `on_code_run` | this server's in-flight cap |
| `Work` | evaluators, detached callbacks, timers | this server's fan-out bound + `C` |
| `Hook` | background hooks | `C` |
| `Lifecycle` | `init`, no token | `LIFECYCLE_CAP` (4) |

The kinds are separate so one at its cap never holds back another: contest
calls parked for their verdicts fill the judging cap and leave the work cap,
where the evaluator calls that produce those verdicts run, untouched. The
server keeps the judging and work caps at its current shares
(`dispatcher::fanout::spawn_admission_sizing`).

The kind is a required argument. `CallOptions` has no default and the trait
has no convenience `call`; every site picks `interactive`, `judging`,
`background`, `background_hook`, `lifecycle` or `nested`. Hooks take theirs
from the dispatch site (`hooks::dispatch_hooks`), because one `PluginHook`
serves both the `before_submission` request path and background
notifications.

The pool lookup happens after admission, so a call queued across a reload
runs on the new version.

## Guest threads

Guests never run on tokio's blocking pool. They run on
`plugin_core::threads::GuestThreads`, which hands a job to an idle thread or
starts one and never queues. It does not need to: the threads in use are at
most one per admitted call plus one for that call's nested call
(`Caps::threads`, twice the sum of the caps), so a call never holds a token
while it waits for a thread, and the thread count is a sum of explicit caps.
Idle threads leave after 30 s.

## Parking

Every host function declares whether it waits on something outside the
guest (`host_funcs::Waits`) where it is built (`host_funcs::host_function`).
One that `Blocks` (SQL, storage, timers, submissions, config, result waits,
batch starts, cancels that reach Redis, nested calls) runs its whole body
inside `plugin_core::executor::parked`: the call's token goes back, and on
return it takes one back ahead of its class's queued calls
(`Admission::resume`). One that `Returns` at once (`log_info`, the
`register_*` registry updates, starting a detached session, cancelling an
operation batch) keeps its token, so a guest that logs never risks waiting
for a token it just held. A burst of contest submissions therefore all
proceed even when there are far more of them than tokens: each holds a
token only while its guest is actually computing.

The thread-local `CallScope` that carries the token, class, deadline and depth
is installed and restored by a guard, so a panicking guest or host function
cannot leave a stale scope behind.

## Nested calls

`checker:resolve`, `checker:interpret` and `language:resolve`
(`host_funcs/checker.rs`, `host_funcs/language.rs`) call back into plugins
with `CallOptions::nested()`.

- They run on a second guest thread. Running them inline would nest
  `block_on` inside `block_on` and panic.
- They skip admission. The parent is parked in that host function, so the
  nested call takes a token the way a resuming call does, and inherits the
  parent's class and the earlier of the parent's deadline and its own call
  timeout.
- Depth is capped at `MAX_DEPTH` (2). That stops A-calls-B-calls-A cycles and
  gives every admitted call at most one nested call, the second thread
  the thread budget reserves for it.
- One instance is never re-entered: each call leases its own instance, so a
  nested call into the same plugin gets a second store.

## Deadlines

Every call has a deadline. Interactive calls use the request's
(`crate::request_deadline`, set per request by a middleware around the
request timeout), hooks the earlier of that and 30 s, timers their claim
lease, judging and operations `plugin.call_timeout_secs`.

One process-wide thread (`packages/plugin-core/src/deadline.rs`) keeps a heap
of `(Instant, CancelHandle)` for every running call and cancels a guest whose
deadline passes. A guest parked in a host function traps when it returns to
guest code.

extism sends `TimerAction::Start` to its timer thread and only then arms the
store's epoch deadline (`set_epoch_deadline(1)`). A cancel handled in between
bumps the epoch too early and also removes the call's timer entry, so the
guest runs on and every repeat of the cancel is ignored. The deadline thread
therefore sends a call's first cancel no sooner than `START_GRACE` after the
call was registered, and repeats it, doubling the gap from `CANCEL_RETRY` up
to `CANCEL_RETRY_MAX`, until the call returns; a guest still running 5 s past
its first cancel is logged as a lost cancel. The manifest timeout, the call
timeout plus `TIMEOUT_BACKSTOP_MARGIN`, ends a guest the deadline thread never
cancelled. A lost cancel has no backstop: it removes the manifest timeout's
timer entry too. Only an extism fix that arms the epoch deadline before
registering the call closes it.

The host's long waits end at the call's deadline too:
`get_next_evaluate_result` and `get_next_operation_result` cut the wait a
guest asks for at `executor::current_deadline`.

A call still queued at its deadline, or cancelled at it, fails with
`PluginError::Unavailable`.

## Unavailable is never a decision

`Unavailable` says the server is busy, never anything about the request.

- `visibility::query_plugins` returns a `Result`. `Unavailable` becomes
  `AppError::PluginUnavailable`: `503 PLUGIN_UNAVAILABLE` with
  `Retry-After: 2`. It is never folded into `Deny`, which would turn a list
  into an empty page, a cursor into a 400 and a single read into a 404. Every
  other querier failure is a broken answer and still denies the batch.
- A blocking hook that is unavailable is the same 503, not a fail-closed
  rejection; a notify hook that is unavailable is skipped.
- Interactive calls are never retried server side. The web client treats the
  503 as a temporary state (`isTemporaryApiError` in
  `packages/web-sdk/src/api/errors.ts`) and TanStack Query retries it with
  backoff.
- Background callers keep their own semantics: a timer delivery cut at its
  lease is retried after the lease, a judging call that reached its deadline
  is a system error the SystemError reaper rejudges.

## Judging fan-out

The test cases a server has outstanding at workers are bounded by
`dispatcher::fanout::Fanout`: this server's share of
`TEST_CASES_PER_WORKER_SLOT` (2) per live worker slot across the fleet.
`WorkerSlots` is the one record of live capacity: the sum of `max_concurrency`
over the worker heartbeats and the number of live servers
(`common::liveness::live_workers_and_server_count`, one `SCAN` over both roles
and one pipelined read), refreshed by one task every 15 s. The claim fiber's
in-flight cap is the same kind of share: the configured per-server floor, or
this server's share of 8 per live worker slot when that is larger. Until any
capacity is seen, one slot per local core is assumed. Inside one operation
batch, publishes run in a fixed window of 32.

## Memory

Pools (`packages/plugin-core/src/pool.rs`) build instances on demand and keep
returned ones, most recently used first. Memory is bounded globally by
`MemoryBudget` (`packages/plugin-core/src/memory.rs`):

- The signal is the process's resident set from `/proc/self/statm`, read at
  most every 50 ms when an instance is returned.
- Above the budget, idle instances are dropped least recently used first
  across every plugin, keeping `WARM_PER_PLUGIN` per plugin.
- The budget holds pools by weak reference, so a pool a reload replaced is
  still trimmed until its last lease returns.
- The budget is 3/4 of the memory limit of the process's cgroup (cgroup v2:
  the `0::` path in `/proc/self/cgroup`, then `memory.max` of that cgroup or
  its nearest limited parent; cgroup v1: `hierarchical_memory_limit`), or 1/4
  of `MemTotal` without one, because the single-host compose shares the
  machine. `plugin.memory_budget` overrides it.
- Each instance keeps a runaway cap on its linear memory (`memory_max`), half
  the budget and at most 1.5 GiB.

`broccoli_plugin_instances_live` and `broccoli_plugin_calls_in_flight` are
per-plugin gauges; `broccoli_plugin_admission_queued_total` and
`broccoli_plugin_admission_wait_duration_seconds` show contention.

## Login

Argon2 runs on its own threads (`utils/hash.rs`), one per core, never on
tokio's blocking pool, so a plugin flood cannot delay login.

## Settings

`plugin.max_concurrent_calls`, `plugin.memory_budget` and
`plugin.call_timeout_secs` are the only knobs; the first two size themselves.
The timer lease is the call timeout plus `LEASE_MARGIN` (30 s). Keys that used to
tune pools and fan-out are listed in `config::REMOVED_KEYS`; boot warns about
any that is still set.
