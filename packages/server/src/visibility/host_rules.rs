//! Host-side (non-plugin) reachability rules for the contest, problem,
//! submission and clarification surface.
//!
//! The rules, in the order a decision applies them:
//!   1. `perm::CONTEST_MANAGE` short-circuits to `Allow`.
//!   2. A contest outside its activation window (its
//!      [`ContestPhase`](crate::utils::contest::ContestPhase) is not active)
//!      -> `Deny`; see [`BatchFacts::check_contest_access_decision`] and
//!      [`BatchFacts::require_contest_started_window_decision`].
//!   3. `contest.is_public` short-circuits the participant check.
//!   4. Otherwise `contest_user` membership is required.
//!   5. A problem not in `contest_problem` for that contest -> `Deny`.
//!   6. Peer submissions: without `perm::SUBMISSION_VIEW_ALL`, another
//!      user's submission requires a `contest_id`, a passing contest-access
//!      check, participation, AND `contest.submissions_visible`.
//!   7. Soft-deleted entity -> `Deny` (contests are fetched with
//!      `find_active`).
//!   8. Standalone (non-contest) problem access: a missing or soft-deleted
//!      problem -> `Deny`; `perm::PROBLEM_CREATE` / `perm::PROBLEM_EDIT` ->
//!      `Allow`; `problem.is_public` -> `Allow`; otherwise rule 9 decides.
//!   9. A hidden problem is reachable through its contests: a problem in
//!      zero contests -> `Deny` (checked BEFORE the `contest:manage` bypass;
//!      see [`BatchFacts::decide_standalone_problem_access`]);
//!      `perm::CONTEST_MANAGE` -> `Allow`; otherwise `Allow` if the problem
//!      is attached to a contest that is both public and started
//!      ([`ContestPhase::has_started`](crate::utils::contest::ContestPhase::has_started):
//!      active and running or ended), or, failing that, if the subject is a
//!      participant of any started contest (public or not) attached to the
//!      problem.
//!      Samples and attachments are parts of their problem: a
//!      `Resource::Sample` or `Resource::Attachment` gets exactly the
//!      decision its problem would get under the same `contest_id` scope
//!      (rules 1-5 with a contest, rules 8-9 without), and `attachment_id`
//!      never enters the host decision; see
//!      [`BatchFacts::decide_problem_or_sample`].
//!      Rule 9 widens READ only: under `Action::Submit` a standalone
//!      decision stops after rule 8, so a hidden problem is denied there
//!      even when one of its contests admits reading it; see
//!      [`standalone_reachable_via_contest`].
//!  10. `Resource::Clarification`; see [`BatchFacts::decide_clarification`].
//!      A missing clarification, or one where the subject is neither
//!      `contest:manage`, the author, the recipient, nor looking at a public
//!      row, -> `Deny`. A participant (admin, author or recipient) ->
//!      `Allow`, and so is anyone looking at a public row.
//!      Creating a clarification has no row to decide about yet, so its
//!      reachability gate is `Resource::Contest` under `Action::Clarify`,
//!      which rules 1-4 decide.
//!
//! Outside these rules, by design:
//!   - `require_contest_started`'s `now < contest.start_time` check. It
//!     raises a distinct `400 VALIDATION_ERROR` business rule, not a
//!     reachability outcome, so it has no representation in `Decision`
//!     (`Allow` / `Redact` / `Deny`) and lives in `utils/contest.rs`.
//!   - The per-element filter over a clarification's `replies` array, which
//!     OMITS non-public replies for a non-participant rather than blanking a
//!     field. A `FieldMask` path blanks a field uniformly across every array
//!     element (`mask.rs`'s `*` wildcard) but cannot drop only SOME elements
//!     by a per-element predicate, so the clarification handler applies it
//!     while building the response.
//!   - `create_clarification`'s admin/type gate, `recipient_id` forcing,
//!     and `is_public` forcing. These are business rules (`403
//!     PermissionDenied`), not reachability, and run in the handler after
//!     the `Action::Clarify` kernel gate.
//!
//! # Batching
//!
//! `host_decide` answers a whole slice of `(subject, action, resource)`
//! queries with a bounded number of queries, not one per resource:
//!   - one query for the submissions referenced by any `Resource::
//!     Submission` in the batch (needed up front, since a submission's
//!     owner/contest_id aren't in the `Resource` itself);
//!   - one query for the problems referenced by a standalone (`contest_id:
//!     None`) problem part - `Resource::Problem`, `Resource::Sample` or
//!     `Resource::Attachment` (needed for rule 8's `is_public` / existence
//!     check);
//!   - one query for `contest_problem` rows keyed by `problem_id` ONLY (not
//!     a `(contest_id, problem_id)` pair), covering the same standalone
//!     problem set - this is rule 9's "which contests is this problem
//!     attached to at all" lookup, distinct from the pair-membership query
//!     below;
//!   - one query for the contests referenced by the batch: directly
//!     (`Resource::Contest`), via a problem part's `contest_id`, via a
//!     fetched submission's `contest_id` (when that submission actually
//!     needs the peer-visibility check), OR via the standalone
//!     `contest_problem` lookup above - a standalone problem's candidate
//!     contests are folded into this fetch BEFORE it runs, since rule 9's
//!     window/public/participant checks look them up;
//!   - one query for `contest_user` membership, covering every contest
//!     fetched above;
//!   - one query for `contest_problem` membership, covering every
//!     `(contest_id, problem_id)` pair referenced by problem parts that
//!     carry a concrete `contest_id` (rule 5 - distinct from
//!     the problem_id-only lookup above, which answers a different
//!     question);
//!   - one query for the clarifications referenced by any `Resource::
//!     Clarification` in the batch (rule 10) - a `Resource::Clarification`
//!     only carries an id, so its author/recipient/is_public/contest_id
//!     aren't in the `Resource` itself, as with submissions;
//!   - one query for the replies of every clarification resolved above,
//!     ordered by `created_at`, to find each one's LATEST reply's own
//!     `is_public` flag (rule 10's `Redact` gate).
//!
//! Each query is skipped entirely when its target set is empty. The
//! `contest_user` query is NOT narrowed to only the contests where
//! `check_contest_access` needs it (it is also fetched for public contests):
//! rule 6's participation check is unconditional (independent of
//! `is_public`), so narrowing the fetch would break peer-submission
//! visibility on public contests.

