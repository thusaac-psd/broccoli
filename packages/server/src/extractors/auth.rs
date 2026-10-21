use std::ops::Deref;

use axum::extract::{FromRef, FromRequestParts, OptionalFromRequestParts};
use axum::http::request::Parts;
use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};

use crate::entity::user;
use crate::error::AppError;
use crate::state::AppState;
use crate::utils::jwt;
use crate::utils::soft_delete::SoftDeletable;

pub struct AuthUser {
    pub user_id: i32,
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
/// claims. Shared by [`AuthUser`] (stateless) and [`FreshAuthUser`].
fn bearer_claims(parts: &Parts, secret: &str) -> Result<jwt::Claims, AppError> {
    let auth_header = parts
        .headers
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(AppError::TokenMissing)?;
    let token = auth_header
        .strip_prefix("Bearer ")
        .ok_or(AppError::TokenInvalid)?;
    jwt::verify(token, secret).map_err(|_| AppError::TokenInvalid)
}

impl From<jwt::Claims> for AuthUser {
    fn from(claims: jwt::Claims) -> Self {
        AuthUser {
            user_id: claims.uid,
            username: claims.sub,
            roles: claims.roles,
            permissions: claims.permissions,
        }
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
    AppState: axum::extract::FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let claims = bearer_claims(parts, &app_state.config.auth.jwt_secret)?;
        Ok(AuthUser::from(claims))
    }
}

/// Like [`AuthUser`], but additionally rejects an access token that was minted
/// before the user's `credentials_changed_at` (password reset, role
/// grant/revoke, deactivation) or belongs to a now-inactive user. Costs one
/// indexed lookup, so it guards only high-value mutation handlers -- the
/// stateless [`AuthUser`] stays free on the hot read path. Deref-es to the
/// inner `AuthUser` so `require_permission(..)` etc. work unchanged.
pub struct FreshAuthUser(pub AuthUser);

impl Deref for FreshAuthUser {
    type Target = AuthUser;
    fn deref(&self) -> &AuthUser {
        &self.0
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
        let claims = bearer_claims(parts, &app_state.config.auth.jwt_secret)?;

        // One indexed read of the active user's credential-change stamp. A
        // missing row means the user was soft-deleted/deactivated -> reject.
        let credentials_changed_at: chrono::DateTime<chrono::Utc> = user::Entity::find_active()
            .filter(user::Column::Id.eq(claims.uid))
            .select_only()
            .column(user::Column::CredentialsChangedAt)
            .into_tuple()
            .one(&app_state.db)
            .await
            .map_err(|e| AppError::Internal(format!("credential freshness lookup failed: {e}")))?
            .ok_or(AppError::TokenInvalid)?;

        // Tokens predating the last credential change (including legacy tokens
        // with no `iat`, which decode as 0) are stale -> force a re-auth.
        //
        // Strict `<` (NOT `<=`) is deliberate: `iat` is second-granularity per the
        // JWT spec, so a token minted by a re-login in the SAME second as the
        // credential change has `iat == credentials_changed_at`. `<=` would revoke
        // that fresh token, breaking login/refresh right after a password/role
        // change (a login loop). The residual sub-second window (a token issued
        // <1s BEFORE the change surviving) is negligible: the access token lives
        // only ~5 min and the same change already revokes the refresh tokens.
        if (claims.iat as i64) < credentials_changed_at.timestamp() {
            return Err(AppError::TokenInvalid);
        }

        Ok(FreshAuthUser(AuthUser::from(claims)))
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
