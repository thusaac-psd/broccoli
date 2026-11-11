---
title: Judge epochs
sidebar_label: Judge epochs
sidebar_position: 2
---

# Judge epochs

A judge epoch is the fencing token of one dispatch of one judgement. Worker
results, dispatch failures (`mark_submission_system_error_with_epoch` in
`consumers/mod.rs`) and the `after_judging:{id}:{epoch}` hook dedupe key are
all keyed on it. Everything below lives in
`packages/server/src/services/judgement_lifecycle.rs`.

## Invariants

- Two judgements of one submission never share an epoch, and a submission's
  `judge_epoch` never moves backwards.
- `submission.judge_epoch` is the fence of the current judgement's dispatch.
  `submission.max_judge_epoch` is the allocator: the highest epoch ever handed
  out to the submission or any of its judgements. A deferred rejudge reserves
  an epoch there without moving `judge_epoch`, so the current judgement keeps
  running under its own fence.
- A judgement's `test_case_result` rows carry the judgement's own epoch.
  Plugins join results to their judgement on it (IOI's
  `sj.judge_epoch = tcr.judge_epoch`).
- A judgement that stops being current while still in flight is finalized as
  `SystemError` (`SUPERSEDED`, or `STUCK_JOB` for a stuck-job retry). It is
  never picked up again by the stuck detector or the steal scanner.

## How the code keeps them

- New epochs come only from `AllocateJudgeEpoch`, which sets
  `GREATEST(max_judge_epoch, judge_epoch) + 1` inside the UPDATE of the
  submission row. The row lock serializes concurrent allocators. Never compute
  an epoch as `epoch + 1` in Rust.
- Every new judgement row is opened by `open_judgement`: admin rejudge,
  deferred rejudge, DLQ retry, stuck-job retry and lease steal. It allocates
  the epoch, supersedes the previous current judgement and inserts the new one
  in one transaction. A rejudge or DLQ retry starts a new judging attempt and
  resets `retry_count` to 0. A re-dispatch spends one unit of it.
- Every re-dispatch of a submission's current judgement writes the submission
  row through `redispatch_current_submission`. A judgement's own retry count is
  carried over with `GREATEST`, so the submission's budget never goes down.
- `apply_judgement` gives the applied judgement a fresh epoch and moves its
  kept results to it in the same transaction.
- The plugin write path (`host_funcs/submissions.rs`) fences every write to a
  judgement on `JUDGEMENT_WRITE_FENCE`: the named judgement, of the named
  submission, at the dispatched epoch, still unfinalized. A deferred rejudge
  finishes its own judgement while an immediate one runs beside it. Only the
  current judgement updates the submission's cached verdict.

## Upgrading

Upgrade all server replicas together. A replica that predates
`max_judge_epoch` bumps epochs as `judge_epoch + 1` without touching the
allocator, so while old and new replicas run side by side two judgements can
end up with the same epoch. Migration `m0012_max_judge_epoch` adds the column
and backfills only the submissions that hold a judgement above their own
epoch.
