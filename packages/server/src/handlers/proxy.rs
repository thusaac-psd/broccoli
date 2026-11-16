use crate::extractors::query::AppQuery;
use std::collections::HashMap;
use std::convert::Infallible;
use std::time::Duration;

use axum::http::header::AUTHORIZATION;
use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, Method, Response},
    response::{
        IntoResponse, Sse,
        sse::{Event, KeepAlive},
    },
};
use plugin_core::http::{PluginHttpAuth, PluginHttpRequest, PluginHttpResponse};
use tracing::{info, instrument, warn};

use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, fresh_user};
use crate::extractors::path::AppPath;
use crate::services::plugin_route_access::authorize;
use crate::state::AppState;
use crate::utils::jwt;

/// The caller of a plugin request, from its bearer token.
///
/// No `Authorization` header, or a non-Bearer scheme (e.g. the print plugin's
/// station secret), yields `None`: public plugin routes take anonymous
/// callers.
///
/// A Bearer token that is PRESENT but fails verification (expired, bad
/// signature, malformed) or is no longer fresh ([`fresh_user`]) is
/// `Err(TokenInvalid)`, a 401, exactly as core routes answer. Clients
/// refresh their access token on 401; treating such a token as anonymous
/// would instead show the caller a page as if they were signed out.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Option<AuthUser>, AppError> {
    let Some(token) = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Ok(None);
    };
    let claims = jwt::verify(token, &state.config.auth.jwt_secret, state.clock.now())
        .map_err(|_| AppError::TokenInvalid)?;
    fresh_user(state, claims).await.map(Some)
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

    // Every authenticated request is checked for freshness, not only routes
    // with a manifest `permission`: a plugin also gates its own actions on the
    // forwarded `auth.permissions`.
    let auth_user = authenticate(&state, &headers).await?;

    let (handler_name, access, params) = {
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
            route_info.access.clone(),
            matched_route
                .params
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<String, String>>(),
        )
    };

    let resolved = authorize(&state, auth_user.as_ref(), &access, &params).await?;

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
        auth: auth_user.map(|user| PluginHttpAuth {
            user_id: user.user_id,
            username: user.username,
            roles: user.roles,
            permissions: user.permissions,
        }),
        contest: resolved.contest,
        problem_id: resolved.problem_id,
    };

    info!("Forwarding request to plugin handler: {}", handler_name);

    let request = serde_json::to_vec(&request)
        .map_err(|e| AppError::Internal(format!("Failed to serialize plugin request: {e}")))?;
    let response = state
        .plugins
        .call_raw(
            &plugin_id,
            &handler_name,
            request,
            crate::interactive_call(),
        )
        .await?;
    let response: PluginHttpResponse =
        serde_json::from_slice(&response).map_err(plugin_core::error::PluginError::from)?;
    // A plugin answers client errors; a server error is the host's to
    // answer, with its generic body, so no internal detail a plugin put in
    // the body reaches the caller.
    if response.status >= 500 {
        return Err(AppError::Internal(format!(
            "plugin '{plugin_id}' handler '{handler_name}' answered {}: {}",
            response.status,
            response.body.unwrap_or_default()
        )));
    }

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

