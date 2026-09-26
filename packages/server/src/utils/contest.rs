use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::entity::{contest, contest_problem, contest_user};
use crate::error::AppError;
use crate::extractors::auth::AuthUser;
use crate::utils::soft_delete::SoftDeletable;

pub async fn is_problem_in_contest<C: sea_orm::ConnectionTrait>(
    db: &C,
    contest_id: i32,
    problem_id: i32,
) -> Result<bool, AppError> {
    let exists = contest_problem::Entity::find_by_id((contest_id, problem_id))
        .one(db)
        .await?
        .is_some();
    Ok(exists)
}

pub async fn check_contest_access<C: sea_orm::ConnectionTrait>(
    db: &C,
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
    if contest.is_public {
        return Ok(());
    }
    let is_participant = contest_user::Entity::find_by_id((contest.id, auth_user.user_id))
        .one(db)
        .await?
        .is_some();
    if is_participant {
        return Ok(());
    }
    Err(AppError::NotFound("Contest not found".into()))
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
    contest_problem::Entity::find_by_id((contest_id, problem_id))
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

#[cfg(test)]
mod contest_access_tests {
    use sea_orm::{DatabaseBackend, MockDatabase};

    use super::*;

    fn user(permissions: &[&str]) -> AuthUser {
        AuthUser {
            user_id: 42,
            username: "contestant".into(),
            roles: vec![],
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        }
    }

    /// `hours` is an offset from now: negative = past, positive = future.
    fn contest_row(
        is_public: bool,
        activate_hours: Option<i64>,
        deactivate_hours: Option<i64>,
    ) -> contest::Model {
        let now = chrono::Utc::now();
        contest::Model {
            id: 7,
            title: "Contest".into(),
            description: "desc".into(),
            activate_time: activate_hours.map(|h| now + chrono::Duration::hours(h)),
            deactivate_time: deactivate_hours.map(|h| now + chrono::Duration::hours(h)),
            start_time: now - chrono::Duration::hours(2),
            end_time: now + chrono::Duration::hours(2),
            is_public,
            submissions_visible: true,
            show_compile_output: true,
            show_participants_list: true,
            contest_type: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    /// A public contest whose `deactivate_time` has passed (archived/deactivated)
    /// is out of its activation window and must 404 for an unprivileged user - even
    /// though `is_public` is true. This is the invariant `list_contest_submissions`
    /// relies on: the pre-fix hand-rolled `is_public` gate skipped the window and
    /// leaked the submission list here. The stubless mock proves the gate rejects
    /// BEFORE any participant lookup query runs.
    #[tokio::test]
    async fn public_but_deactivated_contest_is_not_found_for_unprivileged_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let contest = contest_row(true, Some(-3), Some(-1));
        let err = check_contest_access(&db, &user(&[]), &contest)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AppError::NotFound(_)),
            "public out-of-window contest must be NotFound, got {err:?}"
        );
    }

    /// A public contest not yet activated (`activate_time` in the future) is
    /// likewise out of window and must 404 - an existence oracle otherwise.
    #[tokio::test]
    async fn public_not_yet_activated_contest_is_not_found_for_unprivileged_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let contest = contest_row(true, Some(1), None);
        let err = check_contest_access(&db, &user(&[]), &contest)
            .await
            .unwrap_err();
        assert!(matches!(err, AppError::NotFound(_)));
    }

    /// The positive control: a public contest inside its window is accessible to
    /// any authenticated user with no participant lookup (stubless mock).
    #[tokio::test]
    async fn public_in_window_contest_is_accessible_to_any_user() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let contest = contest_row(true, Some(-1), Some(1));
        check_contest_access(&db, &user(&[]), &contest)
            .await
            .expect("in-window public contest must be accessible");
    }

    /// `contest:manage` short-circuits before the window check, so managers keep
    /// access to a not-yet-activated private contest.
    #[tokio::test]
    async fn contest_manage_bypasses_the_activation_window() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let contest = contest_row(false, Some(1), None);
        check_contest_access(&db, &user(&[perm::CONTEST_MANAGE]), &contest)
            .await
            .expect("contest:manage bypasses the activation window");
    }
}