use std::collections::{HashMap, HashSet};

use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::entity::{
    clarification, contest, contest_problem, contest_user, problem, submission,
};
use crate::error::AppError;
use crate::utils::contest::contest_phase;
use crate::utils::soft_delete::SoftDeletable;

use super::{Action, Decision, Resource, Subject};

/// What [`host_decide`] learns about the contests of a batch's resources,
/// for the plugin step that follows it.
#[derive(Debug, Default)]
pub(crate) struct HostScopes {
    /// Input: also fetch the contest of every submission and clarification,
    /// so `contest_types` covers each resource's contest. Set while a
    /// visibility querier scoped to contest types is loaded; the contests
    /// join the fetch the batch makes anyway.
    pub resolve_contests: bool,
    /// `submission_id -> submission.contest_id` for every `Resource::
    /// Submission` resolved by Query 0 (itself `None` for a contest-less
    /// submission). A `Resource::Submission`'s contest is otherwise
    /// unknowable outside this function, and `VisibilityKernel::decide_batch`
    /// stamps it onto each plugin `QueryResource.contest_id`.
    pub submission_contest_ids: HashMap<i32, Option<i32>>,
    /// `clarification_id -> clarification.contest_id` for every `Resource::
    /// Clarification` resolved by Query C. A clarification always belongs to
    /// exactly one contest, so the value is a plain `i32`.
    pub clarification_contest_ids: HashMap<i32, i32>,
    /// `contest_id -> contest_type` for every active contest the batch
    /// fetched.
    pub contest_types: HashMap<i32, Option<String>>,
}

