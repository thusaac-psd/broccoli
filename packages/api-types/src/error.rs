//! The error body every failed request returns.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ErrorBody {
    /// Machine-readable error code. Any POST sent with an `Idempotency-Key`
    /// can also answer: 422 `IDEMPOTENCY_KEY_MISMATCH` (key reused for a
    /// different method, path, query or body); 409 `IDEMPOTENCY_KEY_IN_PROGRESS`
    /// (the original is still running); 409 `IDEMPOTENCY_RESPONSE_NOT_STORED`
    /// (the original succeeded but its response carried credentials and was not
    /// kept); 409 `IDEMPOTENCY_RESPONSE_TOO_LARGE` (the original succeeded but
    /// its response was too large to keep); 409 `IDEMPOTENCY_OUTCOME_UNKNOWN`
    /// (the original failed or was cut off after it started and may have taken
    /// effect); 413 `PAYLOAD_TOO_LARGE` (body too large for an idempotent
    /// request).
    #[schema(example = "VALIDATION_ERROR")]
    pub code: String,
    #[schema(example = "Title must be 1-256 characters")]
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}
