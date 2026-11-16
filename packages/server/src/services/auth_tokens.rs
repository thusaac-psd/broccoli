//! The one place that mints credentials and opens and ends sign-in sessions.
//!
//! Every endpoint that hands out an access token (`login`, `refresh`,
//! `cli-refresh`, `cli-token`, the device flow) goes through
//! [`issue_access_token`], which reads the user's CURRENT roles and
//! permissions from the database. Nothing copies grants out of a presented
//! JWT: re-signing old claims with a fresh `iat` would launder a token that
//! freshness check rejects (revoked role, reset password, deleted account)
//! into one it accepts.

use common::db::DatabaseConnection;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement, TransactionTrait,
};

use crate::entity::{auth_session, refresh_token};
use crate::error::AppError;
use crate::utils::{hash, jwt, refresh};

/// An access token is valid this long; a session that has no unexpired
/// refresh token and is older than this can mint nothing more.
const ACCESS_TOKEN_TTL_SECS: i32 = jwt::ACCESS_TOKEN_TTL.num_seconds() as i32;

/// The credential check a new sign-in session rests on. Opening the session
/// checks it again, in the statement that inserts the row, so a credential
/// change that commits after the caller's check is never outrun.
#[derive(Debug, Clone, Copy)]
pub enum SignIn<'a> {
    /// The user presented the password whose stored hash is `hash`.
    Password { user_id: i32, hash: &'a str },
    /// The user's open session `approver` approved a device code.
    DeviceApproval { user_id: i32, approver: i64 },
}

/// Open a session for the active user `$1` if the credential check still holds:
/// the stored password hash is `$3`, or the approving session `$4` is open.
/// The user's sessions that can mint nothing more (older than an access token
/// and without an unexpired refresh token) are purged on the way. No row
/// comes back when the check no longer holds.
///
/// The user row is read `FOR SHARE`, which conflicts with the owner lock
/// (`refresh_token::LOCK_OWNER_SQL`) and the row update every credential
/// change takes. A change that holds the row first is waited for, and the row
/// is checked again as it committed it; one that comes after waits for this
/// sign-in to commit and then ends the new session with the others. The lock
/// is taken before any session row, the order a credential change takes them.
const OPEN_SESSION_SQL: &str = r#"
WITH owner AS (
    SELECT u.id FROM "user" u
    WHERE u.id = $1 AND u.deleted_at IS NULL
      AND ($3::varchar IS NULL OR u.password = $3)
      AND ($4::bigint IS NULL
           OR EXISTS (SELECT 1 FROM auth_session a WHERE a.id = $4 AND a.user_id = u.id))
    FOR SHARE OF u
),
purged AS (
    DELETE FROM auth_session s USING owner
    WHERE s.user_id = owner.id
      AND s.created_at < now() - make_interval(secs => $2::int)
      AND NOT EXISTS (
          SELECT 1 FROM refresh_tokens r WHERE r.session_id = s.id AND r.expires_at > now()
      )
)
INSERT INTO auth_session (user_id, created_at)
SELECT id, now() FROM owner
RETURNING id
"#;

/// Open a sign-in session on the strength of `sign_in`.
///
/// Returns `None` when the check no longer holds: the password changed, the
/// approving session ended, or the user is missing or soft-deleted.
pub async fn open_session<C: ConnectionTrait>(
    db: &C,
    sign_in: SignIn<'_>,
) -> Result<Option<i64>, AppError> {
    let (user_id, hash, approver) = match sign_in {
        SignIn::Password { user_id, hash } => (user_id, Some(hash.to_owned()), None),
        SignIn::DeviceApproval { user_id, approver } => (user_id, None, Some(approver)),
    };
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            OPEN_SESSION_SQL,
            [
                user_id.into(),
                ACCESS_TOKEN_TTL_SECS.into(),
                hash.into(),
                approver.into(),
            ],
        ))
        .await?;
    row.map(|row| row.try_get("", "id"))
        .transpose()
        .map_err(Into::into)
}

