//! The one place that mints credentials.
//!
//! Every endpoint that hands out an access token (`login`, `refresh`,
//! `cli-refresh`, `cli-token`, the device flow) goes through
//! [`issue_access_token`], which reads the user's CURRENT roles and
//! permissions from the database. Nothing copies grants out of a presented
//! JWT: re-signing old claims with a fresh `iat` would launder a token that
//! `FreshAuthUser` rejects (revoked role, reset password, deleted account)
//! into one it accepts.

use common::db::DatabaseConnection;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement, TransactionTrait,
};

use crate::entity::refresh_token;
use crate::error::AppError;
use crate::utils::{hash, jwt, refresh};

/// A freshly signed access token and the identity baked into it.
pub struct AccessGrant {
    pub token: String,
    pub user_id: i32,
    pub username: String,
    pub display_name: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}

/// The active user's profile plus their roles and the union of those roles'
/// permissions, in one round-trip. No row means the user does not exist or is
/// soft-deleted.
///
/// `issued_at` is the DB clock that also stamps `credentials_changed_at`
/// (`now()` of the writer's transaction). A statement that sees a committed
/// credential change starts after that commit, so its `statement_timestamp()`
/// is never earlier than the stamp.
const GRANT_SQL: &str = r#"
SELECT
    statement_timestamp() AS issued_at,
    u.username,
    u.display_name,
    to_jsonb(ARRAY(
        SELECT ur.role FROM user_role ur WHERE ur.user_id = u.id ORDER BY ur.role
    )) AS roles,
    to_jsonb(ARRAY(
        SELECT DISTINCT rp.permission
        FROM user_role ur JOIN role_permission rp ON rp.role = ur.role
        WHERE ur.user_id = u.id
        ORDER BY rp.permission
    )) AS permissions
FROM "user" u
WHERE u.id = $1 AND u.deleted_at IS NULL
"#;

/// Sign an access token for `user_id` from the user's current database state.
///
/// Returns `TokenInvalid` when the user is missing or soft-deleted, so a caller
/// that authenticated with an old credential cannot mint a token for an
/// account that no longer exists.
pub async fn issue_access_token<C: ConnectionTrait>(
    db: &C,
    jwt_secret: &str,
    user_id: i32,
) -> Result<AccessGrant, AppError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            GRANT_SQL,
            [user_id.into()],
        ))
        .await?
        .ok_or(AppError::TokenInvalid)?;

    let issued_at: chrono::DateTime<chrono::Utc> = row.try_get("", "issued_at")?;
    let username: String = row.try_get("", "username")?;
    let display_name: String = row.try_get("", "display_name")?;
    let roles: Vec<String> = json_strings(row.try_get("", "roles")?)?;
    let permissions: Vec<String> = json_strings(row.try_get("", "permissions")?)?;

    let token = jwt::sign_access_token(
        user_id,
        &username,
        roles.clone(),
        permissions.clone(),
        issued_at,
        jwt_secret,
    )
    .map_err(|e| AppError::Internal(format!("JWT sign error: {e}")))?;

    Ok(AccessGrant {
        token,
        user_id,
        username,
        display_name,
        roles,
        permissions,
    })
}

fn json_strings(value: serde_json::Value) -> Result<Vec<String>, AppError> {
    serde_json::from_value(value)
        .map_err(|e| AppError::Internal(format!("malformed grant row: {e}")))
}

/// Stored validators carry this prefix. Rows without it are argon2 hashes
/// written before validators moved to SHA-256; they expire within
/// [`refresh::REFRESH_TOKEN_EXPIRY_DAYS`] of the upgrade.
///
/// A validator is 256 bits from the OS RNG, not a human password, so a
/// SHA-256 digest resists offline guessing as well as argon2 does and costs
/// microseconds instead of tens of milliseconds of CPU on every refresh.
const SHA256_VALIDATOR_PREFIX: &str = "sha256$";

fn hash_validator(validator: &str) -> String {
    format!("{SHA256_VALIDATOR_PREFIX}{}", hash::sha256_hex(validator))
}

/// Whether `presented` matches the stored validator hash, in either format.
async fn validator_matches(presented: &str, stored: &str) -> Result<bool, AppError> {
    match stored.strip_prefix(SHA256_VALIDATOR_PREFIX) {
        Some(digest) => Ok(hash::constant_time_eq(
            hash::sha256_hex(presented).as_bytes(),
            digest.as_bytes(),
        )),
        None => hash::verify_password_off_thread(presented.to_owned(), Some(stored.to_owned()))
            .await
            .map_err(|e| AppError::Internal(format!("Refresh token verify error: {e}"))),
    }
}

