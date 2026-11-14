use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use broccoli_server_sdk::permissions as perm;
use tracing::instrument;

use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, Authorized, FreshAuthUser, ProblemEdit};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::models::problem::*;
use crate::state::AppState;
use crate::utils::problem::find_problem;

#[utoipa::path(
    put,
    path = "/",
    tag = "Checker Source",
    operation_id = "uploadCheckerSource",
    summary = "Upload checker source files",
    description = "Sets the checker source files for a problem. Replaces any existing checker source. Requires `problem:edit` permission. Body limit: 32 MiB.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body = UploadCheckerSourceRequest,
    responses(
        (status = 200, description = "Checker source updated", body = CheckerSourceResponse),
        (status = 400, description = "Validation error", body = ErrorBody),
        (status = 413, description = "Request body over the endpoint's limit (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 404, description = "Problem not found", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(_auth, state, payload), fields(id))]
pub async fn upload_checker_source(
    _auth: Authorized<ProblemEdit>,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    AppJson(payload): AppJson<UploadCheckerSourceRequest>,
) -> Result<Json<CheckerSourceResponse>, AppError> {
    let files = parse_checker_source(payload)?;

    // Existence check: 404 for a missing problem, as before.
    find_problem(&state.db, id).await?;

    let files_value = serde_json::to_value(&files)
        .map_err(|e| AppError::Internal(format!("Failed to serialize checker source: {e}")))?;
    let response_files = serde_json::from_value(files_value.clone())
        .map_err(|e| AppError::Internal(format!("Failed to deserialize checker source: {e}")))?;

    // The checker source is owned by the checker plugin's problem-scoped config,
    // not the core problem model. standard-checkers reads it at judge time via
    // `host.config.get_problem(id, "checker_source")`.
    let _ = crate::services::plugin_config::upsert_config(
        &state.db,
        &crate::services::plugin_config::ConfigTarget::problem(id),
        CHECKER_SOURCE_PLUGIN_ID,
        CHECKER_SOURCE_NAMESPACE,
        files_value,
        None,
        0,
    )
    .await?;

    Ok(Json(CheckerSourceResponse {
        files: Some(response_files),
    }))
}

/// The checker plugin whose problem-scoped config owns the testlib checker
/// source. The dedicated checker-source endpoints proxy to this plugin's config
/// (namespace resolves to `standard-checkers:checker_source`).
const CHECKER_SOURCE_PLUGIN_ID: &str = "standard-checkers";
const CHECKER_SOURCE_NAMESPACE: &str = "checker_source";

#[utoipa::path(
    get,
    path = "/",
    tag = "Checker Source",
    operation_id = "getCheckerSource",
    summary = "Get checker source files",
    description = "Returns the checker source files for a problem. Requires `problem:edit` permission.",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 200, description = "Checker source files", body = CheckerSourceResponse),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 404, description = "Problem not found", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_checker_source(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<CheckerSourceResponse>, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    find_problem(&state.db, id).await?;
    let files: Option<Vec<CheckerSourceFile>> = match crate::services::plugin_config::get_config(
        &state.db,
        &crate::services::plugin_config::ConfigTarget::problem(id),
        CHECKER_SOURCE_PLUGIN_ID,
        CHECKER_SOURCE_NAMESPACE,
    )
    .await
    {
        Ok(resp) => serde_json::from_value(resp.0.config).ok(),
        Err(AppError::NotFound(_)) => None,
        Err(e) => return Err(e),
    };

    Ok(Json(CheckerSourceResponse { files }))
}

#[utoipa::path(
    delete,
    path = "/",
    tag = "Checker Source",
    operation_id = "deleteCheckerSource",
    summary = "Clear checker source files",
    description = "Removes the checker source files from a problem. Requires `problem:edit` permission.",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 204, description = "Checker source cleared"),
        (status = 401, description = "Unauthorized", body = ErrorBody),
        (status = 403, description = "Forbidden", body = ErrorBody),
        (status = 404, description = "Problem not found", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn delete_checker_source(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<StatusCode, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    find_problem(&state.db, id).await?;
    // Idempotent: absent config is a no-op (404 from the service is fine here).
    match crate::services::plugin_config::delete_config(
        &state.db,
        &crate::services::plugin_config::ConfigTarget::problem(id),
        CHECKER_SOURCE_PLUGIN_ID,
        CHECKER_SOURCE_NAMESPACE,
    )
    .await
    {
        Ok(_) | Err(AppError::NotFound(_)) => {}
        Err(e) => return Err(e),
    }

    Ok(StatusCode::NO_CONTENT)
}

/// The checker-source PUT body is JSON that `AppJson` buffers ENTIRELY in memory
/// before `parse_checker_source` runs, which caps the useful payload at 20
/// files x 1 MiB (~20 MiB). Reusing the 1 GiB STREAMED-upload limit here let a
/// setter OOM the server with a handful of ~1 GiB JSON bodies (full-buffer before
/// reject), a ~50x amplification the streaming upload paths avoid. Size it to the
/// real max payload plus generous JSON/escaping overhead.
const CHECKER_SOURCE_BODY_LIMIT_BYTES: usize = 32 * 1024 * 1024;

pub fn checker_source_body_limit() -> usize {
    CHECKER_SOURCE_BODY_LIMIT_BYTES
}
