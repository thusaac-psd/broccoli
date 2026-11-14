use axum::{
    Json,
    body::Bytes,
    extract::{FromRequest, Request, rejection::JsonRejection},
    http::{HeaderMap, StatusCode, header::CONTENT_TYPE},
};
use serde::de::DeserializeOwned;

use super::body_limit::BodyLimit;
use crate::error::AppError;

/// A JSON request body. Its strings never hold NUL (U+0000): a body with a
/// `\u0000` escape is rejected with a 400 `VALIDATION_ERROR`, since Postgres
/// stores no NUL in text or JSONB. A body over the route's [`BodyLimit`] is a
/// 413 `PAYLOAD_TOO_LARGE`.
pub struct AppJson<T>(pub T);

impl<S, T> FromRequest<S> for AppJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let limit = BodyLimit::of(req.extensions());
        let rejected = |status: StatusCode, text: String| {
            if status == StatusCode::PAYLOAD_TOO_LARGE {
                limit.exceeded()
            } else {
                AppError::Validation(text)
            }
        };
        if !has_json_content_type(req.headers()) {
            // Json's own rejection names the missing content type.
            let Json(value) = Json::<T>::from_request(req, state)
                .await
                .map_err(|e| rejected(e.status(), e.body_text()))?;
            return Ok(AppJson(value));
        }
        let bytes = Bytes::from_request(req, state)
            .await
            .map_err(|e| rejected(e.status(), e.body_text()))?;
        if has_escaped_nul(&bytes) {
            return Err(crate::utils::text::nul_rejected("The request body"));
        }
        let Json(value) =
            Json::<T>::from_bytes(&bytes).map_err(|e| AppError::Validation(e.body_text()))?;
        Ok(AppJson(value))
    }
}

/// `application/json` or any `application/*+json`, as axum's `Json` accepts.
fn has_json_content_type(headers: &HeaderMap) -> bool {
    let Some(content_type) = headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence == "application/json"
        || essence
            .strip_prefix("application/")
            .is_some_and(|subtype| subtype.ends_with("+json"))
}

/// Whether JSON text holds a `\u0000` escape. A backslash starts an escape
/// only after an even run of backslashes, so `\\u0000` is the literal text
/// `\u0000`. Valid JSON carries no raw NUL byte, so the escape is the only
/// way a NUL gets in.
fn has_escaped_nul(json: &[u8]) -> bool {
    let mut backslashes = 0usize;
    for (i, &byte) in json.iter().enumerate() {
        if byte == b'\\' {
            backslashes += 1;
            continue;
        }
        if byte == b'u' && backslashes % 2 == 1 && json[i + 1..].starts_with(b"0000") {
            return true;
        }
        backslashes = 0;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::has_escaped_nul;

    #[test]
    fn finds_a_nul_escape_and_ignores_an_escaped_backslash() {
        assert!(has_escaped_nul(br#"{"a":"x\u0000y"}"#));
        assert!(has_escaped_nul(br#"{"a\u0000":1}"#));
        assert!(has_escaped_nul(br#"["\\\u0000"]"#));
        assert!(!has_escaped_nul(br#"["\\u0000"]"#));
        assert!(has_escaped_nul(br#"["\u00001"]"#));
        assert!(!has_escaped_nul(br#"["\u0001", "u0000"]"#));
    }
}
