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
| fail | in flight | `SystemError` (finalized) | dispatch, an ended judging session (`fail_dispatch`) |
| requeue | in flight | `Queued`, new epoch, no owner | `release_leases`, `reclaim_abandoned` |
| exhaust | in flight | `SystemError` (finalized), dead-lettered | `reclaim_abandoned` |
| system-error retry | finalized `SystemError` that does not stand, current | `Queued`, new epoch | `retry_system_errors` |
| confirm | finalized `SystemError` that does not stand, current, budget spent | stands, shown on the submission | `retry_system_errors` |

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
  a `SystemError` that does not stand (`retry_system_errors`).

One lease-steal or stuck-job scan takes over everything it selects: it works
in transactions of `steal_batch_size` rows (256 for the detector) and repeats
them until one comes back short, so a dead server's whole in-flight set is
requeued within one scan interval however large it is.

Every lease age is compared on the database clock: claims and refreshes stamp
`NOW()`, and the scans compare against `NOW()`.

A server refreshes the leases of the units it still works on
(`judging::LiveDispatches`): a unit is live from the moment its claim takes
it, before its dispatch waits for a dispatcher permit or prepares its input,
until the last clone of its `Dispatch` is dropped. A unit the server owns but
no longer works on (its task was lost) keeps its old heartbeat, so its lease
lapses and a peer steals it.

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
room is left. A pass is one statement per kind of unit, judgements first: it
counts what the server has in flight against its in-flight cap, takes the
`Queued` rows that fit, marks them `Pending` and returns each with everything
its dispatch and the claim's metrics read (the submission's fixed columns, the
problem type, the time it waited). The dispatch starts right after the
statement, with nothing fallible in between, so a claimed unit never waits in
flight without a dispatch; a claim whose rows cannot be read is handed back to
`Queued` spending nothing (`release_claimed`). Wakes that arrive during a pass fold into one more pass, at
least 10 ms after the last one, so a burst is claimed in batches. Every
replica wakes on the same notification; `SKIP LOCKED` in the claim splits the
work. A one-second fallback tick covers notifications lost while the
listening connection was down; every reconnect also wakes the
fiber.