/// Stream changed snapshots of an existing GET route. Every read goes through
/// the ordinary proxy, including its authentication and plugin access checks.
async fn stream_plugin_request(
    state: AppState,
    plugin_id: String,
    sub_path: String,
    headers: HeaderMap,
    query: HashMap<String, String>,
    body: String,
) -> Result<Response<Body>, AppError> {
    let read = move || {
        handle_plugin_request_impl(
            state.clone(),
            plugin_id.clone(),
            sub_path.clone(),
            Method::GET,
            headers.clone(),
            query.clone(),
            body.clone(),
        )
    };
    let first = read().await?;
    if !first.status().is_success() {
        return Ok(first);
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(60);
    let stream =
        futures::stream::unfold((Some(first), None), move |(mut pending, mut previous)| {
            let read = read.clone();
            async move {
                loop {
                    let response = match pending.take() {
                        Some(first) => first,
                        None => {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                            if tokio::time::Instant::now() >= until {
                                return None;
                            }
                            match read().await {
                                Ok(response) if response.status().is_success() => response,
                                _ => return None, // Reconnect through the normal HTTP auth flow.
                            }
                        }
                    };
                    // This is the finite JSON body produced by the proxy above,
                    // not an unbounded request or upstream network stream.
                    let bytes = to_bytes(response.into_body(), usize::MAX).await.ok()?;
                    if previous.as_ref() == Some(&bytes) {
                        continue;
                    }
                    let snapshot = std::str::from_utf8(&bytes).ok()?;
                    let event = Event::default().event("snapshot").data(snapshot);
                    previous = Some(bytes);
                    return Some((Ok::<_, Infallible>(event), (None, previous)));
                }
            }
        });
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    response
        .headers_mut()
        .insert("cache-control", "no-cache, no-transform".parse().unwrap());
    Ok(response)
}

macro_rules! proxy_handler {
    ($fn_name:ident, $method:ident, $op_id:expr, $summary:expr, request_body = $body_type:ty) => {
        #[utoipa::path(
            $method,
            path = "/{plugin_id}/{*path}",
            tag = "Plugins",
            operation_id = $op_id,
            summary = $summary,
            description = "Handles HTTP requests for plugin-defined routes. The plugin and route are determined by the path parameters. Before the request is forwarded to the plugin's Wasm handler, the host checks the access the route declares in the plugin manifest (sign-in, permission, contest participation) and resolves the contest and problem it addresses with the same visibility rules as the core routes. Errors use the host error shape; a plugin failure is a 500 INTERNAL_ERROR without detail.",
            params(
                ("plugin_id" = String, Path, description = "The unique identifier of the plugin"),
                ("path" = String, Path, description = "The sub-path defined in plugin's manifest")
            ),
            request_body = $body_type,
            responses(
                (status = 200, description = "Success", body = serde_json::Value),
                (status = 400, description = "A path parameter the route declares as an id is not one (VALIDATION_ERROR), or the plugin rejected the request", body = ErrorBody),
                (status = 401, description = "Unauthorized: no token, or an invalid or stale one (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
                (status = 403, description = "Forbidden", body = ErrorBody),
                (status = 404, description = "Plugin, route, or the contest or problem the route addresses not found (NOT_FOUND)", body = ErrorBody),
                (status = 405, description = "Method Not Allowed", body = ErrorBody),
                (status = 500, description = "The plugin failed (INTERNAL_ERROR); the detail is logged, never returned", body = ErrorBody),
                (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
            ),
            security(("jwt" = []))
        )]
        #[instrument(skip(state, headers, body), fields(plugin_id = %plugin_id, sub_path = %sub_path))]
        pub async fn $fn_name(
            State(state): State<AppState>,
            AppPath((plugin_id, sub_path)): AppPath<(String, String)>,
            method: Method,
            headers: HeaderMap,
            AppQuery(query): AppQuery<HashMap<String, String>>,
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
            description = "Handles HTTP requests for plugin-defined routes. The plugin and route are determined by the path parameters. Before the request is forwarded to the plugin's Wasm handler, the host checks the access the route declares in the plugin manifest (sign-in, permission, contest participation) and resolves the contest and problem it addresses with the same visibility rules as the core routes. Errors use the host error shape; a plugin failure is a 500 INTERNAL_ERROR without detail.",
            params(
                ("plugin_id" = String, Path, description = "The unique identifier of the plugin"),
                ("path" = String, Path, description = "The sub-path defined in plugin's manifest")
            ),
            responses(
                (status = 200, description = "Success", body = serde_json::Value),
                (status = 400, description = "A path parameter the route declares as an id is not one (VALIDATION_ERROR), or the plugin rejected the request", body = ErrorBody),
                (status = 401, description = "Unauthorized: no token, or an invalid or stale one (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
                (status = 403, description = "Forbidden", body = ErrorBody),
                (status = 404, description = "Plugin, route, or the contest or problem the route addresses not found (NOT_FOUND)", body = ErrorBody),
                (status = 405, description = "Method Not Allowed", body = ErrorBody),
                (status = 500, description = "The plugin failed (INTERNAL_ERROR); the detail is logged, never returned", body = ErrorBody),
                (status = 503, description = "A plugin this request needs could not answer in time (PLUGIN_UNAVAILABLE); carries Retry-After", body = ErrorBody),
            ),
            security(("jwt" = []))
        )]
        #[instrument(skip(state, headers, body), fields(plugin_id = %plugin_id, sub_path = %sub_path))]
        pub async fn $fn_name(
            State(state): State<AppState>,
            AppPath((plugin_id, sub_path)): AppPath<(String, String)>,
            method: Method,
            headers: HeaderMap,
            AppQuery(query): AppQuery<HashMap<String, String>>,
            body: String,
        ) -> Result<impl IntoResponse, AppError> {
            if method == Method::GET
                && headers.get("accept").is_some_and(|value| value == "text/event-stream")
            {
                return stream_plugin_request(state, plugin_id, sub_path, headers, query, body).await;
            }
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
