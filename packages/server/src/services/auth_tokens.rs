//! The one place that mints credentials.
//!
//! Every endpoint that hands out an access token (`login`, `refresh`,
//! `cli-refresh`, `cli-token`, the device flow) goes through
//! [`issue_access_token`], which reads the user's CURRENT roles and
//! permissions from the database. Nothing copies grants out of a presented
//! JWT: re-signing old claims with a fresh `iat` would launder a token that
//! `FreshAuthUser` rejects (revoked role, reset password, deleted account)
//! into one it accepts.

use sea_orm::sea_query::LockType;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    ModelTrait, QueryFilter, QuerySelect, Set, Statement, TransactionTrait,
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
const GRANT_SQL: &str = r#"
SELECT
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

    let username: String = row.try_get("", "username")?;
    let display_name: String = row.try_get("", "display_name")?;
    let roles: Vec<String> = json_strings(row.try_get("", "roles")?)?;
    let permissions: Vec<String> = json_strings(row.try_get("", "permissions")?)?;

    let token = jwt::sign_access_token(
        user_id,
        &username,
        roles.clone(),
        permissions.clone(),
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

/// Create a refresh-token row for `user_id` and return the opaque token
/// (`selector:validator`) the client stores.
pub async fn mint_refresh_token<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
) -> Result<String, AppError> {
    let now = chrono::Utc::now();
    let selector = hash::generate_random_string();
    let validator = hash::generate_random_string();

    refresh_token::ActiveModel {
        selector: Set(selector.clone()),
        validator: Set(hash_validator(&validator)),
        user_id: Set(user_id),
        expires_at: Set(now + chrono::Duration::days(refresh::REFRESH_TOKEN_EXPIRY_DAYS)),
        created_at: Set(now),
        revoked_at: Set(None),
    }
    .insert(db)
    .await?;

    Ok(refresh::construct_refresh_token(&selector, &validator))
}

/// Find the refresh-token row a presented `selector:validator` names, if the
/// validator matches. A malformed token, unknown selector and wrong validator
/// are indistinguishable to the caller.
async fn find_presented<C: ConnectionTrait>(
    db: &C,
    presented: &str,
    lock: bool,
) -> Result<Option<refresh_token::Model>, AppError> {
    let Ok((selector, validator)) = refresh::parse_refresh_token(presented) else {
        return Ok(None);
    };
    let mut query = refresh_token::Entity::find_by_id(selector.to_owned());
    if lock {
        query = query.lock(LockType::Update);
    }
    match query.one(db).await? {
        Some(row) if validator_matches(validator, &row.validator).await? => Ok(Some(row)),
        _ => Ok(None),
    }
}

/// Delete the refresh-token row a presented token names (logout). Returns
/// `TokenInvalid` when it names none.
pub async fn revoke_presented(db: &DatabaseConnection, presented: &str) -> Result<(), AppError> {
    let row = find_presented(db, presented, false)
        .await?
        .ok_or(AppError::TokenInvalid)?;
    row.delete(db).await?;
    Ok(())
}

/// Seconds after a refresh token is rotated during which a replay of it is
/// treated as a benign concurrent-refresh race (two in-flight requests carried
/// the same pre-rotation cookie) rather than theft. Past this window, replaying
/// an already-rotated token is treated as a stolen refresh chain.
const REFRESH_REUSE_GRACE_SECS: i64 = 10;

/// Exchange a presented refresh token for a new access token and a new
/// refresh token, retiring the presented one. Shared by the cookie
/// (`/auth/refresh`) and body (`/auth/cli-refresh`) endpoints.
///
/// Errors: `TokenInvalid` for a malformed, unknown, expired or rotated token;
/// `PermissionDenied` when the user was soft-deleted.
pub async fn rotate_refresh_token(
    db: &DatabaseConnection,
    jwt_secret: &str,
    presented: &str,
) -> Result<(AccessGrant, String), AppError> {
    let txn = db.begin().await?;

    let row = find_presented(&txn, presented, true)
        .await?
        .ok_or(AppError::TokenInvalid)?;

    // Reuse detection: the validator matched, but the row was already rotated
    // out. A mismatched validator never reaches here, so nobody can trip a
    // victim's family kill by guessing a revoked selector.
    if let Some(revoked_at) = row.revoked_at {
        let elapsed = chrono::Utc::now() - revoked_at;
        if elapsed > chrono::Duration::seconds(REFRESH_REUSE_GRACE_SECS) {
            tracing::warn!(
                user_id = row.user_id,
                "Refresh token reuse detected past the grace window; revoking all refresh tokens for the user"
            );
            refresh_token::Entity::revoke_all_for_user(&txn, row.user_id).await?;
            txn.commit().await?;
        }
        return Err(AppError::TokenInvalid);
    }

    if row.expires_at < chrono::Utc::now() {
        row.delete(&txn).await?;
        txn.commit().await?;
        return Err(AppError::TokenInvalid);
    }

    let user_id = row.user_id;
    let grant = match issue_access_token(&txn, jwt_secret, user_id).await {
        Ok(grant) => grant,
        Err(AppError::TokenInvalid) => {
            // The user was soft-deleted after this token was issued.
            row.delete(&txn).await?;
            txn.commit().await?;
            return Err(AppError::PermissionDenied);
        }
        Err(e) => return Err(e),
    };

    // Retain the rotated row (mark revoked) instead of deleting it, so a later
    // replay is detectable as reuse rather than an unknown selector.
    let now = chrono::Utc::now();
    let mut rotated: refresh_token::ActiveModel = row.into();
    rotated.revoked_at = Set(Some(now));
    rotated.update(&txn).await?;
    let new_refresh_token = mint_refresh_token(&txn, user_id).await?;

    // Bound retention: drop this user's expired rows (rotated or not).
    // Revoked-but-unexpired rows are kept so a replay stays detectable for the
    // token's whole lifetime.
    refresh_token::Entity::delete_many()
        .filter(refresh_token::Column::UserId.eq(user_id))
        .filter(refresh_token::Column::ExpiresAt.lt(now))
        .exec(&txn)
        .await?;

    txn.commit().await?;
    Ok((grant, new_refresh_token))
}
