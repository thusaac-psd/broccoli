//! Host-side (non-plugin) reachability rules for the contest / problem /
//! submission surface.
//!
//! Ported **verbatim** from the pre-kernel access-control helpers so that a
//! later task can drop `host_decide` in behind those call sites without
//! changing what they allow. Nothing here is simplified, reordered, or
//! "cleaned up" relative to the source, including logic that looks
//! redundant or asymmetric - see the task report for a rule-by-rule
//! file:line mapping.
//!
//! Rules ported (numbering matches the task brief):
//!   1. `perm::CONTEST_MANAGE` short-circuits to `Allow`.
//!   2. Contest activation window: `activate_time.is_none_or(|at| at > now)
//!      || deactivate_time.is_some_and(|dt| dt <= now)` -> `Deny`. This
//!      exact predicate is ported from BOTH `check_contest_access` AND
//!      `require_contest_started` (byte-identical in the source, applied to
//!      the same contest/`now`) - see [`window_is_closed`],
//!      [`check_contest_access_decision`], and
//!      [`require_contest_started_window_decision`].
//!   3. `contest.is_public` short-circuits the participant check.
//!   4. Otherwise `contest_user` membership is required.
//!   5. A problem not in `contest_problem` for that contest -> `Deny`.
//!   6. Peer submissions: without `perm::SUBMISSION_VIEW_ALL`, another
//!      user's submission requires a `contest_id`, a passing contest-access
//!      check, participation, AND `contest.submissions_visible`.
//!   7. Soft-deleted entity -> `Deny` (contests are fetched with
//!      `find_active`, mirroring `find_contest`).
//!   8. Standalone (non-contest) problem/sample access, ported from
//!      `require_problem_read_access` (`utils/contest.rs:219-241`):
//!      missing/soft-deleted problem -> `Deny`; `perm::PROBLEM_CREATE` /
//!      `perm::PROBLEM_EDIT` -> `Allow`; `problem.is_public` -> `Allow`;
//!      otherwise fall through to rule 9.
//!   9. "Hidden draft reachable via a contest", ported from
//!      `can_access_problem_via_contest` (`utils/contest.rs:142-217`): a
//!      problem in zero contests -> `Deny` (checked BEFORE the
//!      `contest:manage` bypass - see [`decide_standalone_problem_access`]);
//!      `perm::CONTEST_MANAGE` -> `Allow`; otherwise `Allow` if the problem
//!      is attached to a contest that is both public and "open and started"
//!      (activation window open AND `start_time <= now`), or, failing that,
//!      `Allow` if the subject is a participant of any contest (public or
//!      not) attached to the problem that is "open and started".
//!      `Resource::Attachment` uses this same rule 8+9 pair, keyed on its
//!      `problem_id` only - see [`decide_standalone_problem_access`] for why
//!      `attachment_id` never enters the host decision.
//!
//! Deliberately **not** ported here (out of scope for this task - not in
//! the source list the task brief named):
//!   - `require_contest_started`'s `now < contest.start_time` check. That
//!     raises a distinct `400 VALIDATION_ERROR` business rule, not a
//!     reachability outcome, so it has no representation in `Decision`
//!     (`Allow` / `Redact` / `Deny`) and stays in `utils/contest.rs` for
//!     now.
//!   - `Resource::Clarification` (gated by `handlers/clarification.rs`,
//!     which this task was not told to read - Task 12's scope). Every
//!     `Resource::Clarification` therefore still fails CLOSED
//!     (`Decision::Deny`). This is a scope boundary, not a ported
//!     behaviour.
//!
//! # Batching
//!
//! `host_decide` answers a whole slice of `(subject, action, resource)`
//! queries with a bounded number of queries, not one per resource:
//!   - one query for the submissions referenced by any `Resource::
//!     Submission` in the batch (needed up front, since a submission's
//!     owner/contest_id aren't in the `Resource` itself);
//!   - one query for the problems referenced by a standalone (`contest_id:
//!     None`) `Resource::Problem`/`Resource::Sample`, or by any
//!     `Resource::Attachment` (needed for rule 8's `is_public` /
//!     existence check);
//!   - one query for `contest_problem` rows keyed by `problem_id` ONLY (not
//!     a `(contest_id, problem_id)` pair), covering the same standalone
//!     problem set - this is rule 9's "which contests is this problem
//!     attached to at all" lookup, distinct from the pair-membership query
//!     below;
//!   - one query for the contests referenced by the batch: directly
//!     (`Resource::Contest`), via a `Problem`/`Sample`'s `contest_id`, via a
//!     fetched submission's `contest_id` (when that submission actually
//!     needs the peer-visibility check), OR via the standalone
//!     `contest_problem` lookup above - a standalone problem's candidate
//!     contests must be folded into this fetch BEFORE it runs, or rule 9's
//!     window/public/participant checks would have nothing to look up;
//!   - one query for `contest_user` membership, covering every contest
//!     fetched above;
//!   - one query for `contest_problem` membership, covering every
//!     `(contest_id, problem_id)` pair referenced by `Problem`/`Sample`
//!     resources that carry a concrete `contest_id` (rule 5 - distinct from
//!     the problem_id-only lookup above, which answers a different
//!     question).
//!
//! Each query is skipped entirely when its target set is empty. The
//! `contest_user` query is intentionally NOT narrowed to only the contests
//! where `check_contest_access` would need it (i.e. it is also fetched for
//! public contests) - rule 6's participation check is unconditional
//! (independent of `is_public`), so narrowing the fetch to
//! `check_contest_access`'s needs would silently break peer-submission
//! visibility on public contests. See the task report for the exact query
//! count this achieves per scenario.

