use axum::{
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use sea_orm::*;
use tracing::{error, warn};

use crate::entity::idempotency_key;
use crate::error::AppError;
use crate::state::AppState;

const MAX_RESPONSE_BODY_SIZE: usize = 4 * 1_048_576;

const STALE_PENDING_SECS: i64 = 120;

/// Response marker for bodies that must never be persisted: wraps any
/// response and adds `Cache-Control: no-store`. The idempotency middleware
/// honours that standard directive (from this wrapper or any layer that sets
/// it) by recording only that the request completed, never the body, so a
/// replay answers 409 instead of re-sending the secret. It also keeps
/// browsers and intermediaries from caching it. Use it on every handler whose
/// response carries a credential (generated passwords, tokens).
pub struct NoStore<T>(pub T);

impl<T: IntoResponse> IntoResponse for NoStore<T> {
    fn into_response(self) -> Response {
        let mut response = self.0.into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// Whether the response forbids storing its body (`Cache-Control: no-store`).
fn forbids_storage(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|directive| directive.trim().eq_ignore_ascii_case("no-store"))
}

pub async fn idempotency_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }

    let key = match request.headers().get("idempotency-key") {
        Some(v) => match v.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                return AppError::Validation("Idempotency-Key header is not valid UTF-8".into())
                    .into_response();
            }
        },
        None => return next.run(request).await,
    };

    if key.is_empty() || key.len() > 255 {
        return AppError::Validation("Idempotency-Key must be 1-255 characters".into())
            .into_response();
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return AppError::Validation(
            "Idempotency-Key must contain only alphanumeric characters, hyphens, and underscores"
                .into(),
        )
        .into_response();
    }

    let user_id = match extract_user_id(&request, &state) {
        Some(id) => id,
        None => return next.run(request).await,
    };

    let request_path = request.uri().path().to_string();
    let request_method = request.method().to_string();
    let db = &state.db;

    match try_claim_key(db, &key, user_id, &request_path, &request_method).await {
        Ok(ClaimResult::Claimed) => {
            let response = next.run(request).await;
            complete_key(db, &key, user_id, response).await
        }
        Ok(ClaimResult::AlreadyExists(existing)) => {
            handle_existing_key(
                db,
                existing,
                &key,
                user_id,
                &request_path,
                &request_method,
                request,
                next,
            )
            .await
        }
        Err(e) => {
            warn!(error = %e, key = %key, "Idempotency middleware DB error, passing through");
            next.run(request).await
        }
    }
}

fn extract_user_id(request: &Request<Body>, state: &AppState) -> Option<i32> {
    let auth_header = request.headers().get("Authorization")?.to_str().ok()?;
    let token = auth_header.strip_prefix("Bearer ")?;
    let claims = crate::utils::jwt::verify(token, &state.config.auth.jwt_secret).ok()?;
    Some(claims.uid)
}

enum ClaimResult {
    Claimed,
    AlreadyExists(idempotency_key::Model),
}

async fn try_claim_key(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    request_path: &str,
    request_method: &str,
) -> Result<ClaimResult, DbErr> {
    let now = Utc::now();
    let result = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO idempotency_key (key, user_id, request_path, request_method, status, created_at)
               VALUES ($1, $2, $3, $4, 'pending', $5)
               ON CONFLICT (key, user_id) DO NOTHING"#,
            [
                key.into(),
                user_id.into(),
                request_path.into(),
                request_method.into(),
                now.into(),
            ],
        ))
        .await?;

    if result.rows_affected() == 1 {
        return Ok(ClaimResult::Claimed);
    }

    let existing = idempotency_key::Entity::find_by_id((key.to_string(), user_id))
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::Custom("Idempotency key vanished between INSERT and SELECT".into())
        })?;

    Ok(ClaimResult::AlreadyExists(existing))
}

#[allow(clippy::too_many_arguments)]
async fn handle_existing_key(
    db: &DatabaseConnection,
    existing: idempotency_key::Model,
    key: &str,
    user_id: i32,
    request_path: &str,
    request_method: &str,
    request: Request<Body>,
    next: Next,
) -> Response {
    match existing.status.as_str() {
        "completed" => {
            if existing.request_path != request_path || existing.request_method != request_method {
                return AppError::IdempotencyKeyMismatch(format!(
                    "This key was used with {} {} but this request is {} {}",
                    existing.request_method, existing.request_path, request_method, request_path
                ))
                .into_response();
            }

            let status = existing
                .response_status
                .and_then(|s| StatusCode::from_u16(s as u16).ok())
                .unwrap_or(StatusCode::OK);
            let body = existing.response_body.unwrap_or_default();

            Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        "completed_too_large" | "completed_not_stored" => {
            if existing.request_path != request_path || existing.request_method != request_method {
                return AppError::IdempotencyKeyMismatch(format!(
                    "This key was used with {} {} but this request is {} {}",
                    existing.request_method, existing.request_path, request_method, request_path
                ))
                .into_response();
            }
            if existing.status == "completed_not_stored" {
                AppError::IdempotencyResponseNotStored.into_response()
            } else {
                AppError::IdempotencyResponseTooLarge.into_response()
            }
        }
        "pending" => {
            let age_secs = (Utc::now() - existing.created_at).num_seconds();
            if age_secs < STALE_PENDING_SECS {
                AppError::IdempotencyKeyInProgress.into_response()
            } else {
                warn!(
                    key = %key,
                    age_secs,
                    "Reclaiming stale pending idempotency key"
                );
                match reclaim_stale_key(db, key, user_id, request_path, request_method).await {
                    Ok(true) => {
                        let response = next.run(request).await;
                        complete_key(db, key, user_id, response).await
                    }
                    Ok(false) => AppError::IdempotencyKeyInProgress.into_response(),
                    Err(e) => {
                        warn!(error = %e, "Failed to reclaim stale key, passing through");
                        next.run(request).await
                    }
                }
            }
        }
        _ => {
            warn!(key = %key, status = %existing.status, "Unknown idempotency key status");
            next.run(request).await
        }
    }
}

async fn reclaim_stale_key(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    request_path: &str,
    request_method: &str,
) -> Result<bool, DbErr> {
    let now = Utc::now();
    let stale_cutoff = now - chrono::Duration::seconds(STALE_PENDING_SECS);

    let result = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE idempotency_key
               SET status = 'pending', created_at = $1, completed_at = NULL,
                   response_status = NULL, response_body = NULL,
                   request_path = $2, request_method = $3
               WHERE key = $4 AND user_id = $5
                 AND status = 'pending' AND created_at < $6"#,
            [
                now.into(),
                request_path.into(),
                request_method.into(),
                key.into(),
                user_id.into(),
                stale_cutoff.into(),
            ],
        ))
        .await?;

    Ok(result.rows_affected() == 1)
}

