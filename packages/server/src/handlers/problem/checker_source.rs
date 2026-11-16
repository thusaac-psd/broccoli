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
use crate::services::plugin_config::{ConfigTarget, delete_config, get_config, store_config};
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
        (status = 409, description = "The plugin providing the problem's checker format declares no checker source (CHECKER_SOURCE_UNSUPPORTED)", body = ErrorBody),
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
    let owner = CheckerSourceOwner::of_problem(&state, id)
        .await?
        .ok_or(AppError::CheckerSourceUnsupported(id))?;

    let config = serde_json::json!({ "files": files });
    plugin_core::config_schema::validate(&owner.schema, &config)
        .map_err(AppError::InvalidConfig)?;
    let _ = store_config(
        &state.db,
        &ConfigTarget::problem(id),
        &owner.plugin_id,
        &owner.namespace,
        config,
        None,
        0,
    )
    .await?;

    Ok(Json(CheckerSourceResponse { files: Some(files) }))
}

/// Where a problem's checker source lives: the config namespace that the
/// plugin providing the problem's checker format declares `checker_source`
/// in its manifest. The host knows the record only through that declaration.
struct CheckerSourceOwner {
    plugin_id: String,
    namespace: String,
    schema: plugin_core::manifest::ConfigNamespace,
}

impl CheckerSourceOwner {
    /// The owner for problem `id`, or `None` when its checker format's plugin
    /// declares no checker source (or is not loaded here). 404 for a missing
    /// problem.
    async fn of_problem(state: &AppState, id: i32) -> Result<Option<Self>, AppError> {
        let problem = find_problem(&state.db, id).await?;
        let Some(plugin_id) = state
            .registries
            .checker_stage_registry
            .read()
            .await
            .get(&problem.checker_format)
            .map(|handlers| handlers.plugin_id.clone())
        else {
            return Ok(None);
        };
        let registry = state
            .plugins
            .get_registry()
            .read()
            .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?;
        Ok(registry.get(&plugin_id).and_then(|entry| {
            entry
                .manifest
                .config
                .iter()
                .find(|(_, ns)| ns.checker_source)
                .map(|(namespace, schema)| Self {
                    plugin_id: plugin_id.clone(),
                    namespace: namespace.clone(),
                    schema: schema.clone(),
                })
        }))
    }
}

/// The files of a stored checker source config (`{ "files": [...] }`).
#[derive(serde::Deserialize)]
struct StoredCheckerSource {
    files: Vec<CheckerSourceFile>,
}

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

    let Some(owner) = CheckerSourceOwner::of_problem(&state, id).await? else {
        return Ok(Json(CheckerSourceResponse { files: None }));
    };
    let files = match get_config(
        &state.db,
        &ConfigTarget::problem(id),
        &owner.plugin_id,
        &owner.namespace,
    )
    .await
    {
        Ok(resp) => serde_json::from_value::<StoredCheckerSource>(resp.0.config)
            .ok()
            .map(|stored| stored.files),
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

    // Idempotent: nothing stored, or no checker source for this problem's
    // checker format, is a no-op.
    let Some(owner) = CheckerSourceOwner::of_problem(&state, id).await? else {
        return Ok(StatusCode::NO_CONTENT);
    };
    match delete_config(
        &state.db,
        &ConfigTarget::problem(id),
        &owner.plugin_id,
        &owner.namespace,
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