/// Seconds after a refresh token is rotated during which presenting it again
/// is a benign race rather than theft: two tabs refreshing with the same
/// cookie, or a client retrying after the response carrying its new cookie
/// was lost in a network blip or gateway failover. Such a replay is answered
/// like a normal rotation (a sibling token), so the client stays logged in,
/// but at most once per rotated row and only while its successor is unused;
/// see [`TokenState::Rotated`]. Past the window, a replay is treated as a
/// stolen refresh chain. The window
/// stays far below the clients' 4.5 minute refresh interval, so a token
/// stolen and replayed later is still detected.
const REFRESH_REUSE_GRACE_SECS: i32 = 30;

/// Insert a new refresh-token row for `$3` and, when `$1` names a live row,
/// retire it in the same statement. Expired rows of the user are purged on the
/// way, which bounds retention; revoked-but-unexpired rows are kept so a
/// replay stays detectable for the token's whole lifetime. Every timestamp is
/// the DB clock.
const ISSUE_REFRESH_SQL: &str = r#"
WITH retired AS (
    UPDATE refresh_tokens SET revoked_at = now()
    WHERE selector = $1 AND revoked_at IS NULL
),
purged AS (
    DELETE FROM refresh_tokens WHERE user_id = $3 AND expires_at < now()
)
INSERT INTO refresh_tokens
    (selector, validator, user_id, expires_at, created_at, revoked_at, rotated_from)
VALUES ($2, $4, $3, now() + make_interval(days => $5::int), now(), NULL, $1)
"#;

async fn issue_refresh_token<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
    retire: Option<&str>,
) -> Result<String, AppError> {
    let selector = hash::generate_random_string();
    let validator = hash::generate_random_string();
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        ISSUE_REFRESH_SQL,
        [
            retire.map(str::to_owned).into(),
            selector.clone().into(),
            user_id.into(),
            hash_validator(&validator).into(),
            (refresh::REFRESH_TOKEN_EXPIRY_DAYS as i32).into(),
        ],
    ))
    .await?;
    Ok(refresh::construct_refresh_token(&selector, &validator))
}

/// Create a refresh-token row for `user_id` and return the opaque token
/// (`selector:validator`) the client stores.
pub async fn mint_refresh_token<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
) -> Result<String, AppError> {
    issue_refresh_token(db, user_id, None).await
}

/// A presented refresh token whose validator matched its row, classified with
/// the DB clock.
struct PresentedToken {
    selector: String,
    user_id: i32,
    rotated_from: Option<String>,
    state: TokenState,
}

enum TokenState {
    Live,
    Expired,
    /// Already rotated out.
    Rotated(Replay),
}

/// How a replay of a rotated token is answered.
enum Replay {
    /// Inside the grace window, with exactly one unused successor (or none
    /// recorded, for rows rotated before successors were linked): issue one
    /// sibling.
    Sibling,
    /// Inside the grace window, but a sibling was already issued: refuse
    /// without side effects. The client's other request holds a valid cookie.
    AlreadyAnswered,
    /// Past the grace window, or the successor was itself rotated: the chain
    /// moved on without this token, so treat it as stolen.
    Reuse,
}

const FIND_PRESENTED_SQL: &str = r#"
SELECT selector, validator, user_id,
       expires_at <= now() AS expired,
       revoked_at IS NOT NULL AS rotated,
       revoked_at IS NOT NULL
           AND now() - revoked_at <= make_interval(secs => $2::int) AS in_grace,
       (SELECT count(*) FROM refresh_tokens c WHERE c.rotated_from = r.selector) AS children,
       (SELECT count(*) FROM refresh_tokens c
        WHERE c.rotated_from = r.selector AND c.revoked_at IS NULL) AS live_children,
       rotated_from
FROM refresh_tokens r
WHERE selector = $1
"#;

/// Find the refresh-token row a presented `selector:validator` names, if the
/// validator matches. A malformed token, unknown selector and wrong validator
/// are indistinguishable to the caller. `lock` takes the row lock for a
/// rotation.
async fn find_presented<C: ConnectionTrait>(
    db: &C,
    presented: &str,
    lock: bool,
) -> Result<Option<PresentedToken>, AppError> {
    let Ok((selector, validator)) = refresh::parse_refresh_token(presented) else {
        return Ok(None);
    };
    let sql = if lock {
        format!("{FIND_PRESENTED_SQL} FOR UPDATE")
    } else {
        FIND_PRESENTED_SQL.to_owned()
    };
    let Some(row) = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [selector.into(), REFRESH_REUSE_GRACE_SECS.into()],
        ))
        .await?
    else {
        return Ok(None);
    };
    let stored: String = row.try_get("", "validator")?;
    if !validator_matches(validator, &stored).await? {
        return Ok(None);
    }
    let state = if row.try_get("", "rotated")? {
        let in_grace: bool = row.try_get("", "in_grace")?;
        let children: i64 = row.try_get("", "children")?;
        let live_children: i64 = row.try_get("", "live_children")?;
        TokenState::Rotated(if !in_grace || live_children < children {
            Replay::Reuse
        } else if children <= 1 {
            Replay::Sibling
        } else {
            Replay::AlreadyAnswered
        })
    } else if row.try_get("", "expired")? {
        TokenState::Expired
    } else {
        TokenState::Live
    };
    Ok(Some(PresentedToken {
        selector: row.try_get("", "selector")?,
        user_id: row.try_get("", "user_id")?,
        rotated_from: row.try_get("", "rotated_from")?,
        state,
    }))
}

