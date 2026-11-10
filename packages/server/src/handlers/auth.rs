use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use axum_client_ip::ClientIpSource;
use axum_extra::extract::CookieJar;
use sea_orm::*;
use tracing::instrument;

use crate::client_ip::ClientIp;
use crate::config::ServerConfig;
// visibility-bypass-audited: this module has no permission gate to name
// because it has no third-party resource parameter to gate - every query is
// scoped to the identity resolved from the request's own credentials
// (password match, refresh-token selector match, or the caller's own JWT
// claims), never to a user/contest/problem id supplied by the caller. Login
// cannot be pointed at another account (`cannot_login_with_wrong_password`,
// `cannot_login_with_nonexistent_username`), a refresh only rotates the
// token family it was issued from and a stale/reused one kills that family
// rather than touching anyone else's
// (`reused_refresh_token_past_grace_kills_the_family`), and `/me` returns
// only the caller's own profile with no id parameter
// (`authenticated_user_can_retrieve_their_profile`) - all in
// tests/integration/auth.rs. There is no `Resource::User` in the kernel this
// could route through.
use crate::entity::user;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::models::auth::{
    CliRefreshRequest, CliTokenResponse, DeviceAuthorizeRequest, DeviceCodeRequest,
    DeviceCodeResponse, DeviceTokenRequest, LoginRequest, LoginResponse, MeResponse,
    RegisterRequest, RegisterResponse, validate_login_request, validate_register_request,
};
use crate::services::auth_tokens::{
    issue_access_token, mint_refresh_token, revoke_presented, rotate_refresh_token,
};
use crate::services::device_auth::{self, PollOutcome};
use crate::state::AppState;
use crate::utils::soft_delete::SoftDeletable;
use crate::utils::{hash, refresh};

#[utoipa::path(
    post,
    path = "/register",
    tag = "Auth",
    operation_id = "registerUser",
    summary = "Register a new user account",
    description = "Creates a new user account with the provided credentials. No authentication required. Returns 409 USERNAME_TAKEN if the username is already in use.",
    request_body = RegisterRequest,
    responses(
        (status = 201, description = "User registered", body = RegisterResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 409, description = "Username taken (USERNAME_TAKEN)", body = ErrorBody),
    ),
)]
#[instrument(skip(state, payload), fields(username = %payload.username))]
pub async fn register(
    State(state): State<AppState>,
    AppJson(payload): AppJson<RegisterRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_register_request(&payload)?;

    let prepared = crate::services::user_accounts::prepare_accounts(vec![
        crate::models::user::CreateUserRequest {
            username: payload.username,
            display_name: payload.display_name,
            password: Some(payload.password),
        },
    ])
    .await?;
    let entry = prepared
        .into_iter()
        .next()
        .ok_or_else(|| AppError::Internal("Missing prepared account".into()))?;
    let txn = state.db.begin().await?;
    let user = crate::services::user_accounts::insert_account(&txn, &entry).await?;

    txn.commit().await?;
    Ok((StatusCode::CREATED, Json(RegisterResponse::from(user))))
}

#[utoipa::path(
    post,
    path = "/login",
    tag = "Auth",
    operation_id = "loginUser",
    summary = "Log in and obtain a JWT token",
    description = "Authenticates the user and returns a short-lived JWT access token. Sets a long-lived HttpOnly cookie containing a refresh token. Returns 401 INVALID_CREDENTIALS on wrong username or password.",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Login successful", body = LoginResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Invalid credentials (INVALID_CREDENTIALS)", body = ErrorBody),
        (status = 429, description = "Rate limited (RATE_LIMITED). Includes Retry-After header when auth rate limiting is enabled.", body = ErrorBody),
    ),
)]
#[instrument(skip(state, payload, jar), fields(username = %payload.username))]
pub async fn login(
    State(state): State<AppState>,
    ClientIp(client_ip): ClientIp,
    jar: CookieJar,
    AppJson(payload): AppJson<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    validate_login_request(&payload)?;

    let username = payload.username.trim();

    // Contest-safe brute-force throttle: only FAILED attempts for this
    // (username, client IP) pair count and a success clears them, so a
    // legitimate sign-in is never blocked even when a whole venue shares one NAT
    // IP. Fails open when the client IP is unknown.
    if let Some(ip) = client_ip
        && let Some(retry_after) = state.login_throttle.check(username, ip)
    {
        return Err(AppError::RateLimited { retry_after });
    }

    let maybe_user = user::Entity::find_active()
        .filter(user::Column::Username.eq(username))
        .one(&state.db)
        .await?;

    // An unknown username is checked against a dummy hash so both cases cost
    // the same.
    let is_valid = hash::verify_password_off_thread(
        payload.password.clone(),
        maybe_user.as_ref().map(|u| u.password.clone()),
    )
    .await
    .map_err(|e| AppError::Internal(format!("Password verify error: {e}")))?;

    let user = match maybe_user {
        Some(u) if is_valid => u,
        _ => {
            if let Some(ip) = client_ip {
                state.login_throttle.record_failure(username, ip);
            }
            return Err(AppError::InvalidCredentials);
        }
    };

    // Successful auth: clear any accumulated failures for this pair.
    if let Some(ip) = client_ip {
        state.login_throttle.clear(username, ip);
    }

    let grant = issue_access_token(&state.db, &state.config.auth.jwt_secret, user.id).await?;

    let refresh_token = mint_refresh_token(&state.db, user.id).await?;
    let cookie = refresh::build_refresh_cookie(&refresh_token, state.config.auth.secure_cookies);

    Ok((jar.add(cookie), Json(LoginResponse::from(grant))))
}

