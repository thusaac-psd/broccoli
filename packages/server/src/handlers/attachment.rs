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

// visibility-bypass-audited: `upload_attachment`/`delete_attachment` require
// perm::PROBLEM_EDIT and are write-only. The viewer-facing reads (the
// standalone and contest-scoped list and download routes) all route through
// `VisibilityKernel` below (`Resource::Problem` and `Resource::Attachment`); `problem`/`problem_attachment`
// here are only used for the pre-kernel row fetch and for the admin write
// paths. Pinned by the frozen
// `tests/integration/visibility_matrix.rs::problem_attachment_list` suite.
use crate::entity::{problem, problem_attachment};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::path::AppPath;
use crate::handlers::problem::require_problem_part_reachable;
use crate::models::attachment::{AttachmentListResponse, AttachmentResponse};
use crate::state::AppState;
use crate::upload_limits::LARGE_UPLOAD_LIMIT_BYTES;
use crate::utils::blob::{
    BlobMetadata, build_blob_response, resolve_virtual_path, stream_field_to_store,
    take_required_file,
};
use crate::utils::soft_delete::SoftDeletable;
use crate::visibility::{Action, ProblemScope, Resource, Subject, VisibilityKernel};

pub fn attachment_upload_body_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(LARGE_UPLOAD_LIMIT_BYTES)
}

#[utoipa::path(
    post,
    path = "/",
    tag = "Problem Attachments",
    operation_id = "uploadAttachment",
    summary = "Upload an attachment to a problem",
    description = "Uploads a file as a problem attachment. The `file` multipart field is required. \
        An optional `path` field sets the virtual path (defaults to the filename). \
        Re-uploading to the same path silently replaces the previous attachment.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body(content_type = "multipart/form-data", description = "File upload with optional path"),
    responses(
        (status = 201, description = "Attachment created", body = AttachmentResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, multipart), fields(problem_id))]
pub async fn upload_attachment(
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

    let model = problem_attachment::ActiveModel {
        id: Set(ref_id),
        problem_id: Set(problem_id),
        path: Set(path.clone()),
        content_hash: Set(hash.to_hex()),
        filename: Set(filename.clone()),
        content_type: Set(content_type.clone()),
        size: Set(size),
        created_at: Set(now),
    };

    problem_attachment::Entity::insert(model)
        .on_conflict(
            OnConflict::columns([
                problem_attachment::Column::ProblemId,
                problem_attachment::Column::Path,
            ])
            .update_columns([
                problem_attachment::Column::ContentHash,
                problem_attachment::Column::Filename,
                problem_attachment::Column::ContentType,
                problem_attachment::Column::Size,
                problem_attachment::Column::CreatedAt,
            ])
            .to_owned(),
        )
        .exec_without_returning(&txn)
        .await?;

    let saved = problem_attachment::Entity::find()
        .filter(problem_attachment::Column::ProblemId.eq(problem_id))
        .filter(problem_attachment::Column::Path.eq(&path))
        .one(&txn)
        .await?
        .ok_or_else(|| AppError::Internal("problem_attachment missing after upsert".into()))?;

    txn.commit().await?;

    Ok((StatusCode::CREATED, Json(AttachmentResponse::from(saved))))
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Problem Attachments",
    operation_id = "listAttachments",
    summary = "List attachments for a problem",
    description = "Returns all attachments for a problem. Admin/setter access via permission; \
        contestants access if the problem is in a contest they can see (public or enrolled).",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 200, description = "Attachment list", body = AttachmentListResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Problem not found or not accessible (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id))]
pub async fn list_attachments(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
) -> Result<Json<serde_json::Value>, AppError> {
    list_attachments_in(&state, &auth_user, ProblemScope::standalone(problem_id)).await
}

