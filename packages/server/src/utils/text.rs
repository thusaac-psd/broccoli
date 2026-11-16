use serde_json::Value;

use crate::error::AppError;

/// Read the opaque text exposed by plugin storage, including existing JSON values.
pub fn plugin_storage_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The 400 for text a person submits with a NUL (U+0000) in it, which
/// Postgres cannot store in TEXT or JSONB. `what` names the text.
pub fn nul_rejected(what: &str) -> AppError {
    AppError::Validation(format!("{what} must not contain NUL characters (U+0000)"))
}

/// Reject submitted text holding a NUL, see [`nul_rejected`].
pub fn reject_nul(what: &str, text: &str) -> Result<(), AppError> {
    if text.contains('\0') {
        return Err(nul_rejected(what));
    }
    Ok(())
}