#[utoipa::path(
    post,
    path = "/refresh",
    tag = "Auth",
    operation_id = "refreshToken",
    summary = "Refresh access token",
    description = "Exchanges a valid HttpOnly refresh token cookie for a new short-lived access token and rotates the cookie. Fails if the user is banned or the token is expired/revoked. A token presented again within 30 seconds of its rotation (a concurrent refresh, or a retry after a lost response) is answered with a new token pair; presented later, it revokes every refresh token of the user.",
    responses(
        (status = 200, description = "Token refreshed", body = LoginResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
)]
#[instrument(skip(state, jar))]
pub async fn refresh(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    let presented = jar
        .get(refresh::REFRESH_COOKIE_NAME)
        .map(|c| c.value().to_string())
        .ok_or(AppError::TokenMissing)?;

    let (grant, refresh_token) =
        rotate_refresh_token(&state.db, &state.config.auth.jwt_secret, &presented).await?;
    let new_cookie =
        refresh::build_refresh_cookie(&refresh_token, state.config.auth.secure_cookies);

    Ok((jar.add(new_cookie), Json(LoginResponse::from(grant))))
}

#[utoipa::path(
    post,
    path = "/logout",
    tag = "Auth",
    operation_id = "logoutUser",
    summary = "Log out user",
    description = "Revokes the refresh token and clears the cookie.",
    responses(
        (status = 204, description = "Logged out successfully"),
    ),
)]
#[instrument(skip(state, jar))]
pub async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, AppError> {
    if let Some(cookie) = jar.get(refresh::REFRESH_COOKIE_NAME) {
        revoke_presented(&state.db, cookie.value()).await?;
    }

    let removal_cookie = refresh::build_removal_cookie(state.config.auth.secure_cookies);

    Ok((StatusCode::NO_CONTENT, jar.add(removal_cookie)))
}

#[utoipa::path(
    get,
    path = "/me",
    tag = "Auth",
    operation_id = "getCurrentUser",
    summary = "Get current authenticated user profile",
    description = "Returns the authenticated user's profile, including the roles and permissions embedded in their JWT.",
    responses(
        (status = 200, description = "Current user info", body = MeResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(user_id = auth_user.user_id))]
pub async fn me(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<MeResponse>, AppError> {
    let user = user::Entity::find_active_by_id(auth_user.user_id)
        .one(&state.db)
        .await?
        .ok_or(AppError::TokenInvalid)?;
    Ok(Json(MeResponse {
        id: auth_user.user_id,
        username: user.username,
        display_name: user.display_name,
        roles: auth_user.roles,
        permissions: auth_user.permissions,
    }))
}

const USER_CODE_CHARSET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
const USER_CODE_LEN: usize = 8;
const DEVICE_CODE_EXPIRY_SECS: u64 = 900;
const POLL_INTERVAL_SECS: u64 = 5;

fn generate_device_code() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: [u8; 32] = rng.random();
    hex::encode(bytes)
}

fn generate_user_code() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let code: String = (0..USER_CODE_LEN)
        .map(|_| {
            let idx = rng.random_range(0..USER_CODE_CHARSET.len());
            USER_CODE_CHARSET[idx] as char
        })
        .collect();
    format!("{}-{}", &code[..4], &code[4..])
}