use std::collections::{HashMap, HashSet};

use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::entity::{contest, contest_problem, contest_user, problem, submission};
use crate::error::AppError;
use crate::utils::soft_delete::SoftDeletable;

use super::{Action, Decision, Resource, Subject};

/// See the module docs for the rule -> source mapping and the batching
/// strategy. Returns one [`Decision`] per entry of `resources`, in the same
/// order.
///
/// `submission_contest_ids` is an out-param: for every `Resource::
/// Submission` in `resources`, this inserts `submission_id ->
/// submission.contest_id` (which is itself `None` for a genuinely
/// contest-less submission) once that submission has been resolved by Query
/// 0 below. This surfaces a mapping `host_decide` already builds for its own
/// rule 6 check, rather than resolving it a second time - see
/// `VisibilityKernel::decide_batch`'s CRITICAL fix note
/// (`packages/server/src/visibility/mod.rs`, Task 7) for why the caller
/// needs it: a `Resource::Submission`'s contest is otherwise unknowable
/// outside this function, and losing it produces a confidently wrong (not
/// merely ambiguous) `QueryResource.contest_id` for plugins. Left untouched
/// (and callers pass an empty map) on the `admin_override` early return
/// below, since that path never resolves any submission and its decisions
/// never reach a plugin anyway.
///
/// Called from `VisibilityKernel::decide_batch`
/// (`packages/server/src/visibility/mod.rs`, Task 7), which sends it every
/// deduped, not-yet-memoized resource in a batch before any plugin is
/// consulted.
pub(crate) async fn host_decide<C: sea_orm::ConnectionTrait>(
    db: &C,
    subject: &Subject,
    _action: Action,
    resources: &[Resource],
    submission_contest_ids: &mut HashMap<i32, Option<i32>>,
) -> Result<Vec<Decision>, AppError> {
    if subject.is_admin_override() {
        return Ok(vec![Decision::Allow; resources.len()]);
    }

    let now = chrono::Utc::now();

    // Query 0: submissions referenced by the batch. Needed up front - a
    // `Resource::Submission` only carries an id, so its owner/contest_id
    // have to be resolved before Query 1 knows which contests it needs.
    let submission_ids: Vec<i32> = resources
        .iter()
        .filter_map(|r| match r {
            Resource::Submission(id) => Some(*id),
            _ => None,
        })
        .collect();
    let submissions: HashMap<i32, submission::Model> = if submission_ids.is_empty() {
        HashMap::new()
    } else {
        submission::Entity::find()
            .filter(submission::Column::Id.is_in(submission_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|s| (s.id, s))
            .collect()
    };
    submission_contest_ids.extend(submissions.values().map(|s| (s.id, s.contest_id)));

    // Query A: problems referenced by a standalone (`contest_id: None`)
    // `Resource::Problem`/`Resource::Sample`, or by any `Resource::
    // Attachment` - rule 8. `Resource::Attachment` is keyed on `problem_id`
    // only: `list_attachments`/`download_attachment` both gate on
    // `require_problem_read_access(problem_id)` alone (see
    // `handlers/attachment.rs`), never on `attachment_id`, so this set
    // collects `problem_id` regardless of which of the two resource kinds
    // carries it. Soft-delete-aware (`find_active`), mirroring
    // `require_problem_read_access`'s `find_active_by_id`.
    let standalone_problem_ids: HashSet<i32> = resources
        .iter()
        .filter_map(|r| match r {
            Resource::Problem {
                contest_id: None,
                problem_id,
            }
            | Resource::Sample {
                contest_id: None,
                problem_id,
            } => Some(*problem_id),
            Resource::Attachment { problem_id, .. } => Some(*problem_id),
            _ => None,
        })
        .collect();
    let problems: HashMap<i32, problem::Model> = if standalone_problem_ids.is_empty() {
        HashMap::new()
    } else {
        problem::Entity::find_active()
            .filter(
                problem::Column::Id
                    .is_in(standalone_problem_ids.iter().copied().collect::<Vec<_>>()),
            )
            .all(db)
            .await?
            .into_iter()
            .map(|p| (p.id, p))
            .collect()
    };

    // Query B: `contest_problem` rows keyed by `problem_id` ONLY, for the
    // same standalone problem set - rule 9's "which contests is this
    // problem attached to at all" lookup, ported from
    // `can_access_problem_via_contest`'s first query
    // (`utils/contest.rs:147-153`). Distinct from Query 3 below, which
    // answers "is this exact (contest_id, problem_id) pair a member".
    let problem_contest_ids: HashMap<i32, Vec<i32>> = if standalone_problem_ids.is_empty() {
        HashMap::new()
    } else {
        let mut map: HashMap<i32, Vec<i32>> = HashMap::new();
        for cp in contest_problem::Entity::find()
            .filter(
                contest_problem::Column::ProblemId
                    .is_in(standalone_problem_ids.iter().copied().collect::<Vec<_>>()),
            )
            .all(db)
            .await?
        {
            map.entry(cp.problem_id).or_default().push(cp.contest_id);
        }
        map
    };

    // Query 1: contests referenced by the batch - directly (`Resource::
    // Contest`), via a `Problem`/`Sample`'s `contest_id`, or via a fetched
    // submission's `contest_id` (only when that submission will actually
    // need the peer-visibility contest check - rule 6's owner/`submission:
    // view_all` bypasses never touch the contest at all, mirroring the
    // source's early-return shape). Soft-delete-aware (`find_active`),
    // mirroring `find_contest` - this is rule 7 for contests.
    let mut contest_ids: HashSet<i32> = HashSet::new();
    for r in resources {
        match r {
            Resource::Contest(id) => {
                contest_ids.insert(*id);
            }
            Resource::Problem {
                contest_id: Some(cid),
                ..
            }
            | Resource::Sample {
                contest_id: Some(cid),
                ..
            } => {
                contest_ids.insert(*cid);
            }
            _ => {}
        }
    }
    for sub in submissions.values() {
        let needs_contest_check = !subject.has_permission(perm::SUBMISSION_VIEW_ALL)
            && subject.user_id != Some(sub.user_id);
        if needs_contest_check && let Some(cid) = sub.contest_id {
            contest_ids.insert(cid);
        }
    }
    // Rule 9's candidate contests, folded in BEFORE the fetch below - a
    // standalone problem's "which of its contests are public/started/joined"
    // check (`decide_standalone_problem_access`) needs these contests in
    // `contests`, or it would have nothing to look up.
    for cids in problem_contest_ids.values() {
        contest_ids.extend(cids.iter().copied());
    }
    let contests: HashMap<i32, contest::Model> = if contest_ids.is_empty() {
        HashMap::new()
    } else {
        contest::Entity::find_active()
            .filter(contest::Column::Id.is_in(contest_ids.iter().copied().collect::<Vec<_>>()))
            .all(db)
            .await?
            .into_iter()
            .map(|c| (c.id, c))
            .collect()
    };

    // Query 2: `contest_user` membership for this subject, across every
    // contest fetched above. Fetched unconditionally (whenever the subject
    // is authenticated and at least one contest was fetched) rather than
    // only for contests where `check_contest_access` needs it - see the
    // module docs' "Batching" section for why narrowing this to rule 4's
    // needs alone would be wrong for rule 6.
    let member_of: HashSet<i32> = match subject.user_id {
        Some(uid) if !contests.is_empty() => contest_user::Entity::find()
            .filter(contest_user::Column::UserId.eq(uid))
            .filter(
                contest_user::Column::ContestId.is_in(contests.keys().copied().collect::<Vec<_>>()),
            )
            .all(db)
            .await?
            .into_iter()
            .map(|cu| cu.contest_id)
            .collect(),
        _ => HashSet::new(),
    };

    // Query 3: `contest_problem` membership for the (contest_id,
    // problem_id) pairs referenced by `Problem`/`Sample` resources that
    // carry a concrete `contest_id`. Rule 5. Filtered by both columns'
    // `IN` sets rather than an exact OR-of-pairs condition - this may
    // over-fetch a handful of unrelated rows when the batch spans several
    // contests and problems, but the per-resource decision below always
    // looks up the exact `(contest_id, problem_id)` pair it needs, so an
    // over-fetch cannot change a decision.
    let cp_pairs: Vec<(i32, i32)> = resources
        .iter()
        .filter_map(|r| match r {
            Resource::Problem {
                contest_id: Some(cid),
                problem_id,
            }
            | Resource::Sample {
                contest_id: Some(cid),
                problem_id,
            } => Some((*cid, *problem_id)),
            _ => None,
        })
        .collect();
    let problem_in_contest: HashSet<(i32, i32)> = if cp_pairs.is_empty() {
        HashSet::new()
    } else {
        let cids: Vec<i32> = cp_pairs.iter().map(|(c, _)| *c).collect();
        let pids: Vec<i32> = cp_pairs.iter().map(|(_, p)| *p).collect();
        contest_problem::Entity::find()
            .filter(contest_problem::Column::ContestId.is_in(cids))
            .filter(contest_problem::Column::ProblemId.is_in(pids))
            .all(db)
            .await?
            .into_iter()
            .map(|cp| (cp.contest_id, cp.problem_id))
            .collect()
    };

    Ok(resources
        .iter()
        .map(|r| {
            decide_one(
                subject,
                now,
                r,
                &contests,
                &member_of,
                &problem_in_contest,
                &submissions,
                &problems,
                &problem_contest_ids,
            )
        })
        .collect())
}

#[allow(clippy::too_many_arguments)]
fn decide_one(
    subject: &Subject,
    now: chrono::DateTime<chrono::Utc>,
    resource: &Resource,
    contests: &HashMap<i32, contest::Model>,
    member_of: &HashSet<i32>,
    problem_in_contest: &HashSet<(i32, i32)>,
    submissions: &HashMap<i32, submission::Model>,
    problems: &HashMap<i32, problem::Model>,
    problem_contest_ids: &HashMap<i32, Vec<i32>>,
) -> Decision {
    match resource {
        Resource::Contest(id) => decide_contest(subject, now, *id, contests, member_of),
        Resource::Problem {
            contest_id,
            problem_id,
        }
        | Resource::Sample {
            contest_id,
            problem_id,
        } => decide_problem_or_sample(
            subject,
            now,
            *contest_id,
            *problem_id,
            contests,
            member_of,
            problem_in_contest,
            problems,
            problem_contest_ids,
        ),
        Resource::Submission(id) => {
            decide_submission(subject, now, *id, contests, member_of, submissions)
        }
        Resource::Attachment { problem_id, .. } => decide_standalone_problem_access(
            subject,
            now,
            *problem_id,
            problems,
            problem_contest_ids,
            contests,
            member_of,
        ),
        // Out of scope for this task (Task 12) - see the module docs. Fail closed.
        Resource::Clarification(_) => Decision::Deny,
    }
}

/// Rule 2, ported byte-identically from BOTH `check_contest_access`
/// (`utils/contest.rs:30-34`) and `require_contest_started`
/// (`utils/contest.rs:77-81`) - the two source functions contain the exact
/// same predicate applied to the same contest/`now`.
fn window_is_closed(contest: &contest::Model, now: chrono::DateTime<chrono::Utc>) -> bool {
    contest.activate_time.is_none_or(|at| at > now)
        || contest.deactivate_time.is_some_and(|dt| dt <= now)
}

/// Ported from `check_contest_access` (`utils/contest.rs:21-46`): rules 1-4.
/// Rule 7 (soft-deleted contest) is enforced by the caller only ever
/// looking `contest_id` up in `contests`, which is populated via
/// `find_active`.
fn check_contest_access_decision(
    subject: &Subject,
    contest: &contest::Model,
    now: chrono::DateTime<chrono::Utc>,
    member_of: &HashSet<i32>,
) -> Decision {
    if subject.has_permission(perm::CONTEST_MANAGE) {
        return Decision::Allow;
    }
    if window_is_closed(contest, now) {
        return Decision::Deny;
    }
    if contest.is_public {
        return Decision::Allow;
    }
    if member_of.contains(&contest.id) {
        Decision::Allow
    } else {
        Decision::Deny
    }
}

/// Ported from `require_contest_started` (`utils/contest.rs:69-86`): the
/// `contest:manage` short-circuit and window-gate portion only. The
/// `now < contest.start_time` business-rule check in the source is
/// intentionally NOT reproduced here - see the module docs.
fn require_contest_started_window_decision(
    subject: &Subject,
    contest: &contest::Model,
    now: chrono::DateTime<chrono::Utc>,
) -> Decision {
    if subject.has_permission(perm::CONTEST_MANAGE) {
        return Decision::Allow;
    }
    if window_is_closed(contest, now) {
        Decision::Deny
    } else {
        Decision::Allow
    }
}

/// Mirrors `get_contest`'s gate (`find_contest` + `check_contest_access`
/// only - no `require_contest_started`).
fn decide_contest(
    subject: &Subject,
    now: chrono::DateTime<chrono::Utc>,
    id: i32,
    contests: &HashMap<i32, contest::Model>,
    member_of: &HashSet<i32>,
) -> Decision {
    match contests.get(&id) {
        // Rule 7: missing (soft-deleted, since `contests` was fetched with
        // `find_active`) or nonexistent contest -> Deny, mirroring
        // `find_contest`'s `NotFound`.
        None => Decision::Deny,
        Some(c) => check_contest_access_decision(subject, c, now, member_of),
    }
}

/// Mirrors `list_contest_problems` / `get_contest_problem_samples`, which
/// both call `check_contest_access` THEN `require_contest_started` on the
/// same contest before checking `find_contest_problem` /
/// `is_problem_in_contest` (rule 5). Both gates are applied here, in that
/// sequence.
#[allow(clippy::too_many_arguments)]
fn decide_problem_or_sample(
    subject: &Subject,
    now: chrono::DateTime<chrono::Utc>,
    contest_id: Option<i32>,
    problem_id: i32,
    contests: &HashMap<i32, contest::Model>,
    member_of: &HashSet<i32>,
    problem_in_contest: &HashSet<(i32, i32)>,
    problems: &HashMap<i32, problem::Model>,
    problem_contest_ids: &HashMap<i32, Vec<i32>>,
) -> Decision {
    let Some(cid) = contest_id else {
        // Standalone problem/sample access - rules 8-9.
        return decide_standalone_problem_access(
            subject,
            now,
            problem_id,
            problems,
            problem_contest_ids,
            contests,
            member_of,
        );
    };
    let Some(c) = contests.get(&cid) else {
        return Decision::Deny; // rule 7
    };
    if check_contest_access_decision(subject, c, now, member_of).is_denied() {
        return Decision::Deny;
    }
    if require_contest_started_window_decision(subject, c, now).is_denied() {
        return Decision::Deny;
    }
    if problem_in_contest.contains(&(cid, problem_id)) {
        Decision::Allow
    } else {
        Decision::Deny // rule 5
    }
}

/// Rules 8-9. Ported from `require_problem_read_access`
/// (`utils/contest.rs:219-241`) and `can_access_problem_via_contest`
/// (`utils/contest.rs:142-217`). Used both for standalone (non-contest)
/// `Resource::Problem`/`Resource::Sample` (`contest_id: None`) and for
/// `Resource::Attachment` - `list_attachments`/`download_attachment` both
/// gate on `require_problem_read_access(problem_id)` alone (see
/// `handlers/attachment.rs`), never on `attachment_id`, so this function
/// takes only a `problem_id`, matching the source exactly.
///
/// Preserves a source asymmetry verbatim: inside
/// `can_access_problem_via_contest`, the `contest_ids.is_empty()` check
/// happens BEFORE the `contest:manage` bypass (`utils/contest.rs:155-161`),
/// so a `contest:manage` holder without `problem:create`/`problem:edit` is
/// still denied a hidden problem attached to zero contests.
///
/// The source runs two separate window-filtered DB queries in sequence -
/// `has_public` (is_public AND open-and-started), then, only if that came
/// back empty, `started_contest_ids` (open-and-started, any visibility) for
/// the participant check. Both queries share the identical "open and
/// started" predicate (`utils/contest.rs:172-178`'s `within_window` plus
/// each call site's own `StartTime.lte(now)`), so this fuses them into one
/// in-memory filter pass over `contests` - `has_public`'s candidate set is a
/// strict subset of `started_contest_ids`'s, so filtering once and checking
/// `is_public` first, then membership, is bit-identical to the source's two
/// queries for every input.
fn decide_standalone_problem_access(
    subject: &Subject,
    now: chrono::DateTime<chrono::Utc>,
    problem_id: i32,
    problems: &HashMap<i32, problem::Model>,
    problem_contest_ids: &HashMap<i32, Vec<i32>>,
    contests: &HashMap<i32, contest::Model>,
    member_of: &HashSet<i32>,
) -> Decision {
    // `require_problem_read_access`: missing/soft-deleted problem -> Deny
    // (`problems` was fetched with `find_active`, mirroring
    // `find_active_by_id`).
    let Some(problem) = problems.get(&problem_id) else {
        return Decision::Deny;
    };
    if subject.has_permission(perm::PROBLEM_CREATE) || subject.has_permission(perm::PROBLEM_EDIT) {
        return Decision::Allow;
    }
    if problem.is_public {
        return Decision::Allow;
    }
    // `can_access_problem_via_contest`, inlined below.
    let no_contests: Vec<i32> = Vec::new();
    let candidate_contest_ids = problem_contest_ids.get(&problem_id).unwrap_or(&no_contests);
    if candidate_contest_ids.is_empty() {
        return Decision::Deny;
    }
    if subject.has_permission(perm::CONTEST_MANAGE) {
        return Decision::Allow;
    }
    // "Open and started": `window_is_closed`'s negation (activation window
    // open) AND `start_time <= now` - the extra condition
    // `can_access_problem_via_contest` applies on top of the plain window
    // check (`utils/contest.rs:165-171`'s comment explains why: a
    // not-yet-started or already-deactivated contest must not leak a hidden
    // problem's statement/samples/attachments here, even if it is public).
    let open_and_started: Vec<&contest::Model> = candidate_contest_ids
        .iter()
        .filter_map(|cid| contests.get(cid))
        .filter(|c| !window_is_closed(c, now) && c.start_time <= now)
        .collect();
    if open_and_started.iter().any(|c| c.is_public) {
        return Decision::Allow;
    }
    if open_and_started.iter().any(|c| member_of.contains(&c.id)) {
        return Decision::Allow;
    }
    Decision::Deny
}

/// Ported from `require_submission_visible`
/// (`handlers/submission/filter.rs:18-51`). Rule 6, plus rule 7 via the
/// non-owner branch's contest lookup.
fn decide_submission(
    subject: &Subject,
    now: chrono::DateTime<chrono::Utc>,
    id: i32,
    contests: &HashMap<i32, contest::Model>,
    member_of: &HashSet<i32>,
    submissions: &HashMap<i32, submission::Model>,
) -> Decision {
    let Some(sub) = submissions.get(&id) else {
        // No such submission (the source's raw `find_by_id` 404s the same
        // way before `require_submission_visible` ever runs).
        return Decision::Deny;
    };
    if subject.has_permission(perm::SUBMISSION_VIEW_ALL) {
        return Decision::Allow;
    }
    if subject.user_id == Some(sub.user_id) {
        // Owner bypass: the source skips the contest lookup ENTIRELY here,
        // so this stays Allow even if the parent contest has since been
        // soft-deleted. Preserved verbatim - see the task report.
        return Decision::Allow;
    }
    let Some(cid) = sub.contest_id else {
        return Decision::Deny;
    };
    let Some(c) = contests.get(&cid) else {
        return Decision::Deny; // rule 7
    };
    if check_contest_access_decision(subject, c, now, member_of).is_denied() {
        return Decision::Deny;
    }
    let is_participant = member_of.contains(&cid);
    if !is_participant || !c.submissions_visible {
        Decision::Deny
    } else {
        Decision::Allow
    }
}

#[cfg(test)]
mod tests {
    use common::SubmissionStatus;
    use sea_orm::{DatabaseBackend, MockDatabase};

    use super::*;
    use crate::extractors::auth::AuthUser;

    fn subject(user_id: i32, permissions: &[&str]) -> Subject {
        Subject::from_auth_user(&AuthUser {
            user_id,
            username: "viewer".into(),
            roles: vec![],
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        })
    }

    /// `hours` is an offset from now: negative = past, positive = future.
    fn contest_row(
        id: i32,
        is_public: bool,
        activate_hours: Option<i64>,
        deactivate_hours: Option<i64>,
        submissions_visible: bool,
    ) -> contest::Model {
        let now = chrono::Utc::now();
        contest::Model {
            id,
            title: "Contest".into(),
            description: "desc".into(),
            activate_time: activate_hours.map(|h| now + chrono::Duration::hours(h)),
            deactivate_time: deactivate_hours.map(|h| now + chrono::Duration::hours(h)),
            start_time: now - chrono::Duration::hours(2),
            end_time: now + chrono::Duration::hours(2),
            is_public,
            submissions_visible,
            show_compile_output: true,
            show_participants_list: true,
            contest_type: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    fn contest_user_row(contest_id: i32, user_id: i32) -> contest_user::Model {
        contest_user::Model {
            contest_id,
            user_id,
            registered_at: chrono::Utc::now(),
        }
    }

    fn contest_problem_row(contest_id: i32, problem_id: i32) -> contest_problem::Model {
        contest_problem::Model {
            contest_id,
            problem_id,
            label: "A".into(),
            position: 0,
        }
    }

    fn submission_row(id: i32, user_id: i32, contest_id: Option<i32>) -> submission::Model {
        let now = chrono::Utc::now();
        submission::Model {
            id,
            files: serde_json::json!({}),
            language: "cpp".into(),
            user_id,
            problem_id: 1,
            contest_id,
            contest_type: "ioi".into(),
            status: SubmissionStatus::Pending,
            verdict: None,
            compile_output: None,
            error_code: None,
            error_message: None,
            score: None,
            time_used: None,
            memory_used: None,
            judge_epoch: 0,
            target_worker_id: None,
            owner_server_id: None,
            lease_heartbeat_at: None,
            leased_at: None,
            retry_count: 0,
            created_at: now,
            judged_at: None,
        }
    }

    // -- admin_override: no queries at all --

    #[tokio::test]
    async fn admin_override_allows_everything_without_querying() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let resources = vec![
            Resource::Contest(1),
            Resource::Submission(2),
            Resource::Problem {
                contest_id: Some(1),
                problem_id: 3,
            },
        ];
        let decisions = host_decide(
            &db,
            &Subject::admin_override(),
            Action::Read,
            &resources,
            &mut HashMap::new(),
        )
        .await
        .expect("admin override must not touch the database");
        assert_eq!(
            decisions,
            vec![Decision::Allow, Decision::Allow, Decision::Allow]
        );
    }

    // -- rule 1: contest:manage short-circuits to Allow --

    #[tokio::test]
    async fn rule1_contest_manage_bypasses_window_and_participant_check() {
        // Not-yet-activated AND private: without contest:manage this is a
        // double-Deny (window closed, and not a member either).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, false, Some(1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::CONTEST_MANAGE]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    // -- rule 2: contest activation window --

    #[tokio::test]
    async fn rule2_window_closed_denies_even_when_public() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-3), Some(-1), true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule2_null_activate_time_is_out_of_window() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, None, None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- rule 3: contest.is_public short-circuits the participant check --

    #[tokio::test]
    async fn rule3_public_contest_allows_non_member() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            // Membership is still fetched by the batch (see module docs),
            // but comes back empty - the decision must not depend on it.
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    // -- rule 4: private contest requires contest_user membership --

    #[tokio::test]
    async fn rule4_private_contest_denies_non_member() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, false, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule4_private_contest_allows_member() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, false, Some(-1), None, true)]])
            .append_query_results([vec![contest_user_row(7, 1)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    // -- rule 5: problem must be in contest_problem for that contest --

    #[tokio::test]
    async fn rule5_problem_not_in_contest_denies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Problem {
                contest_id: Some(7),
                problem_id: 3,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule5_problem_in_contest_allows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .append_query_results([vec![contest_problem_row(7, 3)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Sample {
                contest_id: Some(7),
                problem_id: 3,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    // -- rule 6: peer submission visibility --

    #[tokio::test]
    async fn rule6_peer_submission_denied_without_participation_even_on_public_contest() {
        // Public, in-window, submissions_visible - but the viewer is not a
        // participant. `is_contest_participant` is unconditional in the
        // source, independent of `is_public`, so this must still Deny.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(5, 99, Some(7))]])
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Submission(5)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule6_peer_submission_denied_when_submissions_not_visible() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(5, 99, Some(7))]])
            .append_query_results([vec![contest_row(7, false, Some(-1), None, false)]])
            .append_query_results([vec![contest_user_row(7, 1)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Submission(5)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule6_peer_submission_allowed_when_participant_and_visible() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(5, 99, Some(7))]])
            .append_query_results([vec![contest_row(7, false, Some(-1), None, true)]])
            .append_query_results([vec![contest_user_row(7, 1)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Submission(5)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule6_peer_submission_denied_without_contest_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(5, 99, None)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Submission(5)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule6_submission_view_all_bypasses_ownership_and_participation() {
        // No contest/membership stub configured: `submission:view_all`
        // short-circuits before the batch ever needs to fetch the contest.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(5, 99, Some(7))]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::SUBMISSION_VIEW_ALL]),
            Action::Read,
            &[Resource::Submission(5)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule6_owner_bypasses_even_when_contest_would_be_soft_deleted() {
        // Owner bypass skips the contest lookup entirely (see
        // `require_submission_visible`): no contest/membership stub
        // configured, and contest_id points at a contest that doesn't
        // exist in this mock at all.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(5, 1, Some(999))]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Submission(5)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn submission_that_does_not_exist_denies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<submission::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Submission(404)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- rule 7: soft-deleted entity -> Deny --

    #[tokio::test]
    async fn rule7_soft_deleted_contest_denies_even_for_contest_manage() {
        // `find_active` excludes the row entirely - an empty result set is
        // indistinguishable from "never existed", matching `find_contest`.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<contest::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::CONTEST_MANAGE]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- Resource::Clarification is still out of scope (Task 12) --

    #[tokio::test]
    async fn clarification_resource_fails_closed_pending_later_task() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- rules 8-9: standalone problem/sample access, and Resource::Attachment --

    fn problem_row(id: i32, is_public: bool) -> problem::Model {
        let now = chrono::Utc::now();
        problem::Model {
            id,
            title: "Standalone".into(),
            content: "statement".into(),
            time_limit: 1000,
            memory_limit: 262_144,
            problem_type: "batch".into(),
            checker_format: "exact".into(),
            default_contest_type: "ioi".into(),
            show_test_details: false,
            is_public,
            submission_format: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    #[tokio::test]
    async fn rule8_hidden_standalone_problem_in_zero_contests_denies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule8_public_standalone_problem_in_zero_contests_allows() {
        // Mirrors `require_problem_read_access`'s `is_public` short-circuit,
        // but `host_decide` still issues the `contest_problem` prefetch
        // unconditionally for the whole batch - see the module docs'
        // "Batching" section. It comes back empty and is never consulted.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, true)]])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Sample {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule8_editor_reads_hidden_standalone_problem() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::PROBLEM_EDIT]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule8_missing_standalone_problem_denies_even_for_editor() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<problem::Model>::new()])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::PROBLEM_EDIT]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule9_contest_manage_denied_when_zero_contests_despite_permission() {
        // Preserves the source asymmetry: `contest_ids.is_empty()` is
        // checked BEFORE the `contest:manage` bypass inside
        // `can_access_problem_via_contest`.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::CONTEST_MANAGE]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule9_contest_manage_allows_even_when_the_contest_row_cannot_be_resolved() {
        // `contest_ids.is_empty()` (from the problem_id-only lookup) is
        // false, so `contest:manage` short-circuits before ever needing the
        // fetched `contest` row itself.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([vec![contest_problem_row(99, 1)]])
            .append_query_results([Vec::<contest::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::CONTEST_MANAGE]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule9_hidden_problem_reachable_via_public_started_contest_allows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([vec![contest_problem_row(7, 1)]])
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule9_hidden_problem_in_private_started_contest_denies_non_participant() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([vec![contest_problem_row(7, 1)]])
            .append_query_results([vec![contest_row(7, false, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule9_hidden_problem_in_private_started_contest_allows_participant() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([vec![contest_problem_row(7, 1)]])
            .append_query_results([vec![contest_row(7, false, Some(-1), None, true)]])
            .append_query_results([vec![contest_user_row(7, 1)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn rule9_hidden_problem_in_not_yet_started_public_contest_denies() {
        // Extra condition `can_access_problem_via_contest` applies on top of
        // the plain activation-window check: `start_time <= now`. An
        // activated-and-public-but-not-started contest must not leak the
        // problem, even though the plain window (rule 2) is open.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([vec![contest_problem_row(7, 1)]])
            .append_query_results([vec![contest::Model {
                start_time: chrono::Utc::now() + chrono::Duration::hours(1),
                ..contest_row(7, true, Some(-1), None, true)
            }]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- Resource::Attachment routes through the exact same rules 8-9,
    // keyed on problem_id only (never attachment_id) --

    #[tokio::test]
    async fn attachment_of_public_problem_allows_regardless_of_attachment_id() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, true)]])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Attachment {
                problem_id: 1,
                attachment_id: uuid::Uuid::from_u128(999),
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn attachment_of_hidden_problem_in_zero_contests_denies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Attachment {
                problem_id: 1,
                attachment_id: uuid::Uuid::from_u128(2),
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn attachment_of_hidden_problem_reachable_via_participant_membership() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .append_query_results([vec![contest_problem_row(7, 1)]])
            .append_query_results([vec![contest_row(7, false, Some(-1), None, true)]])
            .append_query_results([vec![contest_user_row(7, 1)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Attachment {
                problem_id: 1,
                attachment_id: uuid::Uuid::from_u128(2),
            }],
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    // -- positional matching across a mixed batch --

    #[tokio::test]
    async fn decisions_are_positional_matching_resources() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Query 0: submissions.
            .append_query_results([vec![submission_row(3, 1, Some(1))]])
            // Query 1: contests 1 (private, denies) and 2 (public, allows).
            .append_query_results([vec![
                contest_row(1, false, Some(-1), None, true),
                contest_row(2, true, Some(-1), None, true),
            ]])
            // Query 2: membership - empty (not a member of contest 1;
            // contest 2 is public so it doesn't matter; submission 3 is
            // owned by the subject so its contest was never added to the
            // fetch set).
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let resources = vec![
            Resource::Contest(1),
            Resource::Contest(2),
            Resource::Submission(3),
        ];
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &resources,
            &mut HashMap::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            decisions,
            vec![Decision::Deny, Decision::Allow, Decision::Allow]
        );
    }
}
