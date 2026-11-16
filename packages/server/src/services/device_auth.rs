//! Device-flow login state (RFC 8628) in the `device_authorization` table, so
//! the CLI's request, the browser's approval and the CLI's polls can each
//! land on a different server replica. Every expiry and poll-interval
//! comparison uses the DB clock.

use std::net::IpAddr;

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

use crate::error::AppError;
use crate::services::auth_tokens::{SignIn, issue_access_token, open_session};
use crate::utils::hash;

/// Most pending codes one client address holds; past it, its new requests
/// get 429. Like the failed sign-in quota, it counts only what abuse leaves
/// behind: a CLI sign-in collects its code within one poll of the approval,
/// so a venue that shows the server one address for all its contestants
/// holds about one code per contestant signing in at that moment, while a
/// source minting codes nobody approves fills its allowance and stops. It is
/// counted in the database, so it holds across replicas and while Redis is
/// down, and requests from other addresses are never affected, so no source
/// can lock the fleet's CLI sign-ins out. 1000 rows of a few hundred bytes
/// each is the most one source can make the table hold.
pub const MAX_PENDING_PER_ADDRESS: i64 = 1000;
/// How long a device code stays valid.
pub const CODE_TTL_SECS: u64 = 900;
/// Retries on a user-code collision (20^8 codes, so effectively never).
const MAX_ATTEMPTS: usize = 10;

/// Insert a pending code for client address `$5` unless that address holds
/// `$4` pending codes. Expired rows are purged in the same statement, which
/// is what bounds the table.
const CREATE_SQL: &str = r#"
WITH purged AS (
    DELETE FROM device_authorization WHERE expires_at <= now()
)
INSERT INTO device_authorization
    (device_code_hash, user_code, approved_by, client_addr, created_at, expires_at, last_poll_at)
SELECT $1, $2, NULL, $5, now(), now() + make_interval(secs => $3::int), NULL
WHERE (SELECT count(*) FROM device_authorization
       WHERE client_addr IS NOT DISTINCT FROM $5 AND expires_at > now()) < $4
ON CONFLICT DO NOTHING
RETURNING 1 AS created
"#;

const PENDING_COUNT_SQL: &str = "SELECT count(*) AS pending FROM device_authorization \
     WHERE client_addr IS NOT DISTINCT FROM $1 AND expires_at > now()";

/// Store a new pending code requested from `client`. `generate` yields
/// `(device_code, user_code)` candidates; the user code is stored normalized.
pub async fn create(
    db: &DatabaseConnection,
    client: Option<IpAddr>,
    mut generate: impl FnMut() -> (String, String),
    normalize: impl Fn(&str) -> String,
) -> Result<(String, String), AppError> {
    let client = client.map(|ip| ip.to_string());
    for _ in 0..MAX_ATTEMPTS {
        let (device_code, user_code) = generate();
        let created = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                CREATE_SQL,
                [
                    hash::sha256_hex(&device_code).into(),
                    normalize(&user_code).into(),
                    (CODE_TTL_SECS as i32).into(),
                    MAX_PENDING_PER_ADDRESS.into(),
                    client.clone().into(),
                ],
            ))
            .await?;
        if created.is_some() {
            return Ok((device_code, user_code));
        }
        // Nothing inserted: either the address's cap or a user-code collision.
        let pending: i64 = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                PENDING_COUNT_SQL,
                [client.clone().into()],
            ))
            .await?
            .map(|row| row.try_get("", "pending"))
            .transpose()?
            .unwrap_or(0);
        if pending >= MAX_PENDING_PER_ADDRESS {
            return Err(AppError::RateLimited { retry_after: 60 });
        }
    }
    Err(AppError::Internal(
        "Failed to generate unique user code".into(),
    ))
}

/// Record session `$2`'s approval of the pending code `$1`. The session row is
/// read `FOR KEY SHARE`, the lock the approval's foreign key takes anyway: a
/// logout or credential change that ends the session first leaves nothing to
/// approve with, so nothing is updated; one that ends it after waits and
/// deletes the approval with the session.
const AUTHORIZE_SQL: &str = r#"
UPDATE device_authorization d SET approved_by = s.id
FROM (SELECT id FROM auth_session WHERE id = $2 FOR KEY SHARE) s
WHERE d.user_code = $1 AND d.expires_at > now() AND d.approved_by IS NULL
RETURNING 1 AS approved
"#;

/// Why [`AUTHORIZE_SQL`] approved nothing.
const UNAPPROVED_SQL: &str = r#"
SELECT EXISTS (SELECT 1 FROM device_authorization WHERE user_code = $1 AND expires_at > now())
           AS pending,
       EXISTS (SELECT 1 FROM auth_session WHERE id = $2) AS signed_in