fn normalize_user_code(code: &str) -> String {
    code.to_uppercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Resolve the externally reachable origin (`scheme://authority`, no trailing
/// slash) for links the server hands back to a caller, such as the device-flow
/// `verification_url`. The bind address is NOT a public origin: under the
/// shipped Docker default `server.host = 0.0.0.0` the naive
/// `http://{host}:{port}` yields an unreachable `http://0.0.0.0:3000`.
///
/// Resolution order, most authoritative first:
/// 1. `server.public_base_url` when set - the operator's explicit declaration.
/// 2. The request's own reachable authority: `X-Forwarded-Host` /
///    `X-Forwarded-Proto` when the peer is a trusted proxy, otherwise the
///    `Host` header. The caller just reached us at this authority, so a link
///    built from it routes back. The URL is only ever returned to that same
///    caller, so a spoofed header poisons only the spoofer's own link.
/// 3. The first configured CORS allow-origin.
/// 4. `http://{host}:{port}` - last resort, may be unreachable.
fn resolve_public_origin(
    server: &ServerConfig,
    headers: &HeaderMap,
    forwarded_trusted: bool,
) -> String {
    if let Some(base) = server.public_base_url.as_deref() {
        let base = base.trim().trim_end_matches('/');
        if !base.is_empty() {
            return base.to_string();
        }
    }

    if let Some(origin) = origin_from_request(headers, forwarded_trusted) {
        return origin;
    }

    if let Some(cors) = server.cors.allow_origins.first() {
        let cors = cors.trim().trim_end_matches('/');
        if !cors.is_empty() {
            return cors.to_string();
        }
    }

    format!("http://{}:{}", server.host, server.port)
}

/// Derive `scheme://authority` from the request headers. Forwarded headers are
/// honored only when the peer is a trusted proxy; otherwise the untrusted
/// `Host` header is used over an assumed `http` scheme. Returns None when no
/// host authority is present.
fn origin_from_request(headers: &HeaderMap, forwarded_trusted: bool) -> Option<String> {
    let authority = if forwarded_trusted {
        leftmost_token(headers, "x-forwarded-host").or_else(|| host_header(headers))
    } else {
        host_header(headers)
    }?;

    let scheme = if forwarded_trusted {
        leftmost_token(headers, "x-forwarded-proto").unwrap_or_else(|| "http".to_string())
    } else {
        "http".to_string()
    };

    Some(format!("{scheme}://{authority}"))
}

fn host_header(headers: &HeaderMap) -> Option<String> {
    let value = headers.get("host")?.to_str().ok()?.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// First comma-separated token of a header value (e.g. `a, b` -> `a`), trimmed.
/// Proxies append to `X-Forwarded-*` lists left-to-right, so the leftmost token
/// is the value seen by the outermost (client-facing) proxy.
fn leftmost_token(headers: &HeaderMap, name: &str) -> Option<String> {
    let value = headers.get(name)?.to_str().ok()?;
    let token = value.split(',').next()?.trim();
    if token.is_empty() {
        None
    } else {
        Some(token.to_string())
    }
}

#[utoipa::path(
    post,
    path = "/device-code",
    tag = "Auth",
    operation_id = "requestDeviceCode",
    summary = "Request a device authorization code",
    description = "Initiates the device authorization flow (RFC 8628). Returns a device code for polling and a user code for the user to enter in the browser.",
    request_body = DeviceCodeRequest,
    responses(
        (status = 200, description = "Device code generated", body = DeviceCodeResponse),
        (status = 429, description = "Too many pending device codes (RATE_LIMITED)", body = ErrorBody),
    ),
)]
#[instrument(skip(state, headers, source))]
pub async fn request_device_code(
    State(state): State<AppState>,
    headers: HeaderMap,
    source: Option<Extension<ClientIpSource>>,
    AppJson(_payload): AppJson<DeviceCodeRequest>,
) -> Result<Json<DeviceCodeResponse>, AppError> {
    let (device_code, user_code) = device_auth::create(
        &state.db,
        DEVICE_CODE_EXPIRY_SECS,
        || (generate_device_code(), generate_user_code()),
        normalize_user_code,
    )
    .await?;

    let forwarded_trusted = matches!(
        source,
        Some(Extension(ClientIpSource::RightmostXForwardedFor))
    );
    let origin = resolve_public_origin(&state.config.server, &headers, forwarded_trusted);

    Ok(Json(DeviceCodeResponse {
        device_code,
        user_code,
        verification_url: format!("{}/auth/device", origin),
        expires_in: DEVICE_CODE_EXPIRY_SECS,
        interval: POLL_INTERVAL_SECS,
    }))
}

#[utoipa::path(
    post,
    path = "/device-authorize",
    tag = "Auth",
    operation_id = "authorizeDevice",
    summary = "Authorize a device code",
    description = "Authorizes a pending device code by entering the user code. Requires the user to be logged in via JWT. The CLI will receive a fresh JWT on its next poll.",
    request_body = DeviceAuthorizeRequest,
    responses(
        (status = 200, description = "Device authorized", body = serde_json::Value),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Code not found or expired (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Code already used (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload))]
pub async fn authorize_device(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppJson(payload): AppJson<DeviceAuthorizeRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    // `FreshAuthUser` refused a stale or deleted caller. Only the approving
    // user is recorded; the CLI's token is minted from that user's current
    // grants when it collects it.
    device_auth::authorize(
        &state.db,
        &normalize_user_code(&payload.user_code),
        auth_user.user_id,
    )
    .await?;

    Ok(Json(serde_json::json!({
        "message": "Device authorized successfully"
    })))
}

#[utoipa::path(
    post,
    path = "/device-token",
    tag = "Auth",
    operation_id = "pollDeviceToken",
    summary = "Poll for device authorization token",
    description = "Polling endpoint for the device code flow. Returns a JWT minted from the approving user's current roles once the device is authorized, consuming the code. Otherwise returns 400 with `error` set to 'authorization_pending' (waiting), 'slow_down' (polled faster than the interval), 'expired_token' (unknown, expired or already collected) or 'access_denied' (the approving user was deleted).",
    request_body = DeviceTokenRequest,
    responses(
        (status = 200, description = "Token granted", body = serde_json::Value),
        (status = 400, description = "Authorization pending or expired", body = serde_json::Value),
    ),
)]
#[instrument(skip(state, payload))]
pub async fn poll_device_token(
    State(state): State<AppState>,
    AppJson(payload): AppJson<DeviceTokenRequest>,
) -> Result<impl IntoResponse, AppError> {
    let outcome = device_auth::poll(
        &state.db,
        &state.config.auth.jwt_secret,
        &payload.device_code,
        POLL_INTERVAL_SECS,
    )
    .await?;
    let (status, body) = match outcome {
        PollOutcome::Token(token) => (StatusCode::OK, serde_json::json!({ "token": token })),
        PollOutcome::Pending => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "authorization_pending" }),
        ),
        PollOutcome::SlowDown => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "slow_down" }),
        ),
        PollOutcome::Expired => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "expired_token" }),
        ),
        PollOutcome::Denied => (
            StatusCode::BAD_REQUEST,
            serde_json::json!({ "error": "access_denied" }),
        ),
    };
    Ok((status, Json(body)))
}