/// End the sessions `ids` names: their refresh tokens go with them and their
/// access tokens fail the freshness check from now on. Ending an
/// already-ended session is a no-op.
pub async fn end_sessions<C: ConnectionTrait>(db: &C, ids: &[i64]) -> Result<(), AppError> {
    if ids.is_empty() {
        return Ok(());
    }
    auth_session::Entity::delete_many()
        .filter(auth_session::Column::Id.is_in(ids.iter().copied()))
        .exec(db)
        .await?;
    Ok(())
}

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

/// Sign an access token for `user_id` in session `session_id` from the user's
/// current database state.
///
/// Returns `TokenInvalid` when the user is missing or soft-deleted, so a caller
/// that authenticated with an old credential cannot mint a token for an
/// account that no longer exists.
pub async fn issue_access_token<C: ConnectionTrait>(
    db: &C,
    jwt_secret: &str,
    user_id: i32,
    session_id: i64,
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
        session_id,
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

/// Stored validators are the SHA-256 hex digest of the validator. A validator
/// is 256 bits from the OS RNG, not a human password, so a SHA-256 digest
/// resists offline guessing as well as argon2 does and costs microseconds
/// instead of tens of milliseconds of CPU on every refresh.
fn hash_validator(validator: &str) -> String {
    hash::sha256_hex(validator)
}

/// Whether `presented` matches the stored validator digest, in constant time.
fn validator_matches(presented: &str, stored: &str) -> bool {
    hash::constant_time_eq(hash_validator(presented).as_bytes(), stored.as_bytes())
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

/// Insert a new refresh-token row for `$3` in sign-in session `$6` and, when
/// `$1` names a live row, retire it in the same statement. Expired rows of the
/// user are purged on the way, which bounds retention; revoked-but-unexpired
/// rows are kept so a replay stays detectable for the token's whole lifetime.
/// Every timestamp is the DB clock.
///
/// The session row is read `FOR KEY SHARE`, the lock the row's foreign key
/// takes anyway: a logout or password reset that ends the session first
/// leaves no row to insert for, so the statement inserts nothing instead of
/// failing the key; one that ends it after waits and takes the new row with
/// it.
const ISSUE_REFRESH_SQL: &str = r#"
WITH session AS (
    SELECT id FROM auth_session WHERE id = $6 AND user_id = $3 FOR KEY SHARE
),
retired AS (
    UPDATE refresh_tokens SET revoked_at = now()
    WHERE selector = $1 AND revoked_at IS NULL
),
purged AS (
    DELETE FROM refresh_tokens WHERE user_id = $3 AND expires_at < now()
)
INSERT INTO refresh_tokens
    (selector, validator, user_id, expires_at, created_at, revoked_at, rotated_from, session_id)
SELECT $2, $4, $3, now() + make_interval(days => $5::int), now(), NULL, $1, session.id
  FROM session
"#;

async fn issue_refresh_token<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
    session_id: i64,
    retire: Option<&str>,
) -> Result<String, AppError> {
    let selector = hash::generate_random_string();
    let validator = hash::generate_random_string();
    let inserted = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            ISSUE_REFRESH_SQL,
            [
                retire.map(str::to_owned).into(),
                selector.clone().into(),
                user_id.into(),
                hash_validator(&validator).into(),
                (refresh::REFRESH_TOKEN_EXPIRY_DAYS as i32).into(),
                session_id.into(),
            ],
        ))
        .await?
        .rows_affected();
    // The session ended under the request (a logout, a password reset): the
    // credential it presented is no longer valid.
    if inserted == 0 {
        return Err(AppError::TokenInvalid);
    }
    Ok(refresh::construct_refresh_token(&selector, &validator))
}

/// Create a refresh-token row for `user_id` in session `session_id` and return
/// the opaque token (`selector:validator`) the client stores.
pub async fn mint_refresh_token<C: ConnectionTrait>(
    db: &C,
    user_id: i32,
    session_id: i64,
) -> Result<String, AppError> {
    issue_refresh_token(db, user_id, session_id, None).await
}

