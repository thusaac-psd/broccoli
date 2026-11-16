//! The error body every failed request returns.

use serde::{Deserialize, Serialize};

/// What went wrong. `code` and `details` are the contract a client reads;
/// `message` is an English diagnostic for logs and for people reading the
/// API, the same whatever `Accept-Language` says. A client shows the user its
/// own text for `code` (the web app's `apiError.<code>` keys) and, for a
/// refused config, for each `details.fields[]` entry (its `kind` and
/// parameters, or the plugin's translation `key`). A code a plugin sent (a
/// plugin route's client error, a hook's rejection) is `<plugin id>.<CODE>`,
/// translated only by `<plugin id>.apiError.<CODE>` in that plugin's pack; a
/// code without a plugin id is the server's own.
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
    /// An English description for logs and developers; never translated.
    #[schema(example = "Title must be 1-256 characters")]
    pub message: String,
    /// What the code is about, in a shape each code documents: for example
    /// `fields` for `VALIDATION_ERROR` on a config, `issues` for
    /// `CONFIG_WOULD_BREAK`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}
