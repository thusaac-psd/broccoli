use chrono::Utc;
use common::SubmissionStatus;

use crate::entity::{code_run, submission};

mod handlers;
mod recovery;
mod scan;
#[cfg(test)]
mod tests;

pub use scan::*;

// Rows in these states have been claimed by a dispatcher and are either
// waiting for plugin-side dispatch or already evaluating. `Queued` is
// deliberately excluded: a deep durable queue is backlog, not a row-level
// failure. Old queued rows are observed as an aggregate dispatcher-health
// signal instead, without mutating them.
const STUCK_RECOVERY_STATUSES: [SubmissionStatus; 3] = [
    SubmissionStatus::Pending,
    SubmissionStatus::Compiling,
    SubmissionStatus::Running,
];
const QUEUED_OBSERVABILITY_THRESHOLD_SECS: i64 = 5 * 60;
const PENDING_ORPHAN_TIMEOUT_SECS: i64 = 5 * 60;
/// A finalized current judgement should propagate onto its submission row
/// within milliseconds (the normal finalize writes both rows back to back).
/// If it has not after this grace window, the submission is genuinely stuck
/// (a lost submission-row write) and is reconciled. Kept short - unlike the
/// 6-hour lease-stale timeout there is no "slow but alive" job to protect, only
/// denormalization lag - so recovery is fast enough for a live contest.
const RECONCILE_FINALIZED_GRACE_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StuckDisposition {
    Ignore,
    ObserveQueued,
    Recover,
}

fn detector_retry_lease(
    server_id: &str,
    now: chrono::DateTime<Utc>,
) -> (Option<String>, Option<chrono::DateTime<Utc>>) {
    (Some(server_id.to_string()), Some(now))
}

fn should_recover_directly(lease_steal_enabled: bool, judgement_is_current: Option<bool>) -> bool {
    !lease_steal_enabled || judgement_is_current == Some(false)
}

/// True when an owned row's immutable `leased_at` dispatch anchor is older than
/// the in-flight cap. NULL-safe on both sides: a row with no anchor (legacy /
/// pre-dispatch) or a disabled cap (`None`) never trips. This is the branch that
/// recovers a silently-wedged worker whose lease a live server keeps refreshing,
/// so `lease_heartbeat_at` alone never goes stale.
pub(super) fn inflight_capped(
    leased_at: Option<chrono::DateTime<Utc>>,
    inflight_cap_threshold: Option<chrono::DateTime<Utc>>,
) -> bool {
    matches!((leased_at, inflight_cap_threshold), (Some(leased), Some(cap)) if leased < cap)
}

// Pure decision fn over a status + owner id + five distinct time thresholds;
// the thresholds are separate instants, not a struct-shaped bag.
#[allow(clippy::too_many_arguments)]
fn stuck_disposition(
    status: &SubmissionStatus,
    owner_server_id: Option<&str>,
    created_at: chrono::DateTime<Utc>,
    lease_heartbeat_at: Option<chrono::DateTime<Utc>>,
    leased_at: Option<chrono::DateTime<Utc>>,
    queued_observability_threshold: chrono::DateTime<Utc>,
    pending_orphan_threshold: chrono::DateTime<Utc>,
    lease_stale_threshold: chrono::DateTime<Utc>,
    inflight_cap_threshold: Option<chrono::DateTime<Utc>>,
) -> StuckDisposition {
    if status == &SubmissionStatus::Queued {
        return if owner_server_id.is_none() && created_at < queued_observability_threshold {
            StuckDisposition::ObserveQueued
        } else {
            StuckDisposition::Ignore
        };
    }

    if !STUCK_RECOVERY_STATUSES.contains(status) {
        return StuckDisposition::Ignore;
    }

    match owner_server_id {
        None if status == &SubmissionStatus::Pending && created_at < pending_orphan_threshold => {
            StuckDisposition::Recover
        }
        None => StuckDisposition::Ignore,
        // Owned rows recover on EITHER a stale/missing lease heartbeat OR an
        // over-cap dispatch age. The cap term is what fires when a live server
        // keeps the heartbeat fresh for a worker that has silently died.
        Some(_)
            if lease_heartbeat_at.is_none_or(|heartbeat| heartbeat < lease_stale_threshold)
                || inflight_capped(leased_at, inflight_cap_threshold) =>
        {
            StuckDisposition::Recover
        }
        Some(_) => StuckDisposition::Ignore,
    }
}

/// Outcome of a stuck-job recovery attempt within a transaction.
enum StuckRecovery {
    /// Re-dispatch the (mutated) submission after commit.
    RedispatchSubmission {
        model: submission::Model,
        retry_count: i32,
    },
    /// Re-dispatch the (mutated) code run after commit.
    RedispatchCodeRun {
        model: code_run::Model,
        retry_count: i32,
    },
    /// Re-dispatch a deferred/current judgement after commit.
    RedispatchJudgement {
        submission: submission::Model,
        judgement_id: i32,
        fire_after_judging: bool,
        retry_count: i32,
    },
    /// Retry budget exhausted; row was terminally marked `SystemError`.
    Terminal,
    /// Row no longer needs handling (already terminal, vanished, or
    /// concurrently re-dispatched between SELECT and UPDATE).
    Skip,
}

fn stuck_code_run_message_id(code_run_id: i32) -> String {
    format!("stuck-code-run-{code_run_id}")
}

fn stuck_submission_judgement_message_id(judgement_id: i32) -> String {
    format!("stuck-submission-judgement-{judgement_id}")
}

fn stuck_retry_budget_exhausted(retry_count: i32, max_stuck_retries: u32) -> bool {
    let max = std::cmp::min(max_stuck_retries, i32::MAX as u32) as i32;
    retry_count > max
}

fn stuck_retries_exceeded_message(max_stuck_retries: u32) -> String {
    format!("Exceeded {max_stuck_retries} retries")
}
