use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tracing::instrument;

use crate::error::AppError;
use crate::extractors::path::AppPath;
use crate::state::AppState;

#[instrument(skip(state, headers))]
pub async fn serve_plugin_asset(
    State(state): State<AppState>,
    AppPath((plugin_id, file_path)): AppPath<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let safe_path = state
        .plugins
        .resolve_plugin_asset(&plugin_id, &file_path)
        .map_err(AppError::from)?;

    // A plugin that declares a web entry it never built (or a client asking
    // for a file that does not exist) is a missing asset, not a server
    // fault: answer 404 so clients stop treating it as transient.
    let content = tokio::fs::read(&safe_path)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => AppError::NotFound("Asset not found".into()),
            _ => AppError::Internal(format!("IO error: {}", e)),
        })?;

    let mime = mime_guess::from_path(&safe_path).first_or_octet_stream();

    let mtime = safe_path
        .metadata()
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let etag = format!("W/\"{:x}-{:x}\"", content.len(), mtime);

    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH)
        && let Ok(val) = if_none_match.to_str()
        && (val == etag || val == "*")
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }

    Response::builder()
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::CACHE_CONTROL, "no-cache")
        .header(header::ETAG, etag)
        .body(Body::from(content))
        .map_err(|e| AppError::Internal(e.to_string()))
}