/// See the module docs for the rules and the batching strategy. Returns one
/// [`Decision`] per entry of `resources`, in the same order, and fills
/// `scopes` (see [`HostScopes`]). The maps stay empty on the
/// `admin_override` early return, which resolves nothing and never reaches
/// a plugin.
///
/// `action` matters only for standalone (`contest_id: None`) problem-keyed
/// resources: it decides whether rule 9 applies (see
/// [`standalone_reachable_via_contest`]). Every other rule is the same for
/// every action.
///
/// Called from `VisibilityKernel::decide_batch`, which sends it every
/// deduped, not-yet-memoized resource in a batch before any plugin is
/// consulted.
pub(crate) async fn host_decide<C: sea_orm::ConnectionTrait>(
    db: &C,
    subject: &Subject,
    action: Action,
    resources: &[Resource],
    scopes: &mut HostScopes,
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
    scopes
        .submission_contest_ids
        .extend(submissions.values().map(|s| (s.id, s.contest_id)));

    // Query A: problems referenced by a standalone (`contest_id: None`)
    // problem part - rule 8. Samples and attachments are parts of their
    // problem, so this set collects the `problem_id` of every standalone
    // `Problem`, `Sample` and `Attachment` alike (`attachment_id` never
    // enters the host decision). Soft-delete-aware (`find_active`),
    // mirroring `require_problem_read_access`'s `find_active_by_id`.
    let standalone_problem_ids: HashSet<i32> = resources
        .iter()
        .filter_map(Resource::problem_scope)
        .filter(|scope| scope.contest_id.is_none())
        .map(|scope| scope.problem_id)
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
    // problem attached to at all" lookup. Distinct from Query 3 below, which
    // answers "is this exact (contest_id, problem_id) pair a member".
    // Skipped when `action` never reaches rule 9 (`Action::Submit`): nothing
    // would read the map, and leaving it empty also keeps this batch's
    // contest/`contest_user` queries from fetching rule 9's candidates.
    let rule9_may_apply =
        !standalone_problem_ids.is_empty() && standalone_reachable_via_contest(action);
    let problem_contest_ids: HashMap<i32, Vec<i32>> = if !rule9_may_apply {
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

    // Query C: clarifications referenced by any `Resource::Clarification` in
    // the batch - rule 10. A `Resource::Clarification` only carries an id,
    // so its author/recipient/is_public/contest_id have to be resolved
    // before a decision can be made, mirroring Query 0 for submissions.
    // `clarification` has no soft-delete column, so a plain `find_by_id`-
    // style `IN` lookup is exact - a missing row here means "does not
    // exist", nothing more.
    let clarification_ids: Vec<i32> = resources
        .iter()
        .filter_map(|r| match r {
            Resource::Clarification(id) => Some(*id),
            _ => None,
        })
        .collect();
    let clarifications: HashMap<i32, clarification::Model> = if clarification_ids.is_empty() {
        HashMap::new()
    } else {
        clarification::Entity::find()
            .filter(clarification::Column::Id.is_in(clarification_ids))
            .all(db)
            .await?
            .into_iter()
            .map(|c| (c.id, c))
            .collect()
    };
    scopes
        .clarification_contest_ids
        .extend(clarifications.values().map(|c| (c.id, c.contest_id)));


    // Query 1: contests referenced by the batch - directly (`Resource::
    // Contest`), via a problem part's `contest_id`, or via a fetched
    // submission's `contest_id` when that submission needs the
    // peer-visibility contest check (rule 6's owner/`submission:view_all`
    // bypasses never read the contest). With `scopes.resolve_contests`, every
    // submission's and clarification's contest joins this same fetch, for
    // the plugin step's routing; no decision reads them. Soft-delete-aware
    // (`find_active`) - this is rule 7 for contests.
    let mut contest_ids: HashSet<i32> = resources
        .iter()
        .filter_map(|r| match r {
            Resource::Contest(id) => Some(*id),
            _ => r.problem_scope().and_then(|scope| scope.contest_id),
        })
        .collect();
    for sub in submissions.values() {
        let needs_contest_check = !subject.has_permission(perm::SUBMISSION_VIEW_ALL)
            && subject.user_id != Some(sub.user_id);
        if (needs_contest_check || scopes.resolve_contests)
            && let Some(cid) = sub.contest_id
        {
            contest_ids.insert(cid);
        }
    }
    // Only for the plugin step's routing: no host rule reads a
    // clarification's contest.
    if scopes.resolve_contests {
        contest_ids.extend(clarifications.values().map(|c| c.contest_id));
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
    scopes
        .contest_types
        .extend(contests.values().map(|c| (c.id, c.contest_type.clone())));

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
    // problem_id) pairs referenced by problem parts that carry a concrete
    // `contest_id`. Rule 5. Filtered by both columns'
    // `IN` sets rather than an exact OR-of-pairs condition - this may
    // over-fetch a handful of unrelated rows when the batch spans several
    // contests and problems, but the per-resource decision below always
    // looks up the exact `(contest_id, problem_id)` pair it needs, so an
    // over-fetch cannot change a decision.
    let cp_pairs: Vec<(i32, i32)> = resources
        .iter()
        .filter_map(Resource::problem_scope)
        .filter_map(|scope| Some((scope.contest_id?, scope.problem_id)))
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

    let facts = BatchFacts {
        subject,
        now,
        contests,
        member_of,
        problem_in_contest,
        submissions,
        problems,
        problem_contest_ids,
        clarifications,
    };
    Ok(resources.iter().map(|r| facts.decide(action, r)).collect())
}

/// Everything the host rules read to decide one batch, fetched up front by
/// [`host_decide`] so each per-resource decision is pure.
struct BatchFacts<'a> {
    subject: &'a Subject,
    now: chrono::DateTime<chrono::Utc>,
    /// Active (not soft-deleted) contests the batch references.
    contests: HashMap<i32, contest::Model>,
    /// The contests among `contests` the subject is registered in.
    member_of: HashSet<i32>,
    /// `(contest_id, problem_id)` links among the batch's scoped problems.
    problem_in_contest: HashSet<(i32, i32)>,
    submissions: HashMap<i32, submission::Model>,
    /// Active problems the batch references outside a contest.
    problems: HashMap<i32, problem::Model>,
    /// The contests each of `problems` belongs to.
    problem_contest_ids: HashMap<i32, Vec<i32>>,
    clarifications: HashMap<i32, clarification::Model>,
}

impl BatchFacts<'_> {
    fn decide(&self, action: Action, resource: &Resource) -> Decision {
        match resource {
            Resource::Contest(id) => self.decide_contest(*id),
            Resource::Problem {
                contest_id,
                problem_id,
            }
            | Resource::Sample {
                contest_id,
                problem_id,
            }
            | Resource::Attachment {
                contest_id,
                problem_id,
                ..
            } => self.decide_problem_or_sample(action, *contest_id, *problem_id),
            Resource::Submission(id) => self.decide_submission(*id),
            Resource::Clarification(id) => self.decide_clarification(*id),
        }
    }

    /// Rules 1-4, as `check_contest_access` applies them.
    /// Rule 7 (soft-deleted contest) is enforced by the caller only ever
    /// looking `contest_id` up in `contests`, which is populated via
    /// `find_active`.
    fn check_contest_access_decision(&self, contest: &contest::Model) -> Decision {
        if self.subject.has_permission(perm::CONTEST_MANAGE) {
            return Decision::Allow;
        }
        if !contest_phase(contest, self.now).is_active() {
            return Decision::Deny;
        }
        if contest.is_public {
            return Decision::Allow;
        }
        if self.member_of.contains(&contest.id) {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }

    /// The `contest:manage` short-circuit and window gate of
    /// `require_contest_started`. Its `now < contest.start_time` business
    /// rule is not a reachability outcome; see the module docs.
    fn require_contest_started_window_decision(&self, contest: &contest::Model) -> Decision {
        if self.subject.has_permission(perm::CONTEST_MANAGE) {
            return Decision::Allow;
        }
        if contest_phase(contest, self.now).is_active() {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }

    /// Mirrors `get_contest`'s gate (`find_contest` + `check_contest_access`
    /// only - no `require_contest_started`).
    fn decide_contest(&self, id: i32) -> Decision {
        match self.contests.get(&id) {
            // Rule 7: missing (soft-deleted, since `contests` was fetched with
            // `find_active`) or nonexistent contest -> Deny, mirroring
            // `find_contest`'s `NotFound`.
            None => Decision::Deny,
            Some(c) => self.check_contest_access_decision(c),
        }
    }

    /// Mirrors `list_contest_problems` / `get_contest_problem_samples`, which
    /// both call `check_contest_access` THEN `require_contest_started` on the
    /// same contest before checking `find_contest_problem` /
    /// `is_problem_in_contest` (rule 5). Both gates are applied here, in that
    /// sequence. Used for every problem part (`Problem`, `Sample`,
    /// `Attachment`): a sample or attachment is reachable exactly when its
    /// problem is, under the same scope.
    fn decide_problem_or_sample(
        &self,
        action: Action,
        contest_id: Option<i32>,
        problem_id: i32,
    ) -> Decision {
        let Some(cid) = contest_id else {
            // Standalone problem/sample access - rules 8-9.
            return self.decide_standalone_problem_access(action, problem_id);
        };
        let Some(c) = self.contests.get(&cid) else {
            return Decision::Deny; // rule 7
        };
        if self.check_contest_access_decision(c).is_denied() {
            return Decision::Deny;
        }
        if self.require_contest_started_window_decision(c).is_denied() {
            return Decision::Deny;
        }
        if self.problem_in_contest.contains(&(cid, problem_id)) {
            Decision::Allow
        } else {
            Decision::Deny // rule 5
        }
    }

    /// Rules 8-9. Used for every standalone
    /// (`contest_id: None`) problem part: `Resource::Problem`,
    /// `Resource::Sample` and `Resource::Attachment`. Samples and attachments
    /// are parts of their problem, so this takes only a `problem_id`, never an
    /// `attachment_id`.
    ///
    /// The zero-contests check comes BEFORE the `contest:manage` bypass, so a
    /// `contest:manage` holder without `problem:create`/`problem:edit` is
    /// denied a hidden problem attached to no contest.
    ///
    /// The public-and-started check and the participant-of-a-started-contest
    /// check share one "started" filter, so a single in-memory pass over the
    /// problem's contests serves both: a public started contest admits
    /// first, then membership in any started one does.
    ///
    /// Rule 9 runs only when [`standalone_reachable_via_contest`] says `action`
    /// can use it. For `Action::Submit` the decision ends after rule 8.
    fn decide_standalone_problem_access(&self, action: Action, problem_id: i32) -> Decision {
        let subject = self.subject;
        // Missing or soft-deleted problem -> Deny (`problems` was fetched
        // with `find_active`).
        let Some(problem) = self.problems.get(&problem_id) else {
            return Decision::Deny;
        };
        if subject.has_permission(perm::PROBLEM_CREATE)
            || subject.has_permission(perm::PROBLEM_EDIT)
        {
            return Decision::Allow;
        }
        if problem.is_public {
            return Decision::Allow;
        }
        if !standalone_reachable_via_contest(action) {
            return Decision::Deny;
        }
        // `can_access_problem_via_contest`, inlined below.
        let no_contests: Vec<i32> = Vec::new();
        let candidate_contest_ids = self
            .problem_contest_ids
            .get(&problem_id)
            .unwrap_or(&no_contests);
        if candidate_contest_ids.is_empty() {
            return Decision::Deny;
        }
        if subject.has_permission(perm::CONTEST_MANAGE) {
            return Decision::Allow;
        }
        // A not-yet-started or deactivated contest must not leak a hidden
        // problem's statement, samples or attachments here, even if it is
        // public.
        let started: Vec<&contest::Model> = candidate_contest_ids
            .iter()
            .filter_map(|cid| self.contests.get(cid))
            .filter(|c| contest_phase(c, self.now).has_started())
            .collect();
        if started.iter().any(|c| c.is_public) {
            return Decision::Allow;
        }
        if started.iter().any(|c| self.member_of.contains(&c.id)) {
            return Decision::Allow;
        }
        Decision::Deny
    }

    /// Rule 6, plus rule 7 via the non-owner branch's contest lookup.
    fn decide_submission(&self, id: i32) -> Decision {
        let subject = self.subject;
        let Some(sub) = self.submissions.get(&id) else {
            // No such submission.
            return Decision::Deny;
        };
        if subject.has_permission(perm::SUBMISSION_VIEW_ALL) {
            return Decision::Allow;
        }
        if subject.user_id == Some(sub.user_id) {
            // Owner bypass: an owner always sees their own submission, with no
            // contest lookup, so this stays Allow even if the parent contest
            // has since been soft-deleted.
            return Decision::Allow;
        }
        let Some(cid) = sub.contest_id else {
            return Decision::Deny;
        };
        let Some(c) = self.contests.get(&cid) else {
            return Decision::Deny; // rule 7
        };
        if self.check_contest_access_decision(c).is_denied() {
            return Decision::Deny;
        }
        let is_participant = self.member_of.contains(&cid);
        if !is_participant || !c.submissions_visible {
            Decision::Deny
        } else {
            Decision::Allow
        }
    }

    /// Rule 10: a clarification is reachable by its author, its recipient, a
    /// `contest:manage` holder, or anyone when the row is public. A missing
    /// clarification -> `Deny`, as is a non-public row for anyone else.
    ///
    /// The per-element filter over the `replies` array is not a decision
    /// here; see the module docs.
    fn decide_clarification(&self, id: i32) -> Decision {
        let subject = self.subject;
        let Some(c) = self.clarifications.get(&id) else {
            return Decision::Deny;
        };

        // `subject.user_id.is_some() &&` guards the recipient comparison so an
        // unauthenticated `Subject` can't spuriously match `None == None` on a
        // row with no recipient.
        let is_participant = subject.has_permission(perm::CONTEST_MANAGE)
            || subject.user_id == Some(c.author_id)
            || (subject.user_id.is_some() && c.recipient_id == subject.user_id);

        if is_participant || c.is_public {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

/// Whether rule 9 ("hidden problem reachable via a contest") may admit a
/// standalone (`contest_id: None`) problem-keyed resource for `action`.
///
/// Rule 9 lets a contestant READ a hidden problem's statement, samples and
/// attachments by id while one of its contests runs. It must not admit
/// `Action::Submit`. `create_submission` and `run_code` store their rows with
/// `contest_id = NULL`, so a standalone submit to a contest's hidden problem
/// skips everything that is keyed on the contest:
///   - contest-scoped plugin config such as submission-limit and cooldown,
///     because `resource_enablements(problem_id, None)` ignores it;
///   - IOI/ICPC feedback masking, which only runs when `contest_id` is set;
///   - standings, and the contest's own `contest_type`, since the caller may
///     pick any `contest_type`.
///
/// A contestant submits to a hidden contest problem through the
/// contest-scoped route (`Resource::Problem { contest_id: Some(_) }`, rules
/// 1-5), and the handler then applies the contest's running-window and
/// participant checks. Only rule 8 (problem editors, public problems) admits
/// a standalone submit.
///
/// The match is exhaustive on purpose: a new `Action` has to choose here.
/// `Clarify` and `Download` keep the READ behaviour they always had. Neither
/// is currently decided on a standalone problem.
fn standalone_reachable_via_contest(action: Action) -> bool {
    match action {
        Action::Submit => false,
        Action::Read | Action::Clarify | Action::Download => true,
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
            attempt_id: None,
            status: SubmissionStatus::Pending,
            verdict: None,
            compile_output: None,
            error_code: None,
            error_message: None,
            score: None,
            time_used: None,
            memory_used: None,
            judge_epoch: 0,
            max_judge_epoch: 0,
            target_worker_id: None,
            system_error_retries: 0,
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule2_future_deactivate_time_does_not_close_the_window() {
        // Activated in the past, deactivates in the future: the window is
        // still open. A deactivate_time merely being `Some` must not close
        // it - only `deactivate_time <= now` may.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), Some(1), true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Contest(7)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- rule 10: Resource::Clarification --

    fn clarification_row(
        id: i32,
        contest_id: i32,
        author_id: i32,
        recipient_id: Option<i32>,
        is_public: bool,
    ) -> clarification::Model {
        let now = chrono::Utc::now();
        clarification::Model {
            id,
            contest_id,
            author_id,
            content: "question".into(),
            clarification_type: "question".into(),
            recipient_id,
            is_public,
            resolved: false,
            resolved_at: None,
            resolved_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn clarification_missing_row_denies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<clarification::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn clarification_non_participant_non_public_denies() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![clarification_row(3, 7, 99, None, false)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn clarification_author_sees_own_private_question() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![clarification_row(3, 7, 1, None, false)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn clarification_recipient_sees_own_private_dm() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![clarification_row(3, 7, 99, Some(1), false)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn clarification_contest_manage_sees_any_private_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![clarification_row(3, 7, 99, None, false)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[perm::CONTEST_MANAGE]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn clarification_non_participant_sees_public_row() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![clarification_row(3, 7, 99, None, true)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Read,
            &[Resource::Clarification(3)],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[tokio::test]
    async fn rule9_contest_manage_denied_when_zero_contests_despite_permission() {
        // The zero-contests check comes BEFORE the `contest:manage` bypass.
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
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
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- Action::Submit on a standalone problem: rule 8 only, never rule 9.
    // Each mock below queues ONLY the problem fetch, so
    // these also pin that Submit skips rule 9's contest_problem / contest /
    // contest_user prefetch: an extra query would exhaust the mock and fail
    // the `unwrap()`. --

    async fn decide_standalone_submit(
        db: &sea_orm::DatabaseConnection,
        subject: &Subject,
    ) -> Vec<Decision> {
        host_decide(
            db,
            subject,
            Action::Submit,
            &[Resource::Problem {
                contest_id: None,
                problem_id: 1,
            }],
            &mut HostScopes::default(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn submit_hidden_standalone_problem_denies_even_for_started_contest_participant() {
        // Under Action::Read, a participant of a started contest that holds
        // this problem reaches it through rule 9 (see
        // `rule9_hidden_problem_in_private_started_contest_allows_participant`).
        // Submit never looks at contests, so no contest data is queued here.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .into_connection();
        assert_eq!(
            decide_standalone_submit(&db, &subject(1, &[])).await,
            vec![Decision::Deny]
        );
    }

    #[tokio::test]
    async fn submit_hidden_standalone_problem_denies_contest_manage_without_problem_edit() {
        // Rule 9's `contest:manage` bypass is a READ widening too. Staff who
        // need to submit to a hidden problem use the contest route or hold
        // `problem:create`/`problem:edit` (rule 8).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .into_connection();
        assert_eq!(
            decide_standalone_submit(&db, &subject(1, &[perm::CONTEST_MANAGE])).await,
            vec![Decision::Deny]
        );
    }

    #[tokio::test]
    async fn submit_public_standalone_problem_allows() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, true)]])
            .into_connection();
        assert_eq!(
            decide_standalone_submit(&db, &subject(1, &[])).await,
            vec![Decision::Allow]
        );
    }

    #[tokio::test]
    async fn submit_hidden_standalone_problem_allows_problem_editor() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![problem_row(1, false)]])
            .into_connection();
        assert_eq!(
            decide_standalone_submit(&db, &subject(1, &[perm::PROBLEM_EDIT])).await,
            vec![Decision::Allow]
        );
    }

    #[tokio::test]
    async fn submit_missing_standalone_problem_denies_even_for_editor() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<problem::Model>::new()])
            .into_connection();
        assert_eq!(
            decide_standalone_submit(&db, &subject(1, &[perm::PROBLEM_EDIT])).await,
            vec![Decision::Deny]
        );
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
                contest_id: None,
                problem_id: 1,
                attachment_id: uuid::Uuid::from_u128(999),
            }],
            &mut HostScopes::default(),
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
                contest_id: None,
                problem_id: 1,
                attachment_id: uuid::Uuid::from_u128(2),
            }],
            &mut HostScopes::default(),
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
                contest_id: None,
                problem_id: 1,
                attachment_id: uuid::Uuid::from_u128(2),
            }],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    // -- a contest-scoped attachment follows its problem under that scope
    // (rules 1-5), the same as a contest-scoped problem or sample --

    #[tokio::test]
    async fn contest_scoped_attachment_allows_when_problem_in_contest() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .append_query_results([vec![contest_problem_row(7, 3)]])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Download,
            &[Resource::Attachment {
                contest_id: Some(7),
                problem_id: 3,
                attachment_id: uuid::Uuid::from_u128(2),
            }],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);
    }

    #[tokio::test]
    async fn contest_scoped_attachment_denies_when_problem_not_in_contest() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .append_query_results([Vec::<contest_problem::Model>::new()])
            .into_connection();
        let decisions = host_decide(
            &db,
            &subject(1, &[]),
            Action::Download,
            &[Resource::Attachment {
                contest_id: Some(7),
                problem_id: 3,
                attachment_id: uuid::Uuid::from_u128(2),
            }],
            &mut HostScopes::default(),
        )
        .await
        .unwrap();
        assert_eq!(decisions, vec![Decision::Deny]);
    }

    // -- positional matching across a mixed batch --

    #[tokio::test]
    async fn decisions_are_positional_matching_resources() {
        // The same batch decided in both orders: a decision that followed
        // the resource's kind instead of its position would come back in the
        // same order both times. Exercises rule 4 (private contest denies a
        // non-member), rule 10 (a public clarification is visible) and rule
        // 6 (an owner always sees their own submission).
        async fn decide(resources: &[Resource]) -> Vec<Decision> {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                // Query 0: submissions.
                .append_query_results([vec![submission_row(3, 1, None)]])
                // Query C: clarifications. Its `contest_id` (5) is never
                // added to the contest fetch below.
                .append_query_results([vec![clarification_row(9, 5, 99, None, true)]])
                // Query 1: contest 1 only (private, denies) - submission 3
                // is owned by the subject so its contest is never fetched.
                .append_query_results([vec![contest_row(1, false, Some(-1), None, true)]])
                // Query 2: membership - empty (not a member of contest 1).
                .append_query_results([Vec::<contest_user::Model>::new()])
                .into_connection();
            host_decide(
                &db,
                &subject(1, &[]),
                Action::Read,
                resources,
                &mut HostScopes::default(),
            )
            .await
            .unwrap()
        }
        let resources = vec![
            Resource::Contest(1),
            Resource::Clarification(9),
            Resource::Submission(3),
        ];
        assert_eq!(
            decide(&resources).await,
            vec![Decision::Deny, Decision::Allow, Decision::Allow]
        );
        let reversed: Vec<Resource> = resources.into_iter().rev().collect();
        assert_eq!(
            decide(&reversed).await,
            vec![Decision::Allow, Decision::Allow, Decision::Deny]
        );
    }
}
