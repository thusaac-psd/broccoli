use common::db::DatabaseConnection;
use std::cmp;

use chrono::{Duration, Utc};
use sea_orm::*;

use crate::entity::{code_run, submission};
use crate::error::AppError;

pub async fn check_rate_limit(
    db: &DatabaseConnection,
    user_id: i32,
    limit_per_minute: u32,
) -> Result<(), AppError> {
    if limit_per_minute == 0 {
        return Ok(());
    }

    let one_minute_ago = Utc::now() - Duration::minutes(1);

    let sub_count = submission::Entity::find()
        .filter(submission::Column::UserId.eq(user_id))
        .filter(submission::Column::CreatedAt.gt(one_minute_ago))
        .count(db)
        .await?;

    let run_count = code_run::Entity::find()
        .filter(code_run::Column::UserId.eq(user_id))
        .filter(code_run::Column::CreatedAt.gt(one_minute_ago))
        .count(db)
        .await?;

    let count = sub_count + run_count;

    if count >= limit_per_minute as u64 {
        let oldest_submission = submission::Entity::find()
            .filter(submission::Column::UserId.eq(user_id))
            .filter(submission::Column::CreatedAt.gt(one_minute_ago))
            .order_by_asc(submission::Column::CreatedAt)
            .one(db)
            .await?;

        let oldest_code_run = code_run::Entity::find()
            .filter(code_run::Column::UserId.eq(user_id))
            .filter(code_run::Column::CreatedAt.gt(one_minute_ago))
            .order_by_asc(code_run::Column::CreatedAt)
            .one(db)
            .await?;

        let oldest_created_at = match (
            oldest_submission.map(|s| s.created_at),
            oldest_code_run.map(|r| r.created_at),
        ) {
            (Some(a), Some(b)) => Some(cmp::min(a, b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        let retry_after = oldest_created_at
            .map(|t| {
                let expires = t + Duration::minutes(1);
                cmp::max((expires - Utc::now()).num_seconds(), 1) as u64
            })
            .unwrap_or(60);

        return Err(AppError::RateLimited { retry_after });
    }

    Ok(())
}