#[utoipa::path(
    post,
    path = "/cli-token",
    tag = "Auth",
    operation_id = "issueCliToken",
    summary = "Issue a long-lived refresh token for the CLI",
    description = "Exchanges the caller's current access token for a fresh access token plus a long-lived refresh token. The CLI stores the refresh token and uses POST /auth/cli-refresh to obtain new access tokens without re-prompting.",
    responses(
        (status = 200, description = "CLI token issued", body = CliTokenResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(user_id = auth_user.user_id))]
pub async fn issue_cli_token(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
) -> Result<Json<CliTokenResponse>, AppError> {
    // `FreshAuthUser` refused a stale or deleted caller; the access token is
    // built from current grants before any refresh row exists for it.
    let token = issue_access_token(&state.db, &state.config.auth.jwt_secret, auth_user.user_id)
        .await?
        .token;
    let refresh_token = mint_refresh_token(&state.db, auth_user.user_id).await?;

    Ok(Json(CliTokenResponse {
        token,
        refresh_token,
    }))
}

#[utoipa::path(
    post,
    path = "/cli-refresh",
    tag = "Auth",
    operation_id = "cliRefreshToken",
    summary = "Refresh a CLI access token using a stored refresh token",
    description = "Body-based (cookie-free) refresh for CLI clients. Validates and rotates the supplied refresh token, returning a new access token and a new refresh token. The old refresh token is invalidated, with the same 30-second replay grace as POST /auth/refresh.",
    request_body = CliRefreshRequest,
    responses(
        (status = 200, description = "Token refreshed", body = CliTokenResponse),
        (status = 401, description = "Invalid or expired refresh token (TOKEN_INVALID)", body = ErrorBody),
    ),
)]
#[instrument(skip(state, payload))]
pub async fn cli_refresh(
    State(state): State<AppState>,
    AppJson(payload): AppJson<CliRefreshRequest>,
) -> Result<Json<CliTokenResponse>, AppError> {
    let (grant, refresh_token) = rotate_refresh_token(
        &state.db,
        &state.config.auth.jwt_secret,
        &payload.refresh_token,
    )
    .await?;

    Ok(Json(CliTokenResponse {
        token: grant.token,
        refresh_token,
    }))
}

