//! Device-flow login state (RFC 8628) in the `device_authorization` table, so
//! the CLI's request, the browser's approval and the CLI's polls can each
//! land on a different server replica. Every expiry and poll-interval
//! comparison uses the DB clock.

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

use crate::error::AppError;
use crate::services::auth_tokens::issue_access_token;
use crate::utils::hash;

/// Most pending codes held fleet-wide; past it, new requests get 429.
const MAX_PENDING: i64 = 1000;
/// Retries on a user-code collision (20^8 codes, so effectively never).
const MAX_ATTEMPTS: usize = 10;

/// Insert a pending code unless the fleet-wide cap is reached. Expired rows
/// are purged in the same statement, which is what bounds the table.
const CREATE_SQL: &str = r#"
WITH purged AS (
    DELETE FROM device_authorization WHERE expires_at <= now()
)
INSERT INTO device_authorization
    (device_code_hash, user_code, authorized_user_id, created_at, expires_at, last_poll_at)
SELECT $1, $2, NULL, now(), now() + make_interval(secs => $3::int), NULL
WHERE (SELECT count(*) FROM device_authorization WHERE expires_at > now()) < $4
ON CONFLICT DO NOTHING
RETURNING 1 AS created
"#;

const PENDING_COUNT_SQL: &str =
    "SELECT count(*) AS pending FROM device_authorization WHERE expires_at > now()";

/// Store a new pending code. `generate` yields `(device_code, user_code)`
/// candidates; the user code is stored normalized.
pub async fn create(
    db: &DatabaseConnection,
    ttl_secs: u64,
    mut generate: impl FnMut() -> (String, String),
    normalize: impl Fn(&str) -> String,
) -> Result<(String, String), AppError> {
    for _ in 0..MAX_ATTEMPTS {
        let (device_code, user_code) = generate();
        let created = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                CREATE_SQL,
                [
                    hash::sha256_hex(&device_code).into(),
                    normalize(&user_code).into(),
                    (ttl_secs as i32).into(),
                    MAX_PENDING.into(),
                ],
            ))
            .await?;
        if created.is_some() {
            return Ok((device_code, user_code));
        }
        // Nothing inserted: either the cap or a user-code collision.
        let pending: i64 = db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                PENDING_COUNT_SQL,
            ))
            .await?
            .map(|row| row.try_get("", "pending"))
            .transpose()?
            .unwrap_or(0);
        if pending >= MAX_PENDING {
            return Err(AppError::RateLimited { retry_after: 60 });
        }
    }
    Err(AppError::Internal(
        "Failed to generate unique user code".into(),
    ))
}

/// Approve the pending code `user_code` (normalized) for `user_id`.
///
/// Errors: `NotFound` for an unknown or expired code, `Conflict` for one that
/// was already approved.
pub async fn authorize(
    db: &DatabaseConnection,
    user_code: &str,
    user_id: i32,
) -> Result<(), AppError> {
    let approved = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE device_authorization SET authorized_user_id = $2
               WHERE user_code = $1 AND expires_at > now() AND authorized_user_id IS NULL
               RETURNING 1 AS approved"#,
            [user_code.into(), user_id.into()],
        ))
        .await?;
    if approved.is_some() {
        return Ok(());
    }
    let pending = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 AS pending FROM device_authorization WHERE user_code = $1 AND expires_at > now()",
            [user_code.into()],
        ))
        .await?;
    Err(match pending {
        Some(_) => AppError::Conflict("Code has already been authorized".into()),
        None => AppError::NotFound("Code not found or expired".into()),
    })
}

/// What a poll of a device code yields, as RFC 8628 names it.
pub enum PollOutcome {
    Token(String),
    Pending,
    SlowDown,
    Expired,
    /// The approving user was deleted before the code was collected.
    Denied,
}

/// Poll `device_code`. An approved code is consumed and answered with an
/// access token minted from the approving user's current grants.
pub async fn poll(
    db: &DatabaseConnection,
    jwt_secret: &str,
    device_code: &str,
    interval_secs: u64,
) -> Result<PollOutcome, AppError> {
    let code_hash = hash::sha256_hex(device_code);
    let txn = db.begin().await?;
    let Some(row) = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT expires_at <= now() AS expired,
                      last_poll_at IS NOT NULL
                          AND now() - last_poll_at < make_interval(secs => $2::int) AS too_fast,
                      authorized_user_id
               FROM device_authorization WHERE device_code_hash = $1
               FOR UPDATE"#,
            [code_hash.clone().into(), (interval_secs as i32).into()],
        ))
        .await?
    else {
        return Ok(PollOutcome::Expired);
    };

    let expired: bool = row.try_get("", "expired")?;
    let too_fast: bool = row.try_get("", "too_fast")?;
    let authorized: Option<i32> = row.try_get("", "authorized_user_id")?;

    if !expired && too_fast {
        return Ok(PollOutcome::SlowDown);
    }

    let outcome = match (expired, authorized) {
        (true, _) => {
            delete(&txn, &code_hash).await?;
            PollOutcome::Expired
        }
        (false, Some(user_id)) => {
            delete(&txn, &code_hash).await?;
            match issue_access_token(&txn, jwt_secret, user_id).await {
                Ok(grant) => PollOutcome::Token(grant.token),
                Err(AppError::TokenInvalid) => PollOutcome::Denied,
                Err(e) => return Err(e),
            }
        }
        (false, None) => {
            txn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE device_authorization SET last_poll_at = now() WHERE device_code_hash = $1",
                [code_hash.into()],
            ))
            .await?;
            PollOutcome::Pending
        }
    };
    txn.commit().await?;
    Ok(outcome)
}

async fn delete<C: ConnectionTrait>(db: &C, hash: &str) -> Result<(), AppError> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM device_authorization WHERE device_code_hash = $1",
        [hash.into()],
    ))
    .await?;
    Ok(())
}