#[utoipa::path(
    get,
    path = "/{problem_id}/attachments",
    tag = "Contest Problems",
    operation_id = "listContestProblemAttachments",
    summary = "List attachments for a contest problem",
    description = "Returns the same attachments as `listAttachments`, read through this contest. \
        Visibility is decided for this contest only. The contest must have started.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
    ),
    responses(
        (status = 200, description = "Attachment list", body = AttachmentListResponse),
        (status = 400, description = "Contest has not started yet (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Problem not found or not accessible (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, problem_id))]
pub async fn list_contest_problem_attachments(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id)): AppPath<(i32, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    list_attachments_in(
        &state,
        &auth_user,
        ProblemScope::in_contest(contest_id, problem_id),
    )
    .await
}

/// Shared body of `list_attachments` and `list_contest_problem_attachments`:
/// the attachments of the problem as reached under `scope`.
async fn list_attachments_in(
    state: &AppState,
    auth_user: &AuthUser,
    scope: ProblemScope,
) -> Result<Json<serde_json::Value>, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(state, Subject::from_auth_user(auth_user));

    // Fail fast, before reading the attachment rows below, on a problem the
    // kernel already knows is unreachable under this scope. This is the
    // same Problem decision `get_problem_in` makes (handlers/problem/mod.rs).
    require_problem_part_reachable(
        &kernel,
        state,
        auth_user,
        scope,
        Action::Read,
        "Problem not found",
    )
    .await?;

    let refs = problem_attachment::Entity::find()
        .filter(problem_attachment::Column::ProblemId.eq(scope.problem_id))
        .order_by_asc(problem_attachment::Column::CreatedAt)
        .all(&state.db)
        .await?;

    // Per-attachment decision, distinct from the problem-level gate above: a
    // plugin can still Deny/Redact one specific attachment even when its
    // parent problem is otherwise readable - `Resource::Attachment` exists
    // precisely for that (see the design doc's consumer-validation table).
    // A denied attachment is omitted from the list, never rendered as a
    // placeholder - a placeholder would confirm it exists.
    let items: Vec<(Resource, AttachmentResponse)> = refs
        .into_iter()
        .map(|m| (scope.attachment(m.id), AttachmentResponse::from(m)))
        .collect();

    let visible = kernel.fetch_visible_batch(Action::Read, items).await?;
    let attachments: Vec<serde_json::Value> = visible
        .into_iter()
        .flatten()
        .map(|v| v.into_masked_json())
        .collect::<Result<_, _>>()?;
    let total = attachments.len() as u64;

    Ok(Json(serde_json::json!({
        "attachments": attachments,
        "total": total,
    })))
}

#[utoipa::path(
    get,
    path = "/{ref_id}",
    tag = "Problem Attachments",
    operation_id = "downloadAttachment",
    summary = "Download an attachment",
    description = "Streams the attachment content. Supports ETag-based caching via If-None-Match. \
        Admin/setter access via permission; contestants access if the problem is in a contest \
        they can see (public or enrolled).",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("ref_id" = String, Path, description = "Attachment reference ID (UUID)"),
    ),
    responses(
        (status = 200, description = "Attachment content"),
        (status = 304, description = "Not Modified (ETag match)"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Attachment not found or not accessible (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, headers), fields(problem_id, ref_id))]
pub async fn download_attachment(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, ref_id)): AppPath<(i32, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    download_attachment_in(
        &state,
        &auth_user,
        ProblemScope::standalone(problem_id),
        &ref_id,
        &headers,
    )
    .await
}

#[utoipa::path(
    get,
    path = "/{problem_id}/attachments/{ref_id}",
    tag = "Contest Problems",
    operation_id = "downloadContestProblemAttachment",
    summary = "Download an attachment of a contest problem",
    description = "Streams the same content as `downloadAttachment`, read through this contest. \
        Visibility is decided for this contest only. The contest must have started. \
        Supports ETag-based caching via If-None-Match.",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
        ("ref_id" = String, Path, description = "Attachment reference ID (UUID)"),
    ),
    responses(
        (status = 200, description = "Attachment content"),
        (status = 304, description = "Not Modified (ETag match)"),
        (status = 400, description = "Contest has not started yet, or invalid attachment ID (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 404, description = "Attachment not found or not accessible (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(
    skip(state, auth_user, headers),
    fields(contest_id, problem_id, ref_id)
)]
pub async fn download_contest_problem_attachment(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id, ref_id)): AppPath<(i32, i32, String)>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    download_attachment_in(
        &state,
        &auth_user,
        ProblemScope::in_contest(contest_id, problem_id),
        &ref_id,
        &headers,
    )
    .await
}

