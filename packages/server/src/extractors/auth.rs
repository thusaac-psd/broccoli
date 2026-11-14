use std::marker::PhantomData;
use std::ops::Deref;

use axum::extract::{FromRef, FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use broccoli_server_sdk::permissions as perm;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::error::AppError;
use crate::state::AppState;
use crate::utils::jwt;

pub struct AuthUser {
    pub user_id: i32,
    /// The sign-in session the caller's token belongs to.
    pub session_id: i64,
    pub username: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
}

impl AuthUser {
    pub fn has_permission(&self, permission: &str) -> bool {
        self.permissions.iter().any(|p| p == permission)
    }

    pub fn require_permission(&self, permission: &str) -> Result<(), AppError> {
        if self.has_permission(permission) {
            Ok(())
        } else {
            Err(AppError::PermissionDenied)
        }
    }

    pub fn require_any_permission(&self, permissions: &[&str]) -> Result<(), AppError> {
        if permissions.iter().any(|perm| self.has_permission(perm)) {
            Ok(())
        } else {
            Err(AppError::PermissionDenied)
        }
    }
}

/// Decode and verify the `Bearer` access token from the request into its
/// claims.
fn bearer_claims(parts: &Parts, state: &AppState) -> Result<jwt::Claims, AppError> {
    let auth_header = parts
        .headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::TokenMissing)?;
    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::TokenInvalid)?;
    jwt::verify(token, &state.config.auth.jwt_secret, state.clock.now())
        .map_err(|_| AppError::TokenInvalid)
}

impl From<jwt::Claims> for AuthUser {
    fn from(claims: jwt::Claims) -> Self {
        AuthUser {
            user_id: claims.uid,
            session_id: claims.sid,
            username: claims.sub,
            roles: claims.roles,
            permissions: claims.permissions,
        }
    }
}

/// The caller of a request, from its bearer access token.
///
/// The freshness rule of the platform lives here: a request whose method
/// changes state (anything but `GET`, `HEAD`, `OPTIONS` and `TRACE`) gets the
/// caller only through `fresh_user`, so every mutating route refuses a token
/// that a credential change, a logout or a deactivation has ended, whichever
/// extractor its handler names. A read trusts the signed claims without a
/// database lookup, so a stale token reads for at most
/// [`jwt::ACCESS_TOKEN_TTL`]; a read that must see the caller as the database
/// knows it now takes [`FreshAuthUser`].
impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let claims = bearer_claims(parts, &app_state)?;
        if parts.method.is_safe() {
            Ok(AuthUser::from(claims))
        } else {
            Ok(fresh_user(&app_state, claims).await?.user)
        }
    }
}

/// The session a request's bearer access token names, if the token's
/// signature and expiry verify, fresh or not: for ending that session. No
/// token, or one that does not verify, is `None`.
pub struct BearerSession(pub Option<i64>);

impl<S> FromRequestParts<S> for BearerSession
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        Ok(Self(
            bearer_claims(parts, &app_state)
                .ok()
                .map(|claims| claims.sid),
        ))
    }
}

/// The caller of a request whatever its method, checked by `fresh_user`:
/// for reads that answer from the caller's current account, such as
/// `/auth/me`. Deref-es to the inner [`AuthUser`].
pub struct FreshAuthUser {
    pub user: AuthUser,
    pub display_name: String,
}

impl Deref for FreshAuthUser {
    type Target = AuthUser;
    fn deref(&self) -> &AuthUser {
        &self.user
    }
}

impl<S> FromRequestParts<S> for FreshAuthUser
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let claims = bearer_claims(parts, &app_state)?;
        fresh_user(&app_state, claims).await
    }
}

/// The active account a token's user has now and the change stamp its
/// freshness is judged against, if the token's session is still open.
const FRESH_USER_SQL: &str = r#"
SELECT u.username, u.display_name, u.credentials_changed_at
FROM "user" u JOIN auth_session s ON s.user_id = u.id
WHERE u.id = $1 AND s.id = $2 AND u.deleted_at IS NULL
"#;

