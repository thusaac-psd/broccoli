use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Select};

use crate::entity::{contest, contest_problem, contest_user, problem};
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::utils::soft_delete::SoftDeletable;

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
/// once instead of per caller: before, contest-problem config could still be
/// written (re-creating rows `delete_contest` had purged) after the delete.
/// Both gates are `EXISTS` filters in the same statement.
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
) -> Result<(), AppError> {
    if auth_user.has_permission(perm::CONTEST_MANAGE) {
        return Ok(());
    }
    let now = chrono::Utc::now();
    if contest.activate_time.is_none_or(|at| at > now)
        || contest.deactivate_time.is_some_and(|dt| dt <= now)
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }
    if now < contest.start_time {
        return Err(AppError::Validation("Contest has not started yet".into()));
    }
    Ok(())
}

pub fn require_contest_running(
    auth_user: &AuthUser,
    contest: &contest::Model,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), AppError> {
    if auth_user.has_permission(perm::CONTEST_MANAGE) {
        return Ok(());
    }
    if contest.activate_time.is_none_or(|at| at > now)
        || contest.deactivate_time.is_some_and(|dt| dt <= now)
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }
    if now < contest.start_time {
        return Err(AppError::Validation("Contest has not started yet".into()));
    }
    if now >= contest.end_time {
        return Err(AppError::Validation("Contest has already ended".into()));
    }
    Ok(())
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
        return Err(AppError::PermissionDenied);
    }
    Err(AppError::NotFound("Contest not found".into()))
}
