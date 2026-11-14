---
title: Judging lifecycle
sidebar_label: Judging lifecycle
sidebar_position: 2
---

# Judging lifecycle

Every state change of a submission, its judgements and a code run goes
through one module, `packages/server/src/services/judging/`. Code outside it
reads these rows but never writes their status, epoch, owner, lease or retry
counters.

## The dispatch unit

The dispatcher judges two kinds of rows: a `submission_judgement` and a
`code_run`. A submission is never dispatched itself. `create_submission`
inserts it together with its first judgement, so every submission has a
current judgement from the moment it exists.

| Transition | From | To | Writer |
| --- | --- | --- | --- |
| claim | `Queued` | `Pending`, owned, lease stamped | claim fiber |
| progress | in flight | `Compiling`, `Running` | plugin (`host.submission.update`, `host.code_run.update`) |
| finish | in flight | `Judged`, `CompilationError` (finalized) | plugin |
| fail | in flight | `SystemError` (finalized) | dispatch (`fail_dispatch`) |
| requeue | in flight | `Queued`, new epoch, no owner | `release_leases`, `reclaim_abandoned` |
| exhaust | in flight | `SystemError` (finalized), dead-lettered | `reclaim_abandoned` |
| system-error retry | finalized `SystemError`, current | `Queued`, new epoch | `retry_system_errors` |

Only the claim fiber moves work into flight, so the in-flight cap and
dispatcher admission apply to first dispatches and retries alike. Every
recovery is a requeue:

- a server requeues its own in-flight work at boot and on graceful shutdown,
  without spending a retry (`release_leases`);
- the lease steal requeues work whose lease lapsed for `lease_ttl_secs`, and
  the stuck-job detector requeues work with no heartbeat for
  `stuck_job_timeout_secs` or dispatched more than `max_inflight_secs` ago
  (`reclaim_abandoned`);
- the SystemError-retry reaper requeues a current judgement that finalized as
  a `SystemError` (`retry_system_errors`).

One lease-steal or stuck-job scan takes over everything it selects: it works
in transactions of `steal_batch_size` rows (256 for the detector) and repeats
them until one comes back short, so a dead server's whole in-flight set is
requeued within one scan interval however large it is.

Every lease age is compared on the database clock: claims and refreshes stamp
`NOW()`, and the scans compare against `NOW()`.

## Claiming

A claim fiber on each server claims work when it is told there is some. A
trigger on `submission_judgement` and `code_run` sends a Postgres
notification on the `broccoli_claim` channel from the transaction that
changes the row, so the submit path pays no extra statement and no path that
queues work can forget it:

- `queued` when a unit becomes `Queued`: a new submission or code run, a
  rejudge, a requeue, a system-error retry or a DLQ retry. Postgres folds
  identical notifications of one transaction into one.
- `freed:<server id>` when a unit owned by that server leaves flight. Only a
  fiber whose last pass was held back by its in-flight cap or by dispatcher
  admission acts on it.

A woken fiber claims a batch, and repeats while each pass fills its batch and
room is left. Wakes that arrive during a pass fold into one more pass, at
least 10 ms after the last one, so a burst is claimed in batches. Every
replica wakes on the same notification; `SKIP LOCKED` in the claim splits the
work. A one-second fallback tick covers notifications lost while the
listening connection was down; every reconnect also wakes the
fiber.

## The submission row is a cache

`submission.status`, `judge_epoch`, `target_worker_id` and the judged output
columns are a projection of the current judgement. Each transition of a
current judgement rewrites them in the same statement or transaction. A
plugin's result write is one statement that writes the judgement and the
submission together.

## Judge epochs

A judge epoch is the fencing token of one dispatch of one unit. Plugin
writes, dispatch failures and the `after_judging:{id}:{epoch}` hook dedupe key
are keyed on it.

- Two judgements of one submission never share an epoch, and a submission's
  `judge_epoch` never moves backwards.
- `submission.max_judge_epoch` is the allocator. Every judgement epoch is
  allocated by bumping it inside an UPDATE of the submission row, so the row
  lock serializes concurrent allocators. A code run has one row per run and
  bumps its own `judge_epoch`. Never compute an epoch in Rust.
- A requeue always takes a new epoch, so a write still arriving from the
  dispatch it replaced is rejected.
- A judgement's `test_case_result` rows carry the judgement's own epoch.
  Plugins join results to their judgement on it (IOI's
  `sj.judge_epoch = tcr.judge_epoch`). `apply_judgement` gives the applied
  judgement a fresh epoch and moves its kept results to it.
- A judgement that stops being current while still in flight is finalized as
  `SystemError` with `SUPERSEDED`.
- The plugin write fence (`write_fence`) admits a write only to the named
  judgement, of the named submission, at the dispatched epoch, while it is not
  finalized.

## Retry budgets

`RetryBudget` is the one rule: another retry is allowed while fewer than
`max` were spent.

- `submission_judgement.dispatch_retries` and `code_run.dispatch_retries`
  count requeues after an abandoned dispatch, against
  `server.max_dispatch_retries`. The lease steal and the stuck-job detector
  share this budget.
- `submission.system_error_retries` counts system-error retries since the
  last rejudge, against `server.max_system_error_retries`. Each such retry
  starts the judgement with a fresh dispatch budget. A rejudge resets it.

## Dead letters

A unit that spends its dispatch retries is dead-lettered under its subject:
its submission for a current judgement, otherwise the judgement or code run
itself (`DlqSubject`). A subject has at most one `dead_letter_message` row.
Dead-lettering it again reopens the row and appends the occurrence to its
retry history.

A submission's row resolves itself: when its current judgement finishes with a
verdict that stands (anything terminal but a system error), the same statement
resolves the row and leaves `resolved_by` NULL. A DLQ retry rejudges a
submission whose verdict is a `SystemError` and records the retry in the
history, leaving the row open until the rejudge recovers. Nothing re-judges a
stuck deferred judgement or code run, so an operator resolves those; discarding
the judgement resolves its row. An operator resolution names the operator.

## Locks

Rows are locked in one order: a submission, then its judgements, then dead
letters. A statement that touches many rows (a scan, the lease refresh) takes
them in id order with `SKIP LOCKED`, so it never waits on a busy row and never
deadlocks with a rejudge or a plugin write.
