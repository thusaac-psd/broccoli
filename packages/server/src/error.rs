use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum_typed_multipart::TypedMultipartError;
use common::storage::StorageError;
use plugin_core::error::{AssetError, PluginError};
use sea_orm::DbErr;
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
pub struct ErrorBody {
    #[schema(example = "VALIDATION_ERROR")]
    pub code: String,
    #[schema(example = "Title must be 1-256 characters")]
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug)]
pub enum AppError {
    Validation(String),
    PayloadTooLarge(String),
    TokenMissing,
    TokenInvalid,
    InvalidCredentials,
    PermissionDenied,
    NotFound(String),
    MethodNotAllowed,
    Conflict(String),
    UsernameTaken,
    PluginNotReady(String),
    RateLimited {
        retry_after: u64,
    },
    /// Server is overloaded; client should retry after the given duration.
    /// Distinct from `RateLimited` (429): 429 means "your client is too fast",
    /// 503 means "the system as a whole is past capacity". UP#39 uses this
    /// for durable-queue depth backpressure.
    Overloaded {
        retry_after: u64,
    },
    PluginRejection {
        code: String,
        message: String,
        status_code: u16,
        details: Option<serde_json::Value>,
    },
    IdempotencyKeyInProgress,
    IdempotencyKeyMismatch(String),
    /// Idempotent response was too large to cache and cannot be replayed.
    IdempotencyResponseTooLarge,
    /// Idempotent response carried credentials (`Cache-Control: no-store`),
    /// so only its completion was recorded and it cannot be replayed.
    IdempotencyResponseNotStored,
    /// The original request's handler ran but failed or was cancelled, so it
    /// may have committed; the key is never reused for a re-execution.
    IdempotencyOutcomeUnknown,
    Internal(String),
}

impl AppError {
    fn status_and_body(self) -> (StatusCode, ErrorBody) {
        let simple = |code: &str, message: String| ErrorBody {
            code: code.into(),
            message,
            details: None,
        };

        match self {
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, simple("VALIDATION_ERROR", msg)),
            AppError::PayloadTooLarge(msg) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                simple("PAYLOAD_TOO_LARGE", msg),
            ),
            AppError::TokenMissing => (
                StatusCode::UNAUTHORIZED,
                simple("TOKEN_MISSING", "Authentication required".into()),
            ),
            AppError::TokenInvalid => (
                StatusCode::UNAUTHORIZED,
                simple("TOKEN_INVALID", "Invalid or expired token".into()),
            ),
            AppError::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                simple("INVALID_CREDENTIALS", "Invalid username or password".into()),
            ),
            AppError::PermissionDenied => (
                StatusCode::FORBIDDEN,
                simple("PERMISSION_DENIED", "Insufficient permissions".into()),
            ),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, simple("NOT_FOUND", msg)),
            AppError::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                simple(
                    "METHOD_NOT_ALLOWED",
                    "HTTP method not allowed for this endpoint".into(),
                ),
            ),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, simple("CONFLICT", msg)),
            AppError::UsernameTaken => (
                StatusCode::CONFLICT,
                simple("USERNAME_TAKEN", "Username is already taken".into()),
            ),
            AppError::PluginNotReady(msg) => {
                (StatusCode::BAD_REQUEST, simple("PLUGIN_NOT_READY", msg))
            }
            AppError::RateLimited { retry_after } => (
                StatusCode::TOO_MANY_REQUESTS,
                simple(
                    "RATE_LIMITED",
                    format!("Rate limit exceeded. Try again in {} seconds", retry_after),
                ),
            ),
            AppError::Overloaded { retry_after } => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    "QUEUE_OVERLOADED",
                    format!(
                        "Server is temporarily overloaded; retry in {} seconds",
                        retry_after
                    ),
                ),
            ),
            AppError::PluginRejection {
                code,
                message,
                status_code,
                details,
            } => {
                let status = StatusCode::from_u16(status_code)
                    .ok()
                    .filter(|s| s.is_client_error())
                    .unwrap_or(StatusCode::BAD_REQUEST);
                (
                    status,
                    ErrorBody {
                        code,
                        message,
                        details,
                    },
                )
            }
            AppError::IdempotencyKeyInProgress => (
                StatusCode::CONFLICT,
                simple(
                    "IDEMPOTENCY_KEY_IN_PROGRESS",
                    "A request with this idempotency key is already being processed".into(),
                ),
            ),
            AppError::IdempotencyKeyMismatch(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple("IDEMPOTENCY_KEY_MISMATCH", msg),
            ),
            AppError::IdempotencyResponseTooLarge => (
                StatusCode::CONFLICT,
                simple(
                    "CONFLICT",
                    "Idempotent response too large to replay; original request \
                     succeeded but cannot be safely re-issued"
                        .into(),
                ),
            ),
            AppError::IdempotencyResponseNotStored => (
                StatusCode::CONFLICT,
                simple(
                    "CONFLICT",
                    "Idempotent response carries credentials and is never stored; \
                     original request succeeded but cannot be replayed"
                        .into(),
                ),
            ),
            AppError::IdempotencyOutcomeUnknown => (
                StatusCode::CONFLICT,
                simple(
                    "IDEMPOTENCY_OUTCOME_UNKNOWN",
                    "The original request with this idempotency key failed after it started \
                     and may have taken effect; check its result and retry with a new key"
                        .into(),
                ),
            ),
            AppError::Internal(detail) => {
                tracing::error!("Internal error: {}", detail);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    simple("INTERNAL_ERROR", "An unexpected error occurred".into()),
                )
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Both 429 (RateLimited) and 503 (Overloaded) carry a Retry-After
        // hint. The header semantics are identical (RFC 7231 §7.1.3); only
        // the status code differs in what's "tired" - the client (429) vs
        // the service as a whole (503).
        let retry_after = match &self {
            AppError::RateLimited { retry_after } | AppError::Overloaded { retry_after } => {
                Some(*retry_after)
            }
            _ => None,
        };

        let (status, body) = self.status_and_body();

        if let Some(seconds) = retry_after {
            (status, [("Retry-After", seconds.to_string())], Json(body)).into_response()
        } else {
            (status, Json(body)).into_response()
        }
    }
}