/// Shared body of `download_attachment` and
/// `download_contest_problem_attachment`: one attachment of the problem as
/// reached under `scope`.
async fn download_attachment_in(
    state: &AppState,
    auth_user: &AuthUser,
    scope: ProblemScope,
    ref_id: &str,
    headers: &HeaderMap,
) -> Result<Response, AppError> {
    // The kernel OWNS the subject: one kernel per request per subject.
    let kernel = VisibilityKernel::new(state, Subject::from_auth_user(auth_user));

    // Fail fast, before parsing/looking up the specific attachment, on a
    // problem the kernel already knows is unreachable - preserves the
    // pre-kernel handler's exact order (`require_problem_read_access` ran
    // before `Uuid::parse_str`). `Action::Download` throughout this handler:
    // the whole operation is a file-bytes read, not a metadata read.
    require_problem_part_reachable(
        &kernel,
        state,
        auth_user,
        scope,
        Action::Download,
        "Problem not found",
    )
    .await?;

    let ref_uuid = Uuid::parse_str(ref_id)
        .map_err(|_| AppError::Validation("Invalid attachment ID".into()))?;

    let model = problem_attachment::Entity::find_by_id(ref_uuid)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Attachment not found".into()))?;

    if model.problem_id != scope.problem_id {
        return Err(AppError::NotFound("Attachment not found".into()));
    }

    // Per-attachment Download decision, distinct from the problem-level
    // gate above: a plugin can still Deny/Redact ONE specific attachment
    // even when its parent problem is otherwise readable - `Resource::
    // Attachment` exists precisely for that (see the design doc's
    // consumer-validation table entry for Download + Attachment). The
    // kernel memoizes per (Action, Resource); `Action::Download` on
    // `Resource::Attachment` was never asked above (only on `Resource::
    // Problem`), so this is a genuinely new host decision, not a free
    // re-check - it resolves from the scope alone though, via the same
    // `decide_problem_or_sample` rule as the gate above.
    // NOTE: `is_denied()` only checks for `Decision::Deny` - a `Redact`
    // decision here would be indistinguishable from `Allow` and this
    // handler would stream the raw blob unmasked below regardless, since
    // `build_blob_response` bypasses `Visible<T>`/`into_masked_json`
    // entirely (there is no `FieldMask` concept for a binary blob body). No
    // plugin currently returns `Redact` for `Resource::Attachment`, so this
    // is a documented no-op today, not a live bug - see Task 20 Item 6.
    if kernel
        .decide(Action::Download, scope.attachment(model.id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Attachment not found".into()));
    }

    build_blob_response(&BlobMetadata::from(&model), headers, &*state.blob_store).await
}

#[utoipa::path(
    delete,
    path = "/{ref_id}",
    tag = "Problem Attachments",
    operation_id = "deleteAttachment",
    summary = "Delete an attachment reference",
    description = "Removes the attachment reference. The underlying blob is preserved for GC.",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("ref_id" = String, Path, description = "Attachment reference ID (UUID)"),
    ),
    responses(
        (status = 204, description = "Attachment deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Attachment not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id, ref_id))]
pub async fn delete_attachment(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, ref_id)): AppPath<(i32, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    let ref_uuid = Uuid::parse_str(&ref_id)
        .map_err(|_| AppError::Validation("Invalid attachment ID".into()))?;

    let model = problem_attachment::Entity::find_by_id(ref_uuid)
        .one(&state.db)
        .await?
        .ok_or_else(|| AppError::NotFound("Attachment not found".into()))?;

    if model.problem_id != problem_id {
        return Err(AppError::NotFound("Attachment not found".into()));
    }

    problem_attachment::Entity::delete_by_id(ref_uuid)
        .exec(&state.db)
        .await?;

    Ok(StatusCode::NO_CONTENT)
}