async fn complete_key(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    response: Response,
) -> Response {
    let status = response.status();
    let is_success = status.is_success();

    if is_success && forbids_storage(response.headers()) {
        // The body carries a secret: record completion (so a retry is not
        // executed twice) but never the body itself.
        if let Err(e) = mark_not_stored(db, key, user_id, status.as_u16() as i16).await {
            error!(error = %e, "Failed to mark no-store idempotency key as completed");
        }
        response
    } else if is_success {
        let (parts, body) = response.into_parts();
        match to_bytes(body, usize::MAX).await {
            Ok(bytes) => {
                if bytes.len() <= MAX_RESPONSE_BODY_SIZE {
                    let body_str = match String::from_utf8(bytes.to_vec()) {
                        Ok(s) => s,
                        Err(_) => {
                            warn!("Response body is not valid UTF-8, skipping idempotency cache");
                            let _ = delete_key(db, key, user_id).await;
                            return Response::from_parts(parts, Body::from(bytes.to_vec()));
                        }
                    };
                    if let Err(e) =
                        mark_completed(db, key, user_id, parts.status.as_u16() as i16, &body_str)
                            .await
                    {
                        error!(error = %e, "Failed to cache idempotency response");
                    }
                    Response::from_parts(parts, Body::from(bytes.to_vec()))
                } else {
                    // Too large to cache. Pass the FULL body through to the
                    // current client, then mark the key so retries get 409.
                    warn!(
                        body_len = bytes.len(),
                        cap = MAX_RESPONSE_BODY_SIZE,
                        "Response body too large to cache for idempotency; passing original body through, retries will be rejected"
                    );
                    if let Err(e) =
                        mark_too_large(db, key, user_id, parts.status.as_u16() as i16).await
                    {
                        error!(error = %e, "Failed to mark oversized idempotency key as completed_too_large");
                    }
                    Response::from_parts(parts, Body::from(bytes.to_vec()))
                }
            }
            Err(e) => {
                error!(error = %e, "Failed to read response body for idempotency cache");
                let _ = delete_key(db, key, user_id).await;
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    } else {
        if let Err(e) = delete_key(db, key, user_id).await {
            warn!(error = %e, "Failed to delete idempotency key after error response");
        }
        response
    }
}

async fn mark_too_large(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    response_status: i16,
) -> Result<(), DbErr> {
    mark_completed_without_body(db, key, user_id, "completed_too_large", response_status).await
}

async fn mark_not_stored(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    response_status: i16,
) -> Result<(), DbErr> {
    mark_completed_without_body(db, key, user_id, "completed_not_stored", response_status).await
}

async fn mark_completed_without_body(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    status: &'static str,
    response_status: i16,
) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"UPDATE idempotency_key
           SET status = $1, response_status = $2, response_body = NULL, completed_at = $3
           WHERE key = $4 AND user_id = $5 AND status = 'pending'"#,
        [
            status.into(),
            response_status.into(),
            Utc::now().into(),
            key.into(),
            user_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn mark_completed(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    response_status: i16,
    response_body: &str,
) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"UPDATE idempotency_key
           SET status = 'completed', response_status = $1, response_body = $2, completed_at = $3
           WHERE key = $4 AND user_id = $5 AND status = 'pending'"#,
        [
            response_status.into(),
            response_body.into(),
            Utc::now().into(),
            key.into(),
            user_id.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn delete_key(db: &DatabaseConnection, key: &str, user_id: i32) -> Result<(), DbErr> {
    idempotency_key::Entity::delete_by_id((key.to_string(), user_id))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn cleanup_expired_keys(db: &DatabaseConnection) {
    let cutoff = Utc::now() - chrono::Duration::hours(24);
    match db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "DELETE FROM idempotency_key WHERE created_at < $1",
            [cutoff.into()],
        ))
        .await
    {
        Ok(result) => {
            let deleted = result.rows_affected();
            if deleted > 0 {
                tracing::info!(deleted, "Cleaned up expired idempotency keys");
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to cleanup expired idempotency keys");
        }
    }
}
