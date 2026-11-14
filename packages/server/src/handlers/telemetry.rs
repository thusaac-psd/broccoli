use axum::{extract::State, http::StatusCode, response::IntoResponse};
use serde::Deserialize;
use tracing::instrument;
use utoipa::ToSchema;

use crate::error::{AppError, ErrorBody};
use crate::extractors::json::AppJson;
use crate::state::AppState;

const TELEMETRY_BODY_LIMIT_BYTES: usize = 16 * 1024;

const MAX_VITALS_PER_REQUEST: usize = 50;

#[derive(Debug, Deserialize, ToSchema)]
pub struct ClientError {
    #[schema(example = "TypeError: Cannot read properties of undefined")]
    pub message: String,
    #[serde(default)]
    #[schema(example = "at fn (https://app.example.com/assets/main.js:1:42)")]
    pub stack: Option<String>,
    #[serde(default)]
    #[schema(example = "https://app.example.com/problems/1")]
    pub url: Option<String>,
    #[serde(default)]
    #[schema(example = "req-7f3c9a")]
    pub request_id: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct WebVital {
    #[schema(example = "LCP")]
    pub name: String,
    #[schema(example = 1234.5)]
    pub value: f64,
    #[serde(default)]
    #[schema(example = "https://app.example.com/problems/1")]
    pub url: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct WebVitalsPayload {
    pub vitals: Vec<WebVital>,
}

#[utoipa::path(
    post,
    path = "/errors",
    tag = "Telemetry",
    operation_id = "reportError",
    summary = "Report a client-side error",
    description = "Public endpoint for the web frontend to report uncaught client errors. No authentication required. Bodies are capped at 16 KiB; rate limiting is intentionally deferred (see handler docs). Always returns 204 on success.",
    request_body = ClientError,
    responses(
        (status = 204, description = "Error recorded"),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 413, description = "Payload too large (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 500, description = "Internal error (INTERNAL_ERROR)", body = ErrorBody),
    ),
)]
#[instrument(
    skip(_state, payload),
    fields(
        url = payload.url.as_deref().unwrap_or("unknown"),
        request_id = payload.request_id.as_deref().unwrap_or("none"),
    ),
)]
pub async fn report_error(
    State(_state): State<AppState>,
    AppJson(payload): AppJson<ClientError>,
) -> Result<impl IntoResponse, AppError> {
    tracing::warn!(
        error_message = %payload.message,
        stack = payload.stack.as_deref().unwrap_or(""),
        "Client error reported"
    );
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    post,
    path = "/vitals",
    tag = "Telemetry",
    operation_id = "reportVitals",
    summary = "Report web-vitals measurements",
    description = "Public endpoint for the web frontend to report web-vitals (LCP, CLS, INP, etc.). No authentication required. Bodies are capped at 16 KiB and at most 50 vitals are processed per request; rate limiting is intentionally deferred (see handler docs). Always returns 204 on success.",
    request_body = WebVitalsPayload,
    responses(
        (status = 204, description = "Vitals recorded"),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 413, description = "Payload too large (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 500, description = "Internal error (INTERNAL_ERROR)", body = ErrorBody),
    ),
)]
#[instrument(skip(_state, payload), fields(vital_count = payload.vitals.len()))]
pub async fn report_vitals(
    State(_state): State<AppState>,
    AppJson(payload): AppJson<WebVitalsPayload>,
) -> Result<impl IntoResponse, AppError> {
    for vital in payload.vitals.iter().take(MAX_VITALS_PER_REQUEST) {
        tracing::info!(
            name = %vital.name,
            value = vital.value,
            url = vital.url.as_deref().unwrap_or("unknown"),
            "Web vital"
        );
    }
    Ok(StatusCode::NO_CONTENT)
}

pub fn telemetry_body_limit() -> usize {
    TELEMETRY_BODY_LIMIT_BYTES
}
