use std::collections::HashMap;

use axum::http::header::AUTHORIZATION;
use axum::{
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Method, Response},
    response::IntoResponse,
};
use plugin_core::http::{PluginHttpAuth, PluginHttpRequest, PluginHttpResponse};
use plugin_core::traits::PluginInvokerExt;
use tracing::{info, instrument, warn};

use sea_orm::{ColumnTrait, QueryFilter, QuerySelect};

// visibility-bypass-audited: `user` is used only by `is_token_fresh` below,
// which selects a single `credentials_changed_at` column
// (`select_only().into_tuple()`) and returns a bool - it structurally cannot
// return a `user::Model` to a response body. It mirrors the same
// iat-vs-credentials_changed_at freshness check `FreshAuthUser` performs on
// core routes (pinned by
// tests/integration/auth.rs::stale_access_token_rejected_on_mutation_but_accepted_on_reads),
// duplicated here because the plugin proxy path authenticates via
// `resolve_optional_auth_user` rather than an extractor.
use crate::entity::user;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::AuthUser;
use crate::extractors::path::AppPath;
use crate::state::AppState;
use crate::utils::jwt;
use crate::utils::soft_delete::SoftDeletable;

/// Resolve the caller from the bearer token, returning the `AuthUser` plus the
/// token's `iat` (issued-at). The `iat` is carried so a permission-gated plugin
/// route can enforce credential freshness (see [`is_token_fresh`]).
///
/// No `Authorization` header, or a non-Bearer scheme (e.g. the print plugin's
/// station secret), yields `None`: some plugin routes are public.
///
/// A Bearer token that is PRESENT but fails verification (expired, bad
/// signature, malformed) is `Err(TokenInvalid)` - a 401, exactly as core
/// routes answer. It used to yield `None`, silently turning an expired session
/// into an anonymous caller. On routes that check permissions inside the
/// plugin that surfaced as a 403 "requires contest:manage", which clients do
/// not treat as "refresh your token", so staff hit a misleading permissions
/// error on every action once their 5-minute access token lapsed.
fn resolve_optional_auth_user(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<(AuthUser, i64)>, AppError> {
    let auth_header = match headers.get(AUTHORIZATION) {
        Some(value) => value,
        None => return Ok(None),
    };

    let auth_header = match auth_header.to_str() {
        Ok(h) => h,
        Err(_) => return Ok(None),
    };
    let token = match auth_header.strip_prefix("Bearer ") {
        Some(t) => t,
        None => return Ok(None),
    };
    let claims =
        jwt::verify(token, &state.config.auth.jwt_secret).map_err(|_| AppError::TokenInvalid)?;

    Ok(Some((
        AuthUser {
            user_id: claims.uid,
            username: claims.sub,
            roles: claims.roles,
            permissions: claims.permissions,
        },
        claims.iat as i64,
    )))
}

/// Whether the token is still fresh: the user is active AND the token was issued
/// at or after the last credential change (`iat >= credentials_changed_at`),
/// mirroring the `FreshAuthUser` extractor. A role/permission revoke or a user
/// deactivation bumps `credentials_changed_at` (or drops the active-user row), so
/// a stale token returns `false`. Returns `Err` only on a DB failure.
async fn is_token_fresh(state: &AppState, user_id: i32, iat: i64) -> Result<bool, AppError> {
    let credentials_changed_at: Option<chrono::DateTime<chrono::Utc>> = user::Entity::find_active()
        .filter(user::Column::Id.eq(user_id))
        .select_only()
        .column(user::Column::CredentialsChangedAt)
        .into_tuple()
        .one(&state.db)
        .await
        .map_err(|e| AppError::Internal(format!("credential freshness lookup failed: {e}")))?;
    match credentials_changed_at {
        None => Ok(false), // user soft-deleted / deactivated
        Some(changed_at) => Ok(iat >= changed_at.timestamp()),
    }
}

/// Whether a caller request header may be forwarded to a plugin handler.
///
/// Never forward the caller's *platform* credentials: a plugin receives identity
/// via the `auth` field, not raw headers. The access JWT rides `Authorization:
/// Bearer …` and the long-lived HttpOnly refresh token rides `Cookie`, so both
/// are dropped. A non-Bearer `Authorization` scheme (e.g. the print plugin's
/// `PrintStation <token>` station secret) is NOT a platform credential — it is a
/// plugin-scoped secret the client deliberately addresses to the plugin — so it
/// passes through. An undecodable `Authorization` value is treated as a
/// credential and dropped.
fn is_forwardable_header(name: &str, value: Option<&str>) -> bool {
    if name.eq_ignore_ascii_case("cookie") {
        return false;
    }
    if name.eq_ignore_ascii_case("authorization") {
        return match value {
            Some(v) => !v.trim_start().to_ascii_lowercase().starts_with("bearer "),
            None => false,
        };
    }
    true
}

async fn handle_plugin_request_impl(
    state: AppState,
    plugin_id: String,
    sub_path: String,
    method: Method,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: String,
) -> Result<Response<Body>, AppError> {
    let normalized_path = if sub_path.starts_with('/') {
        sub_path
    } else {
        format!("/{}", sub_path)
    };
    let normalized_path = normalized_path.trim_end_matches('/').to_string();

    info!(
        "Received request for plugin '{}', path '{}'",
        plugin_id, normalized_path
    );

    let mut auth_user = resolve_optional_auth_user(&state, &headers)?;

    // Drop a stale/revoked identity to anonymous BEFORE any authorization decision.
    // This must run for EVERY authenticated request, not only routes with a
    // manifest `permission`, because a plugin can gate its own actions on the
    // forwarded `auth.permissions` via `PluginHttpRequest::has_permission` (e.g.
    // icpc `/reveal`). Zeroing the identity here means a revoked role / deactivated
    // account fails both the manifest-permission check below AND any plugin-side
    // permission check, matching FreshAuthUser on core routes. Anonymous requests
    // (no token) are unaffected.
    if let Some((user, iat)) = &auth_user
        && !is_token_fresh(&state, user.user_id, *iat).await?
    {
        auth_user = None;
    }

    let (handler_name, required_permission, params) = {
        let registry = state
            .plugins
            .get_registry()
            .read()
            .map_err(|_| AppError::Internal("Failed to acquire plugin registry lock".into()))?;

        let entry = registry.get(&plugin_id).ok_or_else(|| {
            warn!("Target plugin not found in registry");
            AppError::NotFound("Plugin not found".into())
        })?;

        let matched_route = entry.router.at(&normalized_path).map_err(|_| {
            warn!("No matching route found in plugin router");
            AppError::NotFound("Route not found".into())
        })?;

        let route_info = matched_route
            .value
            .methods
            .get(&method.to_string())
            .ok_or_else(|| {
                warn!("HTTP method {} not allowed for this route", method);
                AppError::MethodNotAllowed
            })?;

        (
            route_info.handler.clone(),
            route_info.permission.clone(),
            matched_route
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    };

    if let Some(ref permission) = required_permission {
        // `auth_user` was already downgraded to None above if the token was stale
        // or revoked, so a None here on a permission-gated route means no valid
        // identity (missing/invalid/stale token).
        let (user, _iat) = auth_user.as_ref().ok_or_else(|| {
            warn!("Unauthorized access attempt to protected plugin route");
            if headers.contains_key("Authorization") {
                AppError::TokenInvalid
            } else {
                AppError::TokenMissing
            }
        })?;
        user.require_permission(permission)?;
    }

    let request = PluginHttpRequest {
        method: method.to_string(),
        path: normalized_path,
        params,
        query,
        headers: headers
            .iter()
            .filter(|(k, v)| is_forwardable_header(k.as_str(), v.to_str().ok()))
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect(),
        body: serde_json::from_str(&body).ok(),
        auth: auth_user.map(|(user, _iat)| PluginHttpAuth {
            user_id: user.user_id,
            username: user.username,
            roles: user.roles,
            permissions: user.permissions,
        }),
    };

    info!("Forwarding request to plugin handler: {}", handler_name);

    let response: PluginHttpResponse = state
        .plugins
        .call(&plugin_id, &handler_name, request)
        .await?;

    let mut builder = Response::builder().status(response.status);

    if let Some(h) = response.headers {
        for (k, v) in h {
            builder = builder.header(k, v);
        }
    }

    let resp_body = serde_json::to_string(&response.body.unwrap_or(serde_json::Value::Null))
        .map_err(|e| {
            AppError::Internal(format!("Failed to serialize plugin response body: {}", e))
        })?;

    builder
        .body(Body::from(resp_body))
        .map_err(|e| AppError::Internal(format!("Failed to build plugin response: {}", e)))
}

macro_rules! proxy_handler {
    ($fn_name:ident, $method:ident, $op_id:expr, $summary:expr, request_body = $body_type:ty) => {
        #[utoipa::path(
            $method,
            path = "/{plugin_id}/{*path}",
            tag = "Plugins",
            operation_id = $op_id,
            summary = $summary,
            description = "Handles HTTP requests for plugin-defined routes. The plugin and route are determined by the path parameters. The request is forwarded to the plugin's Wasm handler, and the response is returned to the client. Authorization is checked based on the permissions defined in the plugin manifest.",
            params(
                ("plugin_id" = String, Path, description = "The unique identifier of the plugin"),
                ("path" = String, Path, description = "The sub-path defined in plugin's manifest")
            ),
            request_body = $body_type,
            responses(
                (status = 200, description = "Success", body = serde_json::Value),
                (status = 401, description = "Unauthorized", body = ErrorBody),
                (status = 403, description = "Forbidden", body = ErrorBody),
                (status = 404, description = "Plugin or Route not found", body = ErrorBody),
                (status = 405, description = "Method Not Allowed", body = ErrorBody),
            ),
            security(("jwt" = []))
        )]
        #[instrument(skip(state, headers, body), fields(plugin_id = %plugin_id, sub_path = %sub_path))]
        pub async fn $fn_name(
            State(state): State<AppState>,
            AppPath((plugin_id, sub_path)): AppPath<(String, String)>,
            method: Method,
            headers: HeaderMap,
            Query(query): Query<HashMap<String, String>>,
            body: String,
        ) -> Result<impl IntoResponse, AppError> {
            handle_plugin_request_impl(state, plugin_id, sub_path, method, headers, query, body).await
        }
    };

    ($fn_name:ident, $method:ident, $op_id:expr, $summary:expr) => {
        #[utoipa::path(
            $method,
            path = "/{plugin_id}/{*path}",
            tag = "Plugins",
            operation_id = $op_id,
            summary = $summary,
            description = "Handles HTTP requests for plugin-defined routes. The plugin and route are determined by the path parameters. The request is forwarded to the plugin's Wasm handler, and the response is returned to the client. Authorization is checked based on the permissions defined in the plugin manifest.",
            params(
                ("plugin_id" = String, Path, description = "The unique identifier of the plugin"),
                ("path" = String, Path, description = "The sub-path defined in plugin's manifest")
            ),
            responses(
                (status = 200, description = "Success", body = serde_json::Value),
                (status = 401, description = "Unauthorized", body = ErrorBody),
                (status = 403, description = "Forbidden", body = ErrorBody),
                (status = 404, description = "Plugin or Route not found", body = ErrorBody),
                (status = 405, description = "Method Not Allowed", body = ErrorBody),
            ),
            security(("jwt" = []))
        )]
        #[instrument(skip(state, headers, body), fields(plugin_id = %plugin_id, sub_path = %sub_path))]
        pub async fn $fn_name(
            State(state): State<AppState>,
            AppPath((plugin_id, sub_path)): AppPath<(String, String)>,
            method: Method,
            headers: HeaderMap,
            Query(query): Query<HashMap<String, String>>,
            body: String,
        ) -> Result<impl IntoResponse, AppError> {
            handle_plugin_request_impl(state, plugin_id, sub_path, method, headers, query, body).await
        }
    };
}