"#;

/// Approve the pending code `user_code` (normalized) in sign-in session
/// `session_id`. The approval lasts as long as the session.
///
/// Errors: `TokenInvalid` when the session ended under the request,
/// `NotFound` for an unknown or expired code, `Conflict` for one that was
/// already approved.
pub async fn authorize(
    db: &DatabaseConnection,
    user_code: &str,
    session_id: i64,
) -> Result<(), AppError> {
    let values = [user_code.into(), session_id.into()];
    let approved = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            AUTHORIZE_SQL,
            values.clone(),
        ))
        .await?;
    if approved.is_some() {
        return Ok(());
    }
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            UNAPPROVED_SQL,
            values,
        ))
        .await?
        .ok_or_else(|| AppError::Internal("approval check returned no row".into()))?;
    let pending: bool = row.try_get("", "pending")?;
    let signed_in: bool = row.try_get("", "signed_in")?;
    Err(match (signed_in, pending) {
        (false, _) => AppError::TokenInvalid,
        (true, true) => AppError::Conflict("Code has already been authorized".into()),
        (true, false) => AppError::NotFound("Code not found or expired".into()),
    })
}

/// What a poll of a device code yields, as RFC 8628 names it.
pub enum PollOutcome {
    Token(String),
    Pending,
    SlowDown,
    /// Unknown, expired or already collected, or the session that approved
    /// it ended.
    Expired,
    /// The approval no longer holds when the code is collected.
    Denied,
}

/// The owner lock (`refresh_token::LOCK_OWNER_SQL`'s row) of the user whose
/// session approved code `$1`, taken `FOR SHARE` before the code's own row:
/// a credential change takes the owner lock and then deletes the approval
/// with the session, so taking them in the other order could deadlock with
/// it. Returns the user it locked; no row for a pending or unknown code.
const LOCK_APPROVER_SQL: &str = r#"
SELECT u.id AS user_id
FROM device_authorization d
JOIN auth_session s ON s.id = d.approved_by
JOIN "user" u ON u.id = s.user_id
WHERE d.device_code_hash = $1
FOR SHARE OF u
"#;

const POLL_SQL: &str = r#"
SELECT d.expires_at <= now() AS expired,
       d.last_poll_at IS NOT NULL
           AND now() - d.last_poll_at < make_interval(secs => $2::int) AS too_fast,
       d.approved_by,
       s.user_id
FROM device_authorization d LEFT JOIN auth_session s ON s.id = d.approved_by
WHERE d.device_code_hash = $1
FOR UPDATE OF d
"#;

/// Poll `device_code`. An approved code is consumed and answered with an
/// access token minted from the approving user's current grants, in a new
/// session opened against the approval (see
/// [`crate::services::auth_tokens::SignIn::DeviceApproval`]).
pub async fn poll(
    db: &DatabaseConnection,
    jwt_secret: &str,
    device_code: &str,
    interval_secs: u64,
) -> Result<PollOutcome, AppError> {
    let code_hash = hash::sha256_hex(device_code);
    let txn = db.begin().await?;
    let locked: Option<i32> = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            LOCK_APPROVER_SQL,
            [code_hash.clone().into()],
        ))
        .await?
        .map(|row| row.try_get("", "user_id"))
        .transpose()?;
    let Some(row) = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            POLL_SQL,
            [code_hash.clone().into(), (interval_secs as i32).into()],
        ))
        .await?
    else {
        return Ok(PollOutcome::Expired);
    };

    let expired: bool = row.try_get("", "expired")?;
    let too_fast: bool = row.try_get("", "too_fast")?;
    let approver: Option<i64> = row.try_get("", "approved_by")?;
    let user_id: Option<i32> = row.try_get("", "user_id")?;

    if !expired && too_fast {
        return Ok(PollOutcome::SlowDown);
    }

    let outcome = match (expired, approver.zip(user_id)) {
        (true, _) => {
            delete(&txn, &code_hash).await?;
            PollOutcome::Expired
        }
        // Approved after the owner lock was taken: collected on the next
        // poll, under the lock.
        (false, Some((_, user_id))) if locked != Some(user_id) => PollOutcome::Pending,
        (false, Some((approver, user_id))) => {
            delete(&txn, &code_hash).await?;
            // The approval is a sign-in of its own, which the CLI's logout
            // ends.
            match open_session(&txn, SignIn::DeviceApproval { user_id, approver }).await? {
                Some(session_id) => PollOutcome::Token(
                    issue_access_token(&txn, jwt_secret, user_id, session_id)
                        .await?
                        .token,
                ),
                None => PollOutcome::Denied,
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