#[cfg(test)]
mod device_origin_tests {
    use super::*;
    use axum::http::{HeaderName, HeaderValue};

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn server_config() -> ServerConfig {
        // Defaults: host 127.0.0.1, port 3000, empty CORS, no public_base_url.
        ServerConfig::default()
    }

    #[test]
    fn explicit_public_base_url_wins_and_trailing_slash_is_trimmed() {
        let mut cfg = server_config();
        cfg.public_base_url = Some("https://judge.example.org/".to_string());
        cfg.cors.allow_origins = vec!["https://cors.example".to_string()];
        // Even a trusted forwarded host does not override the explicit setting.
        let h = headers(&[
            ("host", "internal:3000"),
            ("x-forwarded-host", "proxy.example"),
        ]);
        assert_eq!(
            resolve_public_origin(&cfg, &h, true),
            "https://judge.example.org"
        );
    }

    #[test]
    fn derives_origin_from_host_header_when_untrusted() {
        let cfg = server_config();
        let h = headers(&[("host", "judge.lan:3000")]);
        assert_eq!(
            resolve_public_origin(&cfg, &h, false),
            "http://judge.lan:3000"
        );
    }

    #[test]
    fn bind_all_host_is_not_used_when_a_host_header_is_present() {
        // Regression: the shipped Docker default binds 0.0.0.0, which must never
        // leak into a link when the request itself carries a reachable authority.
        let mut cfg = server_config();
        cfg.host = "0.0.0.0".to_string();
        let h = headers(&[("host", "judge.lan:3000")]);
        let origin = resolve_public_origin(&cfg, &h, false);
        assert_eq!(origin, "http://judge.lan:3000");
        assert!(!origin.contains("0.0.0.0"));
    }

    #[test]
    fn trusted_proxy_uses_forwarded_host_and_proto() {
        let cfg = server_config();
        let h = headers(&[
            ("host", "internal:3000"),
            ("x-forwarded-host", "judge.example.org"),
            ("x-forwarded-proto", "https"),
        ]);
        assert_eq!(
            resolve_public_origin(&cfg, &h, true),
            "https://judge.example.org"
        );
    }

    #[test]
    fn untrusted_peer_ignores_forwarded_headers_and_uses_host() {
        let cfg = server_config();
        let h = headers(&[
            ("host", "judge.lan"),
            ("x-forwarded-host", "evil.example"),
            ("x-forwarded-proto", "https"),
        ]);
        // Forwarded headers from an untrusted peer are not honored: scheme stays
        // http and the authority is the real Host, not the spoofed forward.
        assert_eq!(resolve_public_origin(&cfg, &h, false), "http://judge.lan");
    }

    #[test]
    fn trusted_proxy_without_forwarded_host_falls_back_to_host() {
        // A trusted proxy that forwards no X-Forwarded-Host must still resolve a
        // reachable origin from the Host authority rather than the bind address.
        let mut cfg = server_config();
        cfg.host = "0.0.0.0".to_string();
        let h = headers(&[("host", "judge.lan:3000")]);
        assert_eq!(
            resolve_public_origin(&cfg, &h, true),
            "http://judge.lan:3000"
        );
    }

    #[test]
    fn trusted_forwarded_host_without_proto_defaults_to_http() {
        let cfg = server_config();
        let h = headers(&[("x-forwarded-host", "judge.example")]);
        assert_eq!(
            resolve_public_origin(&cfg, &h, true),
            "http://judge.example"
        );
    }

    #[test]
    fn forwarded_lists_take_the_leftmost_token() {
        let cfg = server_config();
        let h = headers(&[
            ("x-forwarded-host", "a.example, b.example"),
            ("x-forwarded-proto", "https, http"),
        ]);
        assert_eq!(resolve_public_origin(&cfg, &h, true), "https://a.example");
    }

    #[test]
    fn falls_back_to_cors_origin_when_no_host_header() {
        let mut cfg = server_config();
        cfg.cors.allow_origins = vec!["https://cors.example/".to_string()];
        let h = headers(&[]);
        assert_eq!(
            resolve_public_origin(&cfg, &h, false),
            "https://cors.example"
        );
    }

    #[test]
    fn last_resort_is_host_port_when_nothing_else_is_known() {
        let mut cfg = server_config();
        cfg.host = "0.0.0.0".to_string();
        let h = headers(&[]);
        // Documents the unavoidable last resort: no request authority, no CORS
        // origin, no explicit base URL. Operators hit this only with an empty
        // Host header, which real clients do not send.
        assert_eq!(
            resolve_public_origin(&cfg, &h, false),
            "http://0.0.0.0:3000"
        );
    }
}