/// Delete the refresh-token row a presented token names (logout), together
/// with the row it was rotated from, which would otherwise stay replayable
/// for the rest of the grace window. Returns `TokenInvalid` when it names
/// none.
pub async fn revoke_presented(db: &DatabaseConnection, presented: &str) -> Result<(), AppError> {
    let token = find_presented(db, presented, false)
        .await?
        .ok_or(AppError::TokenInvalid)?;
    refresh_token::Entity::delete_many()
        .filter(
            refresh_token::Column::Selector
                .is_in(std::iter::once(token.selector).chain(token.rotated_from)),
        )
        .exec(db)
        .await?;
    Ok(())
}

/// [`refresh_token::LOCK_OWNER_SQL`], addressed by the presented selector.
const LOCK_OWNER_BY_SELECTOR_SQL: &str = r#"
SELECT 1 FROM "user"
WHERE id = (SELECT user_id FROM refresh_tokens WHERE selector = $1)
FOR NO KEY UPDATE
"#;

/// Exchange a presented refresh token for a new access token and a new
/// refresh token, retiring the presented one. Shared by the cookie
/// (`/auth/refresh`) and body (`/auth/cli-refresh`) endpoints.
///
/// Errors: `TokenInvalid` for a malformed, unknown, expired, or rotated (past
/// the grace window) token; `PermissionDenied` when the user was soft-deleted.
pub async fn rotate_refresh_token(
    db: &DatabaseConnection,
    jwt_secret: &str,
    presented: &str,
) -> Result<(AccessGrant, String), AppError> {
    let txn = db.begin().await?;

    // Take the owner lock before the token row lock, the same order as
    // `revoke_all_for_user` (see `refresh_token::LOCK_OWNER_SQL`).
    if let Ok((selector, _)) = refresh::parse_refresh_token(presented) {
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            LOCK_OWNER_BY_SELECTOR_SQL,
            [selector.into()],
        ))
        .await?;
    }

    let token = find_presented(&txn, presented, true)
        .await?
        .ok_or(AppError::TokenInvalid)?;

    match token.state {
        TokenState::Live => {}
        // A benign race; answered with a sibling of the token that won it.
        // The retire step below is a no-op for an already-rotated row.
        TokenState::Rotated(Replay::Sibling) => {
            tracing::debug!(
                user_id = token.user_id,
                "Refresh token replayed inside the grace window; issuing a sibling"
            );
        }
        TokenState::Rotated(Replay::AlreadyAnswered) => {
            return Err(AppError::TokenInvalid);
        }
        // The validator matched, so this is the token's real holder or a
        // thief: kill the whole family so a stolen chain cannot outlive
        // detection. A mismatched validator never reaches here, so nobody can
        // trip a victim's family kill by guessing a revoked selector.
        TokenState::Rotated(Replay::Reuse) => {
            tracing::warn!(
                user_id = token.user_id,
                "Refresh token reuse detected; revoking all refresh tokens for the user"
            );
            refresh_token::Entity::revoke_all_for_user(&txn, token.user_id).await?;
            txn.commit().await?;
            return Err(AppError::TokenInvalid);
        }
        TokenState::Expired => {
            refresh_token::Entity::delete_by_id(token.selector)
                .exec(&txn)
                .await?;
            txn.commit().await?;
            return Err(AppError::TokenInvalid);
        }
    }

    let grant = match issue_access_token(&txn, jwt_secret, token.user_id).await {
        Ok(grant) => grant,
        Err(AppError::TokenInvalid) => {
            // The user was soft-deleted after this token was issued.
            refresh_token::Entity::delete_by_id(token.selector)
                .exec(&txn)
                .await?;
            txn.commit().await?;
            return Err(AppError::PermissionDenied);
        }
        Err(e) => return Err(e),
    };

    // The rotated row is retained (marked revoked) instead of deleted, so a
    // later replay is detectable as reuse rather than an unknown selector.
    let new_refresh_token = issue_refresh_token(&txn, token.user_id, Some(&token.selector)).await?;
    txn.commit().await?;
    Ok((grant, new_refresh_token))
}
