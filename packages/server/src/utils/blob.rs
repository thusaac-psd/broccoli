use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use common::storage::{BlobStore, BoxReader, ContentHash};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::entity::{additional_file, problem_attachment};
use crate::error::AppError;
use crate::utils::filename::{validate_flat_filename, validate_virtual_path};

pub struct BlobMetadata {
    pub content_hash: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size: i64,
}

impl From<&problem_attachment::Model> for BlobMetadata {
    fn from(m: &problem_attachment::Model) -> Self {
        Self {
            content_hash: m.content_hash.clone(),
            filename: m.filename.clone(),
            content_type: m.content_type.clone(),
            size: m.size,
        }
    }
}

impl From<&additional_file::Model> for BlobMetadata {
    fn from(m: &additional_file::Model) -> Self {
        Self {
            content_hash: m.content_hash.clone(),
            filename: m.filename.clone(),
            content_type: m.content_type.clone(),
            size: m.size,
        }
    }
}

pub async fn build_blob_response(
    metadata: &BlobMetadata,
    headers: &HeaderMap,
    blob_store: &dyn BlobStore,
) -> Result<Response, AppError> {
    let etag_value = format!("\"{}\"", metadata.content_hash);
    if let Some(if_none_match) = headers.get(header::IF_NONE_MATCH)
        && let Ok(val) = if_none_match.to_str()
        && (val == etag_value || val == "*")
    {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }

    let hash = ContentHash::from_hex(&metadata.content_hash)?;
    let reader = blob_store.get_stream(&hash).await?;
    let stream = ReaderStream::new(reader);
    let body = Body::from_stream(stream);

    let content_type = metadata
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream");

    // SECURITY: these blobs are user uploads served from the SAME origin as the
    // SPA. An attachment whose bytes are HTML or SVG-with-script, served `inline`
    // with a matching Content-Type, would EXECUTE in a contestant's/admin's
    // browser (stored XSS -> session/JWT theft). Only a small allowlist of
    // non-scriptable RASTER image types is rendered inline (they are embedded in
    // problem markdown); everything else - including SVG - is forced to download
    // as an attachment so the browser never renders it. `nosniff` on every
    // response stops the browser from MIME-sniffing a declared-safe type into an
    // executable one.
    let inline_ok = matches!(
        content_type,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp"
    );
    let disposition = if inline_ok { "inline" } else { "attachment" };

    let response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_LENGTH, metadata.size.to_string())
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition_value(disposition, &metadata.filename),
        )
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::ETAG, &etag_value)
        .header(header::CACHE_CONTROL, "private, max-age=3600")
        .body(body)
        .map_err(|e| AppError::Internal(format!("Failed to build response: {e}")))?;

    Ok(response)
}

pub fn content_disposition_value(disposition: &str, filename: &str) -> String {
    let ascii_safe: String = filename
        .chars()
        .filter(|c| c.is_ascii_graphic() && !matches!(c, '"' | ';' | '\\'))
        .collect();
    let ascii_name = if ascii_safe.is_empty() {
        "download".to_string()
    } else {
        ascii_safe
    };

    let encoded: String = filename
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~' => String::from(b as char),
            _ => format!("%{b:02X}"),
        })
        .collect();

    format!("{disposition}; filename=\"{ascii_name}\"; filename*=UTF-8''{encoded}")
}

/// Unwrap the required `file` field of a problem-file upload into its blob hash,
/// size, and a validated flat filename. Every upload handler drains its own
/// sibling multipart fields (they differ) but ends with this identical required
/// file field; sharing it keeps the "Missing 'file' field" contract in one place.
pub fn take_required_file(
    file_result: Option<(ContentHash, i64)>,
    file_name: Option<String>,
) -> Result<(ContentHash, i64, String), AppError> {
    let (hash, size) =
        file_result.ok_or_else(|| AppError::Validation("Missing 'file' field".into()))?;
    let filename =
        file_name.ok_or_else(|| AppError::Validation("File field must have a filename".into()))?;
    let filename = validate_flat_filename(&filename)
        .map_err(|e| AppError::Validation(e.message().into()))?
        .to_string();
    Ok((hash, size, filename))
}

