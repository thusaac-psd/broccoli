use axum::Json;
use axum::extract::{DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use common::storage::ContentHash;
use sea_orm::sea_query::OnConflict;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set, TransactionTrait};
use tracing::instrument;
use uuid::Uuid;

// visibility-bypass-audited: all four handlers require perm::PROBLEM_EDIT,
// pinned by tests/integration/additional_file.rs's
// `contestant_cannot_upload_additional_file`, `contestant_cannot_list_additional_files`,
// `contestant_cannot_download_additional_file`, and
// `contestant_cannot_delete_additional_file`. These are judge-private files
// (grader headers, etc.) - never contestant-facing, regardless of contest
// participation.
use crate::entity::{additional_file, problem};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::path::AppPath;
use crate::models::attachment::{AdditionalFileListResponse, AdditionalFileResponse};
use crate::state::AppState;
use crate::upload_limits::LARGE_UPLOAD_LIMIT_BYTES;
use crate::utils::blob::{
    BlobMetadata, build_blob_response, resolve_virtual_path, stream_field_to_store,
    take_required_file,
};
use crate::utils::soft_delete::SoftDeletable;

pub fn additional_file_upload_body_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(LARGE_UPLOAD_LIMIT_BYTES)
}

fn validate_language_code(lang: &str) -> Result<(), AppError> {
    if lang.is_empty() {
        return Err(AppError::Validation("Language code cannot be empty".into()));
    }
    if !lang
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(AppError::Validation(format!(
            "Language code contains invalid characters: '{lang}'"
        )));
    }
    Ok(())
}

#[utoipa::path(
    post,
    path = "/",
    tag = "Additional Files",
    operation_id = "uploadAdditionalFile",
    summary = "Upload a judge-private additional file",
    description = "Uploads a file that will be compiled alongside contestant submissions for the \
        specified language. The `file` multipart field is required, and the `language` field \
        specifies which language submissions receive this file. An optional `path` field sets \
        the virtual subpath (e.g. `include/grader.h`); defaults to the upload filename. \
        Re-uploading the same language+path replaces the previous version.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body(
        content_type = "multipart/form-data",
        description = "File upload with language code"
    ),
    responses(
        (status = 201, description = "Additional file created", body = AdditionalFileResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, multipart), fields(problem_id))]
pub async fn upload_additional_file(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    problem::Entity::find_active_by_id(problem_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Problem not found".into()))?;

    let mut file_result: Option<(ContentHash, i64)> = None;
    let mut file_name: Option<String> = None;
    let mut language: Option<String> = None;
    let mut virtual_path: Option<String> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::Validation(format!("Multipart error: {e}")))?
    {
        match field.name() {
            Some("file") => {
                file_name = field.file_name().map(|s| s.to_string());
                file_result = Some(
                    stream_field_to_store(
                        field,
                        &*state.blob_store,
                        state.config.storage.max_blob_size,
                    )
                    .await?,
                );
            }
            Some("language") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::Validation(format!("Failed to read language: {e}")))?;
                language = Some(text);
            }
            Some("path") => {
                let text = field
                    .text()
                    .await
                    .map_err(|e| AppError::Validation(format!("Failed to read path: {e}")))?;
                virtual_path = Some(text);
            }
            _ => {}
        }
    }

    let (hash, size, filename) = take_required_file(file_result, file_name)?;

    let lang = language.ok_or_else(|| AppError::Validation("Missing 'language' field".into()))?;
    validate_language_code(&lang)?;

    let path = resolve_virtual_path(virtual_path.as_deref(), &filename)?;

    let content_type = mime_guess::from_path(&filename)
        .first()
        .map(|m| m.to_string());

    let ref_id = Uuid::now_v7();
    let now = Utc::now();

    let txn = state.db.begin().await?;

    problem::Entity::find_active_by_id(problem_id)
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::NotFound("Problem not found".into()))?;

    let model = additional_file::ActiveModel {
        id: Set(ref_id),
        problem_id: Set(problem_id),
        language: Set(lang.clone()),
        path: Set(path.clone()),
        content_hash: Set(hash.to_hex()),
        filename: Set(filename.clone()),
        content_type: Set(content_type),
        size: Set(size),
        created_at: Set(now),
    };

    additional_file::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                additional_file::Column::ProblemId,
                additional_file::Column::Language,
                additional_file::Column::Path,
            ])
            .update_columns([
                additional_file::Column::ContentHash,
                additional_file::Column::Filename,
                additional_file::Column::ContentType,
                additional_file::Column::Size,
                additional_file::Column::CreatedAt,
            ])
            .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;

    let saved = additional_file::Entity::find()
        .filter(additional_file::Column::ProblemId.eq(problem_id))
        .filter(additional_file::Column::Language.eq(&lang))
        .filter(additional_file::Column::Path.eq(&path))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::Internal("additional_file missing after upsert".into()))?;

    txn.commit().await?;

    Ok((
        StatusCode::CREATED,
        Json(AdditionalFileResponse::from(saved)),
    ))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Additional Files",
    operation_id = "listAdditionalFiles",
    summary = "List judge-private additional files for a problem",
    description = "Returns all additional files (stubs, graders) for a problem, \
        across all languages. Requires problem:edit permission.",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 200, description = "Additional file list", body = AdditionalFileListResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id))]
