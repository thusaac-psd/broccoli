use broccoli_server_sdk::permissions as perm;
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, Select};

use crate::entity::{contest, contest_problem, contest_user, problem};
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::utils::soft_delete::SoftDeletable;

/// Where a contest is at a given instant. Every time-based contest rule
/// derives from [`contest_phase`], so all of them share one set of
/// boundaries: the activation window is `[activate_time, deactivate_time)`
/// and the contest runs during `[start_time, end_time)`. Contest timelines
/// are validated as `activate_time <= start_time < end_time <=
/// deactivate_time`, so the phases follow each other in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContestPhase {
    /// `activate_time` is unset or in the future. Only `contest:manage` sees
    /// the contest.
    Unactivated,
    /// Activated, before `start_time`.
    Upcoming,
    /// From `start_time` until just before `end_time`.
    Running,
    /// From `end_time` until the contest is deactivated.
    Ended,
    /// `deactivate_time` has passed. Only `contest:manage` sees the contest.
    Deactivated,
}

impl ContestPhase {
    /// Whether the contest is inside its activation window, i.e. visible to
    /// users without `contest:manage`.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Upcoming | Self::Running | Self::Ended)
    }

    /// Whether the contest is active and has started (running or ended).
    pub fn has_started(self) -> bool {
        matches!(self, Self::Running | Self::Ended)
    }
}

/// The phase of `contest` at `now`.
pub fn contest_phase(contest: &contest::Model, now: DateTime<Utc>) -> ContestPhase {
    if contest.activate_time.is_none_or(|at| at > now) {
        ContestPhase::Unactivated
    } else if contest.deactivate_time.is_some_and(|dt| dt <= now) {
        ContestPhase::Deactivated
    } else if !start_has_passed(contest, now) {
        ContestPhase::Upcoming
    } else if now < contest.end_time {
        ContestPhase::Running
    } else {
        ContestPhase::Ended
    }
}

/// Whether `contest`'s `start_time` has passed at `now`, whatever its
/// activation state: the boundary between [`ContestPhase::Upcoming`] and
/// [`ContestPhase::Running`]. From then on standings may exist.
pub fn start_has_passed(contest: &contest::Model, now: DateTime<Utc>) -> bool {
    now >= contest.start_time
}

/// Whether a submission's compile output is shown to viewers who neither own
/// it nor hold `submission:view_all`. Outside contests it always is; inside a
/// contest it is once the contest has ended, or throughout when the contest
/// opts in with `show_compile_output`.
pub fn compile_output_public(contest: Option<&contest::Model>, now: DateTime<Utc>) -> bool {
    contest.is_none_or(|c| {
        c.show_compile_output
            || matches!(
                contest_phase(c, now),
                ContestPhase::Ended | ContestPhase::Deactivated
            )
    })
}

/// SQL form of [`ContestPhase::is_active`] at `now`, for filters pushed into
/// a contest query. Keep the two in step: they must agree on every
/// boundary.
pub fn contest_active_condition(now: DateTime<Utc>) -> Condition {
    Condition::all()
        .add(contest::Column::ActivateTime.is_not_null())
        .add(contest::Column::ActivateTime.lte(now))
        .add(
            Condition::any()
                .add(contest::Column::DeactivateTime.is_null())
                .add(contest::Column::DeactivateTime.gt(now)),
        )
}

/// The contest type that judges submissions to `contest`. Contests created
/// before a type was required may have none; they accept no submissions until
/// an admin sets one, rather than being judged by whichever registered type
/// happens to sort first.
pub fn required_contest_type(contest: &contest::Model) -> Result<String, AppError> {
    contest.contest_type.clone().ok_or_else(|| {
        AppError::Conflict(
            "This contest has no contest type. An admin must set one before it accepts submissions."
                .into(),
        )
    })
}

/// The `contest_problem` link `(contest_id, problem_id)`, only while BOTH
/// parents are active (not soft-deleted). Every contest-problem lookup goes
/// through this, so a deleted contest's problems stop resolving everywhere at
/// once, and contest-problem config cannot be written back for a contest that
/// `delete_contest` purged. Both gates are `EXISTS` filters in the same
/// statement.
fn find_active_contest_problem(
    contest_id: i32,
    problem_id: i32,
) -> Select<contest_problem::Entity> {
    contest_problem::Entity::find_by_id((contest_id, problem_id))
        .filter(contest::Entity::active_exists(contest_id))
        .filter(problem::Entity::active_exists(problem_id))
}

pub async fn is_problem_in_contest<C: sea_orm::ConnectionTrait>(
    db: &C,
    contest_id: i32,
    problem_id: i32,
) -> Result<bool, AppError> {
    let exists = find_active_contest_problem(contest_id, problem_id)
        .one(db)
        .await?
        .is_some();
    Ok(exists)
}

/// Self-registration and self-unregistration are only offered for active
/// public contests. Anything else is a 404 for both, so the two endpoints
/// cannot be told apart for a contest the caller may not see (no existence
/// oracle), and a caller can never leave a contest it could not rejoin.
/// Returns the contest's phase for the caller's own timing rules.
pub fn ensure_self_service_contest(
    contest: &contest::Model,
    now: DateTime<Utc>,
) -> Result<ContestPhase, AppError> {
    let phase = contest_phase(contest, now);
    if !phase.is_active() || !contest.is_public {
        return Err(AppError::NotFound("Contest not found".into()));
    }
    Ok(phase)
}