/// Resolve the stored virtual path for an upload: use the client-supplied `path`
/// field when present and non-blank, else fall back to the upload filename. Both
/// candidates go through `validate_virtual_path`. Shared by the attachment and
/// additional-file handlers.
pub fn resolve_virtual_path(
    virtual_path: Option<&str>,
    filename: &str,
) -> Result<String, AppError> {
    match virtual_path {
        Some(p) if !p.trim().is_empty() => {
            validate_virtual_path(p).map_err(|e| AppError::Validation(e.into()))
        }
        _ => validate_virtual_path(filename).map_err(|e| AppError::Validation(e.into())),
    }
}

pub async fn stream_field_to_store(
    mut field: axum::extract::multipart::Field<'_>,
    blob_store: &dyn BlobStore,
    max_size: u64,
) -> Result<(ContentHash, i64), AppError> {
    let temp_path = std::env::temp_dir().join(format!("broccoli-upload-{}", Uuid::new_v4()));

    let result = async {
        let mut temp_file = tokio::fs::File::create(&temp_path)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to create temp file: {e}")))?;

        let mut total_size: u64 = 0;

        while let Some(chunk) = field.chunk().await.map_err(AppError::from)? {
            total_size += chunk.len() as u64;
            if total_size > max_size {
                return Err(AppError::Validation(format!(
                    "File exceeds maximum size of {max_size} bytes"
                )));
            }
            temp_file
                .write_all(&chunk)
                .await
                .map_err(|e| AppError::Internal(format!("Temp file write failed: {e}")))?;
        }

        temp_file
            .flush()
            .await
            .map_err(|e| AppError::Internal(format!("Temp file flush failed: {e}")))?;
        drop(temp_file);

        let file = tokio::fs::File::open(&temp_path)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to reopen temp file: {e}")))?;
        let reader: BoxReader = Box::new(file);
        let hash = blob_store.put_stream(reader).await?;

        Ok((hash, i64::try_from(total_size).unwrap_or(i64::MAX)))
    }
    .await;

    let _ = tokio::fs::remove_file(&temp_path).await;

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_hash() -> ContentHash {
        ContentHash::from_bytes([0u8; 32])
    }

    #[test]
    fn take_required_file_errors_when_file_missing() {
        let err = take_required_file(None, Some("solution.cpp".into())).unwrap_err();
        assert!(matches!(err, AppError::Validation(m) if m == "Missing 'file' field"));
    }

    #[test]
    fn take_required_file_errors_when_filename_missing() {
        let err = take_required_file(Some((dummy_hash(), 10)), None).unwrap_err();
        assert!(matches!(err, AppError::Validation(m) if m == "File field must have a filename"));
    }

    #[test]
    fn take_required_file_validates_and_trims_filename() {
        let (_, size, filename) =
            take_required_file(Some((dummy_hash(), 42)), Some("  grader.h  ".into())).unwrap();
        assert_eq!(size, 42);
        assert_eq!(filename, "grader.h");
    }

    #[test]
    fn take_required_file_rejects_invalid_filename() {
        // Leading-dash argv-injection guard surfaces as a validation error.
        let err = take_required_file(Some((dummy_hash(), 1)), Some("-o".into())).unwrap_err();
        assert!(matches!(err, AppError::Validation(_)));
    }

    #[test]
    fn resolve_virtual_path_prefers_explicit_path() {
        assert_eq!(
            resolve_virtual_path(Some("include/grader.h"), "fallback.h").unwrap(),
            "include/grader.h"
        );
    }

    #[test]
    fn resolve_virtual_path_falls_back_to_filename_when_blank_or_absent() {
        assert_eq!(
            resolve_virtual_path(Some("   "), "fallback.h").unwrap(),
            "fallback.h"
        );
        assert_eq!(
            resolve_virtual_path(None, "fallback.h").unwrap(),
            "fallback.h"
        );
    }

    #[test]
    fn resolve_virtual_path_rejects_invalid_candidates() {
        assert!(resolve_virtual_path(Some("../etc/passwd"), "ok.h").is_err());
        assert!(resolve_virtual_path(None, "../escape").is_err());
    }
}