pub async fn list_additional_files(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
) -> Result<Json<AdditionalFileListResponse>, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    problem::Entity::find_active_by_id(problem_id)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Problem not found".into()))?;

    let refs = additional_file::Entity::find()
        .filter(additional_file::Column::ProblemId.eq(problem_id))
        .order_by_asc(additional_file::Column::Language)
        .order_by_asc(additional_file::Column::Path)
        .all(&state.db)
        .await?;

    let total = refs.len() as u64;
    let files = refs.into_iter().map(AdditionalFileResponse::from).collect();

    Ok(Json(AdditionalFileListResponse { files, total }))
}

#[utoipa::path(
    get,
    path = "/{ref_id}",
    tag = "Additional Files",
    operation_id = "downloadAdditionalFile",
    summary = "Download a judge-private additional file",
    description = "Streams the additional file content. Requires problem:edit permission.",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("ref_id" = String, Path, description = "Additional file reference ID (UUID)"),
    ),
    responses(
        (status = 200, description = "File content"),
        (status = 304, description = "Not Modified (ETag match)"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, headers), fields(problem_id, ref_id))]
pub async fn download_additional_file(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, ref_id)): AppPath<(i32, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    let ref_uuid = Uuid::parse_str(&ref_id)
        .map_err(|_| AppError::Validation("Invalid additional file ID".into()))?;

    let model = additional_file::Entity::find_by_id(ref_uuid)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Additional file not found".into()))?;

    if model.problem_id != problem_id {
        return Err(AppError::NotFound("Additional file not found".into()));
    }

    build_blob_response(&BlobMetadata::from(&model), &headers, &*state.blob_store).await
}

#[utoipa::path(
    delete,
    path = "/{ref_id}",
    tag = "Additional Files",
    operation_id = "deleteAdditionalFile",
    summary = "Delete a judge-private additional file",
    description = "Removes the additional file reference. Requires problem:edit permission.",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("ref_id" = String, Path, description = "Additional file reference ID (UUID)"),
    ),
    responses(
        (status = 204, description = "Additional file deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id, ref_id))]
pub async fn delete_additional_file(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, ref_id)): AppPath<(i32, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    let ref_uuid = Uuid::parse_str(&ref_id)
        .map_err(|_| AppError::Validation("Invalid additional file ID".into()))?;

    let model = additional_file::Entity::find_by_id(ref_uuid)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Additional file not found".into()))?;

    if model.problem_id != problem_id {
        return Err(AppError::NotFound("Additional file not found".into()));
    }

    additional_file::Entity::delete_by_id(ref_uuid)
        .exec(&state.db)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}