pub async fn find_contest<C: sea_orm::ConnectionTrait>(
    db: &C,
    id: i32,
) -> Result<contest::Model, AppError> {
    contest::Entity::find_active_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Contest not found".into()))
}

pub async fn find_contest_problem<C: sea_orm::ConnectionTrait>(
    db: &C,
    contest_id: i32,
    problem_id: i32,
) -> Result<contest_problem::Model, AppError> {
    find_active_contest_problem(contest_id, problem_id)
        .one(db)
        .await?
        .ok_or_else(|| AppError::NotFound("Contest problem not found".into()))
}

pub fn require_contest_started(
    auth_user: &AuthUser,
    contest: &contest::Model,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    if auth_user.has_permission(perm::CONTEST_MANAGE) {
        return Ok(());
    }
    match contest_phase(contest, now) {
        ContestPhase::Unactivated | ContestPhase::Deactivated => {
            Err(AppError::NotFound("Contest not found".into()))
        }
        ContestPhase::Upcoming => Err(AppError::Validation("Contest has not started yet".into())),
        ContestPhase::Running | ContestPhase::Ended => Ok(()),
    }
}

pub fn require_contest_running(
    auth_user: &AuthUser,
    contest: &contest::Model,
    now: DateTime<Utc>,
) -> Result<(), AppError> {
    if auth_user.has_permission(perm::CONTEST_MANAGE) {
        return Ok(());
    }
    match contest_phase(contest, now) {
        ContestPhase::Unactivated | ContestPhase::Deactivated => {
            Err(AppError::NotFound("Contest not found".into()))
        }
        ContestPhase::Upcoming => Err(AppError::Validation("Contest has not started yet".into())),
        ContestPhase::Running => Ok(()),
        ContestPhase::Ended => Err(AppError::Validation("Contest has already ended".into())),
    }
}

pub async fn is_contest_participant<C: sea_orm::ConnectionTrait>(
    db: &C,
    contest_id: i32,
    user_id: i32,
) -> Result<bool, AppError> {
    let exists = contest_user::Entity::find()
        .filter(contest_user::Column::ContestId.eq(contest_id))
        .filter(contest_user::Column::UserId.eq(user_id))
        .one(db)
        .await?
        .is_some();
    Ok(exists)
}

pub async fn require_contest_participant<C: sea_orm::ConnectionTrait>(
    db: &C,
    auth_user: &AuthUser,
    contest: &contest::Model,
) -> Result<(), AppError> {
    if auth_user.has_permission(perm::CONTEST_MANAGE) {
        return Ok(());
    }
    let is_participant = is_contest_participant(db, contest.id, auth_user.user_id).await?;
    if is_participant {
        return Ok(());
    }
    if contest.is_public {
        return Err(AppError::ContestNotRegistered);
    }
    Err(AppError::NotFound("Contest not found".into()))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    fn at(minute: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::minutes(minute)
    }

    /// Runs during minutes [10, 20); the activation bounds are minutes too.
    fn timeline(activate: Option<i64>, deactivate: Option<i64>) -> contest::Model {
        contest::Model {
            id: 1,
            title: "Contest".into(),
            description: String::new(),
            activate_time: activate.map(at),
            deactivate_time: deactivate.map(at),
            start_time: at(10),
            end_time: at(20),
            is_public: true,
            submissions_visible: false,
            show_compile_output: false,
            show_participants_list: true,
            contest_type: None,
            created_at: at(0),
            updated_at: at(0),
            deleted_at: None,
        }
    }

    #[test]
    fn phase_boundaries_are_half_open() {
        let c = timeline(Some(0), Some(30));
        let cases = [
            (-1, ContestPhase::Unactivated),
            (0, ContestPhase::Upcoming),
            (9, ContestPhase::Upcoming),
            (10, ContestPhase::Running),
            (19, ContestPhase::Running),
            (20, ContestPhase::Ended),
            (29, ContestPhase::Ended),
            (30, ContestPhase::Deactivated),
        ];
        for (minute, expected) in cases {
            assert_eq!(contest_phase(&c, at(minute)), expected, "minute {minute}");
        }
    }

    #[test]
    fn contest_without_activate_time_is_never_active() {
        let c = timeline(None, None);
        for minute in [0, 15, 25] {
            assert_eq!(contest_phase(&c, at(minute)), ContestPhase::Unactivated);
        }
    }

    #[test]
    fn contest_without_deactivate_time_stays_ended() {
        let c = timeline(Some(0), None);
        assert_eq!(contest_phase(&c, at(10_000)), ContestPhase::Ended);
    }

    #[test]
    fn compile_output_is_public_from_end_time() {
        let c = timeline(Some(0), Some(30));
        assert!(!compile_output_public(Some(&c), at(19)));
        assert!(compile_output_public(Some(&c), at(20)));
        assert!(compile_output_public(Some(&c), at(30)));
        assert!(compile_output_public(None, at(15)));
        let opted_in = contest::Model {
            show_compile_output: true,
            ..c
        };
        assert!(compile_output_public(Some(&opted_in), at(15)));
    }
}
