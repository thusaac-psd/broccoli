//! The judging lifecycle: the one owner of every state transition of a
//! submission, its judgements and a code run.
//!
//! # Dispatch units
//!
//! The dispatcher judges two kinds of rows, [`Unit::Judgement`]
//! (`submission_judgement`) and [`Unit::CodeRun`] (`code_run`). Both carry the
//! same dispatch columns: `status`, `judge_epoch`, `owner_server_id`,
//! `lease_heartbeat_at`, `leased_at` and `dispatch_retries`. A submission is
//! never dispatched itself: its current judgement is.
//!
//! ```text
//!            claim                 plugin writes / dispatch failure
//!  Queued ─────────▶ Pending ─▶ Compiling ─▶ Running ─────────────▶ Judged | CompilationError | SystemError
//!    ▲                  │            │           │                         (finalized)
//!    │   requeue        │            │           │                              │
//!    └──────────────────┴────────────┴───────────┘                              │
//!    │      (released, abandoned with retries left)     exhaust ─▶ SystemError  │
//!    └───────────────────────────────────────────────────────────────────────────┘
//!                 system-error retry (current judgement, SystemError only)
//! ```
//!
//! * **claim** (`Queued` → `Pending`) is the only way into flight. It stamps
//!   the claiming server as owner and both lease columns with the database
//!   clock. Only the claim fiber claims, so the in-flight cap and dispatcher
//!   admission apply to every dispatch, first or retried.
//! * **plugin writes** move an in-flight unit to `Compiling`/`Running` or
//!   finalize it, fenced on the unit id, its epoch and `is_finalized = FALSE`.
//! * **requeue** (in flight or finalized `SystemError` → `Queued`) gives the
//!   unit a fresh epoch, clears its output, its owner and its lease, and
//!   deletes its results. Every recovery is a requeue: a server releasing its
//!   own work ([`release_leases`]), a lapsed lease or a stuck job
//!   ([`reclaim_abandoned`]) and a system-error retry
//!   ([`retry_system_errors`]).
//! * **exhaust** finalizes an abandoned unit as `SystemError` once its
//!   dispatch-retry budget is spent and dead-letters it.
//!
//! A judgement additionally moves between current and non-current through
//! [`open_judgement`] (rejudge), [`apply_judgement`] and
//! [`discard_judgement`]. A submission and its first judgement are inserted
//! together by [`create_submission`], so every submission has a current
//! judgement from the moment it exists.
//!
//! # The submission row is a cache of its current judgement
//!
//! `submission.status`, `judge_epoch` and the judged-output columns are a
//! projection of the current judgement, rewritten from it in the same
//! statement or transaction as every transition of that judgement (see
//! `mirror`). Nothing else writes them.
//!
//! # Epochs
//!
//! A judge epoch is the fencing token of one dispatch of one unit. Every
//! judgement epoch is allocated from `submission.max_judge_epoch`, bumped in
//! SQL inside an UPDATE of the submission row, so concurrent allocators for
//! one submission serialize on its row lock and two judgements of a
//! submission never share an epoch. A code run has one row per run, so its
//! own `judge_epoch` is its allocator. Epochs are never computed in Rust.
//!
//! # Retry budgets
//!
//! [`RetryBudget`] is the one rule for every budget: another retry is allowed
//! while fewer than `max` retries were spent. A unit's `dispatch_retries`
//! counts requeues after it was abandoned (budget `max_dispatch_retries`);
//! a submission's `system_error_retries` counts system-error retries since
//! the last rejudge (budget `max_system_error_retries`).
//!
//! # Locks
//!
//! Statements lock a submission row before its judgements. Scans lock both
//! with `FOR UPDATE OF ... SKIP LOCKED`, so they never wait on either.

mod budget;
mod open;
mod plugin_writes;
mod unit;

#[cfg(test)]
mod tests;

pub(crate) use budget::RetryBudget;
pub use open::create_submission;
pub(crate) use open::{
    ApplyJudgement, DiscardJudgement, OpenJudgement, apply_judgement, discard_judgement,
    open_judgement,
};
pub(crate) use plugin_writes::{
    code_run_write, code_run_write_fence, judgement_write, write_fence,
};
pub(crate) use unit::{
    Abandoned, ClaimedCodeRun, ClaimedJudgement, claim_code_runs, claim_judgements, fail_dispatch,
    reclaim_abandoned, refresh_leases, retry_system_errors,
};
pub use unit::{Unit, UnitCounts, count_in_flight, release_leases};