A replica claims only work it can run here. A unit needs exactly the
plugins it uses (`providers::Needs`, built by `PluginLifecycle::needs`):
its contest type's plugin, its problem type's evaluator, its language's
resolver, and its problem's checker format's checker only when the
evaluator declared at registration that it uses a checker (`uses_checker`,
recorded as the plugin's `unchecked_problem_types` in `plugin.governs`). A
problem whose evaluator checks no output (communication) is therefore not
parked by a disabled checker plugin, and work in a language whose plugin is
disabled is parked instead of failing. Every key resolves
by one rule, on every replica alike: to its fleet-wide provider, the plugin
with the first id among the enabled ones that provide it
(`PluginLifecycle::provider`, `providers::provider` in SQL, both over
`plugin.is_enabled` and `plugin.governs`). The dispatch runs each key on
that plugin only (`providers::resolve`), never on another provider that
happens to be loaded here, so one contest is never judged by two plugins
under two configs. The claim, the system-error retry, the steal and the
stuck detector take a unit only when this replica has every key's
fleet-wide provider loaded (`Loaded::here`, read from its registries, so the
check costs no round trip), or when no live replica has one loaded: then the
replica that takes the unit fails it visibly instead of it waiting unseen
(`providers::runs_here`). Every attempt would meet the same, so its system
error stands on the first attempt, never retried, as `PLUGIN_NOT_LOADED`
with a message naming the plugin, its role and the key
(`judging::fault::missing_plugin`). Each replica publishes what it has loaded, each key
with the plugins that registered it there, in its `plugin_replica.provides`
row. A unit
this replica cannot run yet stays `Queued` for a replica that can, or for
this one once it loads the plugin. A plugin that stops being loaded here
after a unit was claimed with everything loaded (a reload, an unload) is
this replica's fault, `PLUGIN_NOT_LOADED_HERE`: the attempt is handed back
without spending any budget (`Dispatch::not_loaded_here`).

## Judging sessions end in a verdict

A contest plugin judges a submission in a detached session: the host runs its
test cases and calls the plugin back with each result (see
`services/windowed_session.rs`). The plugin records the verdict from a
callback. The session loop returns how it ended, a `SessionEnd`, on every
exit, and the evaluate driver acts on it once:

- `Decided`: the plugin ended the session (finish, cancel, or its answer to
  the last result). A judgement it left open is finalized as `SystemError`
  with `PLUGIN_NO_VERDICT`.
- `Aborted`: a callback failed, or the plugin is gone (disabled, unloaded).
  The judgement is finalized as `SystemError` with `PLUGIN_SESSION_ABORTED`.
- `Lost`: the platform lost the attempt (see [Retry budgets](#retry-budgets)).
  The session ends before the next event reaches the plugin, so the plugin
  never judges from the lost operations, and the judgement goes back to
  `Queued`.
- `Interrupted`: this server is shutting down. Nothing is recorded; the
  shutdown releases the judgement and a peer judges it again.

Every end goes through `fail_dispatch`, fenced on the judgement's epoch and on
it being open, so a verdict the plugin already wrote stands. The after-judging
hooks fire after it, for a judgement it left terminal. While the plugin providing a contest
type is disabled, a submission to its contests is refused with
`503 CONTEST_TYPE_DISABLED`, with a `Retry-After` of 30 seconds (the work
is parked and serves once an administrator enables the plugin); `CONTEST_TYPE_UNAVAILABLE`
(with `Retry-After`) means only that the serving replica has not loaded the
plugin yet.

## A disabled plugin parks the work that needs it

A unit of work needs exactly the plugins it uses (`providers::Needs`, built by
`PluginLifecycle::needs`): the one that provides its contest type, the
evaluator of its problem's type, the resolver of its language, and the checker
of its problem's checker format only when the evaluator registered with
`uses_checker` (an evaluator that does not is recorded in
`unchecked_problem_types`). It is parked while every plugin that recorded one
of those keys (`plugin.governs`: `contest_types`, `problem_types`,
`checker_formats`, `languages`) is disabled fleet-wide; `providers::runnable` states that in SQL
(`judging::Unit::runnable` over a unit's scan) and
`PluginLifecycle::closed_by` in Rust. Parked work waits `Queued` and resumes
once a provider is enabled again. A new submission or code run that would be
parked is refused: `503 CONTEST_TYPE_DISABLED` for its contest type,
`503 PLUGIN_DISABLED` naming the evaluator, language or checker plugin, both with a
`Retry-After` of 30 seconds. A system error a plugin writes without a code is recorded as
`PLUGIN_ERROR`, so none is shown without one:

- The claim statement takes no judgement or code run of a parked type, so
  nothing is dispatched to a plugin that is not there.
- Unloading a plugin for good cuts every judging session it runs on that
  replica at once (`JudgingSessions::cut`), so each ends `Aborted` now rather
  than when its next result arrives. The replica that made the disable and
  every replica that applies it through its reconcile unload through the same
  `deactivate`.
- A judgement or code run whose dispatch fails while its type is parked (its
  session aborted when the plugin went away, or nothing was registered to
  dispatch it to) goes straight back to `Queued` under a fresh epoch,
  whatever the failure said (`judging::fail_dispatch`): it is never finalized
  as a `SystemError`, spends no budget, and the submission follows it in the
  same statement. The decision reads the fleet-wide `plugin` table, so the
  replica that made the disable and a replica that applied it through its
  reconcile hand the work back the same way.
- The reaper retries no system error of a parked type and confirms none:
  one still awaiting its decision goes back to `Queued` spending nothing, so
  the budget is not spent on attempts nothing can judge. One pick decides
  every awaiting error, so a batch never retries one of a parked type that a
  smaller batch would have handed back. The stuck-job detector hands back
  lapsed work of a parked type without spending a dispatch retry.
- A system error that stands (see [Retry budgets](#retry-budgets)) is a
  decision, not an attempt: it stays what the submission shows while its type
  is parked, whether the dispatch, the plugin or the reaper's confirmation
  made it stand.

A viewer whose submission the plugin governs sees its outcome withheld under
the contest's `plugin_outage`; staff see it `Queued`. Once the plugin is
enabled, the next claim pass (at most the claim fiber's 1 s fallback tick
later) takes the parked work.

## The submission row is a cache

`submission.status`, `judge_epoch`, `target_worker_id` and the judged output
columns are a projection of the current judgement. Each transition of a
current judgement rewrites them in the same statement or transaction. A
plugin's result write is one statement that writes the judgement and the
submission together.

One projection waits: a current judgement that finalized as a system error
that does not stand yet (`error_stands` is false), which the system-error
retry may still re-judge. A system error its submission already shows as its
final result (a terminal status at the judgement's epoch) is decided, stands
or not, and no retry reopens it (`awaits_retry_decision`).
The submission keeps showing the attempt in flight until the reaper decides
it within one scan (5 s): it requeues the judgement while the submission has
retries left, so the submission goes on to `Queued` and the next attempt; or
it confirms the error once the budget is spent, which marks it standing,
projects it and fires the after-judging hooks. Judging work runs those
hooks as part of itself (a contest plugin's submission call, a detached
session); the reaper and an applied judgement start them in a task of their
own that counts as judging work until they have run
(`spawn_after_judging_hooks`), so neither waits for a hook. A hook event
is delivered at least once fleet-wide. The replica that dispatches it first
records it in `hook_event` (its id, topic, payload and enablements) with a
lease, before its hooks run, so a duplicate dispatch on any replica, or after
a restart, is skipped. A dispatch that completes marks the event done. One
whose hooks could not answer (a busy plugin, a database outage), or that is
dropped, gives the lease back, without waiting on the database, so shutdown
is never held up. Every replica re-scans the table every 10 s: it runs again
each event that is not done and whose lease is free or has lapsed (10 min, so
the event of a replica that died runs again), records and runs the events it
could not record itself, and deletes completed events after 7 days. Plugins
take a hook event as idempotent. A client therefore never sees a terminal status
that a retry takes back: within an attempt the statuses only move forward,
and a terminal status is final.

## Judge epochs

A judge epoch is the fencing token of one dispatch of one unit, and the
identity of the attempt its results belong to. Plugin writes, dispatch
failures, result rows and the `after_judging:{id}:{epoch}` hook dedupe key
are keyed on it.

- Two judgements of one submission never share an epoch, and a judgement's
  epoch never moves backwards. The submission shows its current judgement's
  epoch, so applying an earlier judgement shows that judgement's epoch again.
- `submission.max_judge_epoch` is the allocator. Every judgement epoch is
  allocated by bumping it inside an UPDATE of the submission row, so the row
  lock serializes concurrent allocators. A code run has one row per run and
  bumps its own `judge_epoch`. Never compute an epoch in Rust.
- A requeue always takes a new epoch, so a write still arriving from the
  dispatch it replaced is rejected.
- A judgement's `test_case_result` rows carry the judgement's own epoch.
  Plugins join results to their judgement on it (IOI's
  `sj.judge_epoch = tcr.judge_epoch`), and a plugin keys the rows it stores
  for a judgement by (judgement, epoch) the same way. `apply_judgement` keeps
  the applied judgement's epoch, so every result row, the host's and every
  plugin's, stays attached without being re-keyed. The apply still allocates
  from `max_judge_epoch`, which locks the submission and names the apply's
  after-judging event (`after_judging:{id}:applied:{n}`), so re-applying a
  judgement fires its hooks again.
- Everything judging reads of a problem is versioned as one:
  `problem.inputs_version` counts every change to its test cases, its time
  and memory limits, its type, its checker format, its additional files
  (graders) and its problem-scoped and contest-problem configs (its checker
  source among them). Database triggers count them (`judging::inputs`), so
  every write path counts a change without knowing to. The dispatch reads the
  problem and its test cases in one snapshot and stamps the judgement with
  that snapshot's version (`judging::stamp_inputs_version`). Applying a
  judgement judged against other inputs than the problem has now is refused
  with `409 JUDGED_ON_OTHER_INPUTS` unless the request says `force=true`,
  since its verdicts would show beside inputs they were not judged with; a
  forced apply marks the judgement `judged_on_earlier_inputs`, which staff see
  in its Versions card.
- A judgement that stops being current while still in flight is finalized as
  `Superseded`, a status of its own: it never got a verdict, so it is
  history, not a system error. No retry takes it, and applying it is refused
  (`400 VALIDATION_ERROR`); the version history shows it as superseded,
  without an apply action.
- A plugin writes only the unit its own call was dispatched with. The
  dispatch (`judging::Dispatch`: the plugin it went to, the judgement or code
  run at its epoch, and its faults) rides in the call locals of the dispatch
  call, the calls nested in it and the detached callbacks it starts; every
  `host.submission.*` and `host.code_run.*` write, and a detached session
  that names a judgement to finish, is refused unless the caller owns that
  dispatch and names its unit. A write from a route, a timer, a checker or an
  evaluator of the same call tree is refused before it reaches the database.
- The database fence (`write_fence`) then admits the write only while that
  judgement is not finalized at the dispatched epoch.

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

Only a failure of judging the submission spends the system-error budget.
Everything that works for one dispatch (its plugin calls, their operations,
evaluate batches and detached sessions) shares the `DispatchFaults` of its
`Dispatch`, and
whatever meets a platform fault records it there: a publish the queue
refused (`QUEUE_UNREACHABLE`), an operation the queue lost
(`OPERATION_LOST`), a worker without a platform tool
(`PLATFORM_TOOL_MISSING`), a fleet with no live worker that takes work
(`NO_CAPABLE_WORKER`), a plugin the unit needs that stopped being loaded on
the replica that claimed it (`PLUGIN_NOT_LOADED_HERE`), a statement its
plugins ran that the database did not answer (`DATABASE_UNAVAILABLE`, as
`common::db::is_outage` decides it, recorded by the plugin SQL host functions
through `Dispatch::note_database_error`). Each of these faults is a hand-back
(`SystemErrorClass::HandBack`): the platform lost the attempt, and it never
becomes a verdict. The judging session ends as `Lost` before another result
reaches the plugin, a plugin write that would finalize the judgement is fenced
out like a stale dispatch's, and the dispatch ends through `fail_dispatch`,
which sends the judgement back to `Queued` under a fresh epoch spending
nothing. The submission follows in the same transaction, so no client and no
after-judging hook sees a `SystemError` that is then taken back.

A failure every attempt repeats stands at once (`SystemErrorClass::Stands`): a
contest plugin's refusal (`PLUGIN_REFUSED`), a problem its contest plugin says
cannot be judged (a `SystemError` written with `SubmissionUpdate::stands`,
for example IOI's and ICPC's `NO_TEST_CASES`), the problem's checker
failing (`CHECKER_FAILED`), or another program the problem supplies failing
(`PROBLEM_FAILED`). The judgement records it in `error_stands`, which
the reaper reads; the submission shows it at once. A checker plugin reports
the checker failing on its verdict
(`CheckerVerdict::checker_failure`, set by the standard testlib checker when
the checker does not compile, was killed by a signal, failed to start, or
exited with a code testlib gives no meaning or with a partial score that is not
a finite number); the host records it as the dispatch's fault, so the
system error carries the code and how the checker failed (staff only, like
every system-error diagnostic). An evaluator plugin reports a program of the
problem failing (a manager whose source the compiler rejects, or that
exits non-zero, dies of a signal of its own or prints no valid score, however
early the contestants exited, since contestants normally exit before the
manager scores; only a manager killed by SIGPIPE wrote to contestants that had
left, which is theirs, a wrong answer, and a manager that judges a short
interaction says so with its score) on the case's verdict (`TestCaseVerdict::problem_failure`, set by the
communication evaluator); the host records it the same way, as
`PROBLEM_FAILED`, where it receives the evaluator's verdict
(`evaluate_batch::dispatch`), so the system error the contest plugin ends the
submission with stands without the contest plugin knowing why. Any other
system error is retried within the budget.

A system error's class is decided where the failure is observed and recorded
with it (`DispatchFailure` for a dispatch, `judging::as_recorded` for a
plugin's write), never read back from its error code, so no code a plugin
writes, whatever its text, can pass for a host class. Every write a plugin
makes to a judgement or a code run goes through `as_recorded`: the plugin's own
error code is recorded as `<plugin_id>.<CODE>`, its word that a system error
stands counts only on the write that ends the unit in one, and a fault the
dispatch met decides the code of a system error, recorded bare, and whether it
stands. A plugin's system error is a terminal write with a `SystemError`
verdict, whichever terminal status it carries; the judging scans read a
finished row by the same rule. The UI translates a plugin's code from that
plugin's pack only.

How a dispatch's plugin call ended is one rule for both dispatchers
(`judging::CallEnd::of`): a call the host could not serve (its deadline,
admission, the plugin between loads, an instance it could not build) ends as
`PLUGIN_UNAVAILABLE`, any other call error as `PLUGIN_EXECUTION_ERROR`, a
refusal as `PLUGIN_REFUSED`, output the host cannot read as
`PLUGIN_INVALID_OUTPUT`. A host function that calls into another plugin
(a checker or language resolver) passes on the callee's unavailability: the
caller's call fails as unavailable too (`PluginInvoker::call_raw`). A call
that runs out of time is a judging failure, not a hand-back, so a unit that
always outlasts its deadline still ends.

A code run follows the judgement's rules: its plugin call carries the
dispatch's faults, a write that would finalize an attempt the platform lost is
fenced out, and `fail_dispatch` hands a lost or parked run back. A code run is
never retried as a system error, so every other failure stands.

A missing platform tool is the worker's fault, never the submission's. A worker
that lacks one reads no task at all: it stays live, and its heartbeat lists the
missing tools, so the System page shows it as not taking work and does not
count it among the workers taking work; while no live worker takes work, the
page says so, and its queued count shows the backlog waiting for one. A task reaches
it only if a tool goes missing while the worker holds it; the attempt is handed
back for a worker that has the tool. A pin never holds work for such a worker:
an admin cannot name it as a rejudge target, a task pinned to it goes to the
shared stream, and pinned tasks waiting for it when it stops taking work move
there within a reclaim tick (see [Message queue](./message-queue.md)). The
claim fiber claims only while some live worker takes work
(`WorkerSlots::takes_work`): with no live worker at all, or only workers that
take none, it claims nothing, so submissions wait `Queued` instead of reading
as being judged while nothing runs them. Work already dispatched when every
live worker stops taking work is handed back the same way: every operation the server waits for fails as
`NO_CAPABLE_WORKER` and gets its cancel key, and an operation started
meanwhile is refused when it publishes
(`consumers::operation_result::hand_back_while_none_accepting`). Workers
announce each change of whether they take work, so the claim fiber sees it at
once.

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

An operation task that a worker dead-letters (its execution retries or its
deliveries ran out) carries the judgement it ran for, so its row is filed
under that submission (`submission_id`, with the judgement id in the
payload). The statement that gives the submission a verdict that stands
resolves these rows with the submission's own, and a row that reaches the
database after that verdict (dead letters travel through the queue) is
resolved as it is filed. An operation row with no submission ran for no
judgement.

## Locks

Rows are locked in one order: a submission, then its judgements, then dead
letters. A statement that touches many rows (a scan, the lease refresh) takes
them in id order with `SKIP LOCKED`, so it never waits on a busy row and never
deadlocks with a rejudge or a plugin write.
