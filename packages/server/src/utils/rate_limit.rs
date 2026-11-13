use chrono::{DateTime, Duration, Utc};
use sea_orm::*;

use crate::error::AppError;

/// The window `rate_limit_per_minute` counts over.
const WINDOW: Duration = Duration::minutes(1);

/// Refuse with `RATE_LIMITED` when `user_id` created `limit_per_minute` or
/// more submissions and code runs together in the minute before `now`, the
/// platform clock (which also stamped their `created_at`). One statement.
pub async fn check_rate_limit(
    db: &DatabaseConnection,
    now: DateTime<Utc>,
    user_id: i32,
    limit_per_minute: u32,
) -> Result<(), AppError> {
    if limit_per_minute == 0 {
        return Ok(());
    }

    let since = now - WINDOW;
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT count(*) AS n, min(created_at) AS oldest FROM (
                   SELECT created_at FROM submission WHERE user_id = $1 AND created_at > $2
                   UNION ALL
                   SELECT created_at FROM code_run WHERE user_id = $1 AND created_at > $2
               ) recent"#,
            [user_id.into(), since.into()],
        ))
        .await?
        .ok_or_else(|| AppError::Internal("rate limit query returned no row".into()))?;
    let count: i64 = row.try_get("", "n")?;
    let oldest: Option<DateTime<Utc>> = row.try_get("", "oldest")?;

    if count >= i64::from(limit_per_minute) {
        let retry_after = oldest
            .map(|t| std::cmp::max((t + WINDOW - now).num_seconds(), 1) as u64)
            .unwrap_or(60);
        return Err(AppError::RateLimited { retry_after });
    }

    Ok(())
}