impl From<DbErr> for AppError {
    fn from(err: DbErr) -> Self {
        AppError::Internal(err.to_string())
    }
}

impl From<StorageError> for AppError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::NotFound(_) => AppError::NotFound(err.to_string()),
            StorageError::SizeLimitExceeded { .. } => AppError::Validation(err.to_string()),
            StorageError::InvalidHash(_) => AppError::Validation(err.to_string()),
            StorageError::Io(_) => AppError::Internal(err.to_string()),
            StorageError::Backend(_) => AppError::Internal(err.to_string()),
        }
    }
}

impl From<PluginError> for AppError {
    fn from(err: PluginError) -> Self {
        match err {
            PluginError::NotFound(detail) => {
                tracing::warn!("Plugin not found: {detail}");
                AppError::NotFound(format!("Plugin '{detail}' not found"))
            }
            PluginError::NotLoaded(detail) => {
                tracing::warn!("Plugin not loaded: {detail}");
                AppError::NotFound(format!("Plugin '{detail}' not found"))
            }
            PluginError::NoRuntime(_) => {
                tracing::warn!("Plugin not ready: {err}");
                AppError::PluginNotReady(err.to_string())
            }
            PluginError::Serialization(_) => AppError::Validation(err.to_string()),
            // Note: `PluginError::PoolTimeout` deliberately falls through to
            // `Internal` (500). Hot-path callers (submission dispatch,
            // evaluate batch, hook firing) retry transparently via
            // `plugin_core::retry::call_raw_with_pool_retry`, so by the time a
            // PoolTimeout reaches HTTP error mapping the server is genuinely
            // overloaded - a 429 would misleadingly tell the *client* to back
            // off when the right signal is server-internal failure (UP#4).
            _ => AppError::Internal(err.to_string()),
        }
    }
}

impl From<AssetError> for AppError {
    fn from(err: AssetError) -> Self {
        match err {
            AssetError::NotFound => AppError::NotFound("Asset not found".into()),
            AssetError::NoWebConfig => AppError::NotFound("Plugin does not have web assets".into()),
            AssetError::PathTraversal => AppError::PermissionDenied,
            AssetError::Io(_) | AssetError::Internal(_) => AppError::Internal(err.to_string()),
        }
    }
}

impl From<TypedMultipartError> for AppError {
    fn from(err: TypedMultipartError) -> Self {
        match err {
            TypedMultipartError::MissingField { .. }
            | TypedMultipartError::WrongFieldType { .. }
            | TypedMultipartError::DuplicateField { .. }
            | TypedMultipartError::UnknownField { .. }
            | TypedMultipartError::InvalidEnumValue { .. }
            | TypedMultipartError::NamelessField => AppError::Validation(err.to_string()),
            TypedMultipartError::FieldTooLarge { .. } => AppError::PayloadTooLarge(err.to_string()),
            TypedMultipartError::InvalidRequest { .. }
            | TypedMultipartError::InvalidRequestBody { .. } => {
                AppError::Validation(err.to_string())
            }
            _ => AppError::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    /// UP#39 follow-up: durable-queue backpressure must surface as
    /// `503 Service Unavailable` with code `QUEUE_OVERLOADED` and the
    /// `Retry-After` header populated from the variant payload. This
    /// pins the contract so a future refactor that re-tags the
    /// variant or drops the header branch fails loudly here.
    #[tokio::test]
    async fn overloaded_maps_to_503_with_retry_after() {
        let err = AppError::Overloaded { retry_after: 7 };
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .expect("Retry-After header"),
            "7"
        );

        let body_bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("json");
        assert_eq!(body["code"], "QUEUE_OVERLOADED");
    }

    /// Sanity check that 429 RateLimited and 503 Overloaded both
    /// flow Retry-After through `IntoResponse` - the implementation
    /// shares the branch, so a regression in either path would also
    /// drop the header for the other.
    #[tokio::test]
    async fn rate_limited_still_maps_to_429_with_retry_after() {
        let err = AppError::RateLimited { retry_after: 1 };
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .expect("Retry-After header"),
            "1"
        );
    }
}