/// A presented refresh token whose validator matched its row, classified with
/// the DB clock.
struct PresentedToken {
    selector: String,
    user_id: i32,
    session_id: i64,
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
    /// Inside the grace window, with exactly one unused successor: issue one
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
       session_id
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
    if !validator_matches(validator, &stored) {
        return Ok(None);
    }
    let state = if row.try_get("", "rotated")? {
        let in_grace: bool = row.try_get("", "in_grace")?;
        let children: i64 = row.try_get("", "children")?;
        let live_children: i64 = row.try_get("", "live_children")?;
        TokenState::Rotated(if !in_grace || live_children < children {
            Replay::Reuse
        } else if children == 1 {
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
        session_id: row.try_get("", "session_id")?,
        state,
    }))
}

/// The session a presented refresh token belongs to, if the token is genuine
/// (its validator matches), whatever its state.
pub async fn session_of_presented(
    db: &DatabaseConnection,
    presented: &str,
) -> Result<Option<i64>, AppError> {
    Ok(find_presented(db, presented, false)
        .await?
        .map(|token| token.session_id))
}

/// [`refresh_token::LOCK_OWNER_SQL`], addressed by the presented selector,
/// and a key-share lock on the token's session. A logout ending the session
/// waits for the rotation, or the rotation finds the session gone, never the
/// two waiting on each other's rows.
const LOCK_OWNER_BY_SELECTOR_SQL: &str = r#"
SELECT 1 FROM "user" u JOIN auth_session s ON s.user_id = u.id
WHERE s.id = (SELECT session_id FROM refresh_tokens WHERE selector = $1)
FOR NO KEY UPDATE OF u FOR KEY SHARE OF s
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

    // Take the owner and session locks before the token row lock, the same
    // order as `revoke_all_for_user` (see `refresh_token::LOCK_OWNER_SQL`).
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
    let session_id = token.session_id;

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
        // thief: end the whole family, the token's session, so a stolen chain
        // cannot outlive detection; its access tokens die with it. A
        // mismatched validator never reaches here, so nobody can trip a
        // victim's family kill by guessing a revoked selector.
        TokenState::Rotated(Replay::Reuse) => {
            tracing::warn!(
                user_id = token.user_id,
                session_id,
                "Refresh token reuse detected; ending its session"
            );
            end_sessions(&txn, &[session_id]).await?;
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

    let grant = match issue_access_token(&txn, jwt_secret, token.user_id, session_id).await {
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
    let new_refresh_token =
        issue_refresh_token(&txn, token.user_id, session_id, Some(&token.selector)).await?;
    txn.commit().await?;
    Ok((grant, new_refresh_token))
}

#[cfg(test)]
mod tests {
    use sea_orm::TransactionTrait;

    use super::*;
    use crate::entity::user;
    use sea_orm::{ColumnTrait, QueryFilter};

    /// The stored hash `test_support::seed_user_and_problem` gives its user.
    const SEEDED_HASH: &str = "x";

    fn password(user_id: i32) -> SignIn<'static> {
        SignIn::Password {
            user_id,
            hash: SEEDED_HASH,
        }
    }

    /// What a password reset does to the user row and sessions
    /// (`handlers::user::update_user`).
    async fn reset_password<C: ConnectionTrait>(db: &C, user_id: i32) {
        db.execute_unprepared(&format!(
            r#"SELECT 1 FROM "user" WHERE id = {user_id} FOR UPDATE;
               UPDATE "user" SET password = 'new' WHERE id = {user_id}"#
        ))
        .await
        .unwrap();
        refresh_token::Entity::revoke_all_for_user(db, user_id)
            .await
            .unwrap();
        user::Entity::touch_credentials_changed(db, user_id)
            .await
            .unwrap();
    }

    async fn session_count(db: &DatabaseConnection, user_id: i32) -> usize {
        auth_session::Entity::find()
            .filter(auth_session::Column::UserId.eq(user_id))
            .all(db)
            .await
            .unwrap()
            .len()
    }

    /// A password checked before a reset committed opens no session after it.
    #[tokio::test]
    async fn a_sign_in_checked_against_a_replaced_password_opens_no_session() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, _) = crate::test_support::seed_user_and_problem(&db).await;
        reset_password(&db, user_id).await;

        assert_eq!(open_session(&db, password(user_id)).await.unwrap(), None);
        assert_eq!(session_count(&db, user_id).await, 0);
    }

    /// A reset that holds the user row when the sign-in arrives: the sign-in
    /// waits for it and finds the password replaced.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_sign_in_racing_a_reset_that_holds_the_user_opens_no_session() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, _) = crate::test_support::seed_user_and_problem(&db).await;

        let reset = db.begin().await.unwrap();
        reset_password(&reset, user_id).await;
        let sign_in = tokio::spawn({
            let db = db.clone();
            async move { open_session(&db, password(user_id)).await }
        });
        crate::test_support::wait_until_blocked_on_a_lock(&db).await;
        reset.commit().await.unwrap();

        assert_eq!(sign_in.await.unwrap().unwrap(), None);
        assert_eq!(session_count(&db, user_id).await, 0);
    }

    /// A sign-in that holds the user row when a reset arrives: the reset
    /// waits for it and ends the new session with the others.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_reset_racing_a_sign_in_that_holds_the_user_ends_its_session() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, _) = crate::test_support::seed_user_and_problem(&db).await;

        let sign_in = db.begin().await.unwrap();
        let session = open_session(&sign_in, password(user_id))
            .await
            .unwrap()
            .expect("the password still holds");
        mint_refresh_token(&sign_in, user_id, session)
            .await
            .unwrap();
        let reset = tokio::spawn({
            let db = db.clone();
            async move {
                let txn = db.begin().await.unwrap();
                reset_password(&txn, user_id).await;
                txn.commit().await.unwrap();
            }
        });
        crate::test_support::wait_until_blocked_on_a_lock(&db).await;
        sign_in.commit().await.unwrap();
        reset.await.unwrap();

        assert_eq!(session_count(&db, user_id).await, 0);
        assert!(
            refresh_token::Entity::find()
                .all(&db)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// A device approval opens a session only while the session that approved
    /// it is open.
    #[tokio::test]
    async fn a_device_approval_opens_a_session_only_while_its_approver_is_open() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, _) = crate::test_support::seed_user_and_problem(&db).await;
        let approver = open_session(&db, password(user_id)).await.unwrap().unwrap();
        let approval = SignIn::DeviceApproval { user_id, approver };

        assert!(open_session(&db, approval).await.unwrap().is_some());
        end_sessions(&db, &[approver]).await.unwrap();
        assert_eq!(open_session(&db, approval).await.unwrap(), None);
    }

    /// A logout that holds the session row when a token mint for it arrives:
    /// the mint waits for it and, once the logout commits, answers
    /// `TokenInvalid` instead of failing the session's foreign key.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mint_racing_the_sessions_logout_is_an_invalid_token() {
        let (_pg, db) = crate::test_support::postgres().await;
        let (user_id, _) = crate::test_support::seed_user_and_problem(&db).await;
        let session = open_session(&db, password(user_id)).await.unwrap().unwrap();

        let logout = db.begin().await.unwrap();
        end_sessions(&logout, &[session]).await.unwrap();
        let mint = tokio::spawn({
            let db = db.clone();
            async move { mint_refresh_token(&db, user_id, session).await }
        });
        logout.commit().await.unwrap();
        assert!(
            matches!(mint.await.unwrap(), Err(AppError::TokenInvalid)),
            "the mint finds the session ended"
        );

        // A mint after the logout committed finds no session either.
        assert!(matches!(
            mint_refresh_token(&db, user_id, session).await,
            Err(AppError::TokenInvalid)
        ));
        let rows = refresh_token::Entity::find().all(&db).await.unwrap();
        assert!(rows.is_empty(), "{rows:?}");
    }
}