/// The caller of a verified access token, if the token is still fresh: its
/// session is open (no logout ended it), its user is active and it was minted
/// no earlier than the user's last credential change. A stale token is `TokenInvalid`, a 401, so the client
/// refreshes and gets a token carrying the user's current roles. The one
/// freshness check of the platform: [`AuthUser`] on mutating requests,
/// [`FreshAuthUser`] and the plugin proxy all apply it. One indexed read.
pub(crate) async fn fresh_user(
    state: &AppState,
    claims: jwt::Claims,
) -> Result<FreshAuthUser, AppError> {
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            FRESH_USER_SQL,
            [claims.uid.into(), claims.sid.into()],
        ))
        .await
        .map_err(|e| AppError::Internal(format!("credential freshness lookup failed: {e}")))?
        // No row: the session ended, or the user was deleted or deactivated.
        .ok_or(AppError::TokenInvalid)?;
    let username: String = row.try_get("", "username")?;
    let display_name: String = row.try_get("", "display_name")?;
    let credentials_changed_at: chrono::DateTime<chrono::Utc> =
        row.try_get("", "credentials_changed_at")?;

    // Both sides of the comparison are DB-clock values (`iat` is stamped from
    // `statement_timestamp()` in `services::auth_tokens`), so it holds even
    // when server replicas' clocks disagree.
    //
    // Strict `<` (NOT `<=`): `iat` is second-granularity per the JWT spec, so
    // a token minted by a re-login in the SAME second as the credential change
    // has `iat == credentials_changed_at`. `<=` would revoke that fresh token,
    // breaking login/refresh right after a password/role change (a login
    // loop). The residual sub-second window (a token issued <1s BEFORE the
    // change surviving) is negligible: the access token lives only ~5 min and
    // the same change already revokes the refresh tokens.
    if (claims.iat as i64) < credentials_changed_at.timestamp() {
        return Err(AppError::TokenInvalid);
    }
    Ok(FreshAuthUser {
        user: AuthUser {
            username,
            ..AuthUser::from(claims)
        },
        display_name,
    })
}

/// A permission an [`Authorized`] extractor demands. Implemented by zero-sized
/// marker types so the requirement is part of the handler's signature.
pub trait RequiredPermission {
    const NAME: &'static str;
}

/// Marker for [`perm::PROBLEM_EDIT`].
pub struct ProblemEdit;

impl RequiredPermission for ProblemEdit {
    const NAME: &'static str = perm::PROBLEM_EDIT;
}

/// An [`AuthUser`] that already holds permission `P`, checked for freshness
/// whatever the request's method.
///
/// Use it instead of `AuthUser` + `require_permission(..)` on any handler
/// that takes a request body. Axum runs every `FromRequestParts` extractor
/// before the single body extractor (`AppJson`, `BaseMultipart`, ...), so
/// checking the permission HERE rejects an unauthorised caller with 403 before
/// the body is read. A check in the handler body runs only after the body
/// extractor has buffered and parsed the whole request, which behind the
/// 1 GiB upload limit lets any logged-in user make the server allocate ~1 GiB
/// per request.
pub struct Authorized<P: RequiredPermission> {
    user: FreshAuthUser,
    _permission: PhantomData<P>,
}

impl<P: RequiredPermission> Deref for Authorized<P> {
    type Target = AuthUser;
    fn deref(&self) -> &AuthUser {
        &self.user
    }
}

impl<S, P> FromRequestParts<S> for Authorized<P>
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
    P: RequiredPermission,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let user = FreshAuthUser::from_request_parts(parts, state).await?;
        user.require_permission(P::NAME)?;
        Ok(Self {
            user,
            _permission: PhantomData,
        })
    }
}

impl<S> OptionalFromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        match <Self as FromRequestParts<S>>::from_request_parts(parts, state).await {
            Ok(user) => Ok(Some(user)),
            Err(AppError::TokenMissing) | Err(AppError::TokenInvalid) => Ok(None),
            Err(e) => Err(e),
        }
    }
}