proxy_handler!(
    post_plugin_request,
    post,
    "postPluginRequest",
    "POST proxy to plugin route",
    request_body = serde_json::Value
);
proxy_handler!(
    put_plugin_request,
    put,
    "putPluginRequest",
    "PUT proxy to plugin route",
    request_body = serde_json::Value
);
proxy_handler!(
    delete_plugin_request,
    delete,
    "deletePluginRequest",
    "DELETE proxy to plugin route",
    request_body = serde_json::Value
);
proxy_handler!(
    patch_plugin_request,
    patch,
    "patchPluginRequest",
    "PATCH proxy to plugin route",
    request_body = serde_json::Value
);

proxy_handler!(
    get_plugin_request,
    get,
    "getPluginRequest",
    "GET proxy to plugin route"
);
proxy_handler!(
    head_plugin_request,
    head,
    "headPluginRequest",
    "HEAD proxy to plugin route"
);
proxy_handler!(
    options_plugin_request,
    options,
    "optionsPluginRequest",
    "OPTIONS proxy to plugin route"
);
proxy_handler!(
    trace_plugin_request,
    trace,
    "tracePluginRequest",
    "TRACE proxy to plugin route"
);

#[cfg(test)]
mod tests {
    use super::is_forwardable_header;

    #[test]
    fn drops_platform_bearer_token() {
        // The access JWT must never reach a plugin, in any case variant.
        assert!(!is_forwardable_header(
            "authorization",
            Some("Bearer abc.def.ghi")
        ));
        assert!(!is_forwardable_header("Authorization", Some("bearer abc")));
        assert!(!is_forwardable_header(
            "authorization",
            Some("  Bearer  abc")
        ));
    }

    #[test]
    fn always_drops_cookie() {
        // Carries the long-lived HttpOnly refresh token.
        assert!(!is_forwardable_header("cookie", Some("refresh=xyz")));
        assert!(!is_forwardable_header("Cookie", None));
    }

    #[test]
    fn forwards_non_bearer_authorization_scheme() {
        // The print plugin's station secret is addressed to the plugin, not a
        // platform credential — it must pass through so `authenticate_station`
        // can validate it.
        assert!(is_forwardable_header(
            "authorization",
            Some("PrintStation tok-7f3a")
        ));
        assert!(is_forwardable_header("Authorization", Some("ApiKey k123")));
    }

    #[test]
    fn drops_undecodable_authorization() {
        // A value we cannot read as UTF-8 is treated as a credential and dropped.
        assert!(!is_forwardable_header("authorization", None));
    }

    #[test]
    fn forwards_ordinary_headers() {
        assert!(is_forwardable_header(
            "content-type",
            Some("application/json")
        ));
        assert!(is_forwardable_header("x-forwarded-for", Some("10.0.0.1")));
        assert!(is_forwardable_header("accept", Some("*/*")));
    }
}
