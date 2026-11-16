use crate::extractors::query::AppQuery;
use axum::{
    Json,
    extract::{Multipart, State},
};
use broccoli_api_types::plugin::PluginReplaces;
use broccoli_server_sdk::permissions as perm;
use sea_orm::EntityTrait;
use tracing::instrument;

// visibility-bypass-audited: every handler in this module (list_all_plugins,
// get_plugin_details, get_plugin_dependents, enable_plugin, disable_plugin, reload_plugin,
// reload_all_plugins, get_plugin_upload_policy, inspect_plugin_archive,
// upload_plugin, remove_plugin_upload) requires perm::PLUGIN_MANAGE, pinned by
// tests/integration/plugin.rs's `contestant_cannot_manage_plugins`. This is
// operator tooling over the plugin registry, never a per-row
// Contest/Problem/Submission/Clarification view the kernel governs.
use crate::entity::plugin as plugin_entity;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::path::AppPath;
use crate::models::plugin::{
    PluginArchiveForm, PluginArchiveSummary, PluginDependentsResponse, PluginDetailResponse,
    PluginFullDetailResponse, PluginUploadPolicy, PluginUploadResponse, PublishedPluginResponse,
    ReloadAllResponse, RemoveUploadQuery, RemoveUploadResponse, RunningContestRef,
};
use crate::plugin_archive::{MAX_ARCHIVE_BYTES, PluginArchive};
use crate::plugin_distribution::{PluginSource, bundled_root};
use crate::plugin_id::PluginId;
use crate::plugin_lifecycle::UploadRemoval;
use crate::services::plugin_config::incompatible_configs;
use crate::state::AppState;

/// Whether this replica's bundle has a copy of the plugin `id`.
fn is_bundled(state: &AppState, id: &str) -> bool {
    PluginId::parse(id).is_ok_and(|id| bundled_root(state, &id).is_some())
}

#[utoipa::path(
    get,
    path = "/plugins",
    tag = "Admin",
    operation_id = "listAllPlugins",
    summary = "List all discovered plugins",
    description = "Returns a list of all plugins known to the fleet (a plugin whose files are not on the serving replica is listed with its id as its name and no manifest details), along with their manifest information, whether each is enabled, where its files come from (`source`: bundled with each node, or an uploaded archive), and its load state on every live server replica (`fleet`). Requires `plugin:manage` permission.",
    responses(
        (status = 200, description = "List of plugins retrieved successfully", body = Vec<PluginDetailResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn list_all_plugins(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<Vec<PluginDetailResponse>>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    let fleet = crate::plugin_fleet::read(&state.db).await?;
    let mut plugins: Vec<PluginDetailResponse> = state
        .plugins
        .list_plugins()
        .map_err(AppError::from)?
        .into_iter()
        .map(|info| {
            let bundled = is_bundled(&state, &info.id);
            PluginDetailResponse::new(info, &fleet, bundled)
        })
        .collect();
    // Plugins installed on other replicas but not on this one: still listed,
    // so the page shows the same set whichever replica serves it.
    let local: std::collections::HashSet<String> = plugins.iter().map(|p| p.id.clone()).collect();
    let mut remote: Vec<&str> = fleet
        .plugin_ids()
        .filter(|id| !local.contains(*id))
        .collect();
    remote.sort_unstable();
    plugins.extend(
        remote
            .into_iter()
            .map(|id| PluginDetailResponse::without_manifest(id, &fleet, is_bundled(&state, id))),
    );

    Ok(Json(plugins))
}

#[utoipa::path(
    get,
    path = "/plugins/{id}",
    tag = "Admin",
    operation_id = "getPluginDetails",
    summary = "Get details of a specific plugin",
    description = "Returns detailed information about a specific plugin, including its manifest, whether it is enabled, where its files come from (`source`), and its load state (with any load error) on every live server replica (`fleet`). Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "Plugin details retrieved successfully", body = PluginFullDetailResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Plugin not found (NOT_FOUND)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_plugin_details(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<PluginId>,
) -> Result<Json<PluginFullDetailResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    let plugin = state
        .plugins
        .list_plugins()
        .map_err(AppError::from)?
        .into_iter()
        .find(|p| p.id == id.as_str());
    let fleet = crate::plugin_fleet::read(&state.db).await?;
    let bundled = bundled_root(&state, &id).is_some();
    match plugin {
        Some(plugin) => Ok(Json(PluginFullDetailResponse::new(plugin, &fleet, bundled))),
        None if fleet.plugin_ids().any(|known| known == id.as_str()) => Ok(Json(
            PluginFullDetailResponse::without_manifest(&id, &fleet, bundled),
        )),
        None => Err(AppError::NotFound(format!("Plugin '{}' not found", id))),
    }
}

#[utoipa::path(
    post,
    path = "/plugins/{id}/enable",
    tag = "Admin",
    operation_id = "enablePlugin",
    summary = "Enable a plugin",
    description = "Enables a plugin by its ID on every server replica. Loads it on the serving replica first and records it as enabled only if that succeeds; the other replicas follow within a few seconds. Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "Plugin enabled successfully", body = serde_json::Value),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Plugin not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Plugin already enabled (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn enable_plugin(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<PluginId>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    crate::plugin_lifecycle::enable(&state, &id).await?;

    Ok(Json(serde_json::json!({
        "message": format!("Plugin '{}' enabled successfully", id)
    })))
}

#[utoipa::path(
    get,
    path = "/plugins/{id}/dependents",
    tag = "Admin",
    operation_id = "getPluginDependents",
    summary = "List what depends on a plugin right now",
    description = "Returns the contests running now (started, not yet ended, not deleted) whose contest type the plugin provides on the serving replica, so an admin can see what disabling it would stop. A plugin that is not loaded on the serving replica provides no contest types here. Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "What depends on the plugin", body = PluginDependentsResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn get_plugin_dependents(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<PluginId>,
) -> Result<Json<PluginDependentsResponse>, AppError> {
    use sea_orm::{DbBackend, FromQueryResult, Statement};

    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    let contest_types: Vec<String> = state
        .registries
        .contest_type_registry
        .read()
        .await
        .iter()
        .filter(|(_, handlers)| handlers.plugin_id == id.as_str())
        .map(|(contest_type, _)| contest_type.clone())
        .collect();
    if contest_types.is_empty() {
        return Ok(Json(PluginDependentsResponse {
            running_contests: Vec::new(),
        }));
    }
    // "Running" is judged by the database clock, the one every replica shares.
    let running_contests = RunningContestRef::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"SELECT id, title, contest_type, end_time
             FROM contest
            WHERE contest_type = ANY($1)
              AND deleted_at IS NULL
              AND start_time <= NOW() AND end_time > NOW()
            ORDER BY end_time, id"#,
        [contest_types.into()],
    ))
    .all(&state.db)
    .await?;
    Ok(Json(PluginDependentsResponse { running_contests }))
}

#[utoipa::path(
    post,
    path = "/plugins/{id}/disable",
    tag = "Admin",
    operation_id = "disablePlugin",
    summary = "Disable a plugin",
    description = "Disables a plugin by its ID on every server replica and deletes its pending timers. The other replicas follow within a few seconds. Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "Plugin disabled successfully", body = serde_json::Value),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Plugin not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Plugin already disabled (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn disable_plugin(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<PluginId>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    crate::plugin_lifecycle::disable(&state, &id).await?;

    Ok(Json(serde_json::json!({
        "message": format!("Plugin '{}' disabled successfully", id)
    })))
}

#[utoipa::path(
    post,
    path = "/plugins/{id}/reload",
    tag = "Admin",
    operation_id = "reloadPlugin",
    summary = "Reload a plugin",
    description = "Reloads a plugin from its published source (each node's bundled copy, or the uploaded archive) and re-creates the WASM runtime. The plugin must be enabled; it need not be loaded on the serving replica, so this is also how to retry a plugin that failed to load. Reloads on the serving replica first; if that succeeds, every other replica reloads within a few seconds. Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "Plugin reloaded successfully", body = serde_json::Value),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Plugin not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Plugin not enabled, or changed through another replica meanwhile (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn reload_plugin(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<PluginId>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    crate::plugin_lifecycle::reload(&state, &id).await?;

    Ok(Json(serde_json::json!({
        "message": format!("Plugin '{}' reloaded successfully", id)
    })))
}

#[utoipa::path(
    post,
    path = "/plugins/reload-all",
    tag = "Admin",
    operation_id = "reloadAllPlugins",
    summary = "Reload all plugins and discover new ones",
    description = "Reloads every enabled plugin from its published source and discovers any new plugins in the bundled plugins directory, on every server replica: the serving replica reloads first and the others follow within a few seconds. The response reports the serving replica. Requires `plugin:manage` permission.",
    responses(
        (status = 200, description = "Reload results", body = ReloadAllResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn reload_all_plugins(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
) -> Result<Json<ReloadAllResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    Ok(Json(crate::plugin_lifecycle::reload_all(&state).await?))
}

/// Room for the multipart framing around the archive.
const MULTIPART_OVERHEAD_BYTES: usize = 1024 * 1024;

pub fn upload_body_limit() -> usize {
    MAX_ARCHIVE_BYTES + MULTIPART_OVERHEAD_BYTES
}

/// Read and validate the `plugin` field of an upload form.
/// The fields of a plugin upload form.
struct ArchiveForm {
    archive: PluginArchive,
    /// The `replaces` field, if sent.
    replaces: Option<String>,
}

/// `body_len` is the request's `Content-Length`, if sent: the archive buffer
/// is sized from it once instead of growing (and copying) as chunks arrive.
async fn read_archive(
    state: &AppState,
    body_len: Option<usize>,
    mut multipart: Multipart,
) -> Result<ArchiveForm, AppError> {
    if !state.config.plugin.uploads_enabled {
        return Err(AppError::PluginUploadDisabled);
    }
    let too_large = || {
        AppError::PayloadTooLarge(format!(
            "Plugin archives are limited to {} MiB",
            MAX_ARCHIVE_BYTES / (1024 * 1024)
        ))
    };
    let unreadable = |e: axum::extract::multipart::MultipartError| {
        if e.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
            too_large()
        } else {
            AppError::Validation(format!("Failed to read the upload: {e}"))
        }
    };
    let (mut bytes, mut replaces) = (None, None);
    while let Some(mut field) = multipart.next_field().await.map_err(unreadable)? {
        match field.name() {
            Some("plugin") => {
                // Chunk by chunk, so an oversized archive is refused as soon
                // as it crosses the limit rather than read to the end.
                let mut buf = Vec::with_capacity(body_len.unwrap_or(0).min(MAX_ARCHIVE_BYTES));
                while let Some(chunk) = field.chunk().await.map_err(unreadable)? {
                    if buf.len() + chunk.len() > MAX_ARCHIVE_BYTES {
                        return Err(too_large());
                    }
                    buf.extend_from_slice(&chunk);
                }
                bytes = Some(buf);
            }
            Some("replaces") => replaces = Some(field.text().await.map_err(unreadable)?),
            _ => {}
        }
    }
    let bytes = bytes
        .ok_or_else(|| AppError::Validation("Missing 'plugin' field in multipart form".into()))?;
    // Decompressing and checking up to the unpacked limit is CPU work.
    let archive = tokio::task::spawn_blocking(move || PluginArchive::parse(bytes))
        .await
        .map_err(|e| AppError::Internal(format!("Archive check task failed: {e}")))?
        .map_err(|e| AppError::Validation(e.to_string()))?;
    Ok(ArchiveForm { archive, replaces })
}

/// The `replaces` field, as inspecting the archive reported the current
/// source (see [`PluginReplaces`]).
fn parse_replaces(value: Option<&str>) -> Result<Option<PluginSource>, AppError> {
    let missing = || {
        AppError::Validation(
            "The 'replaces' field must be 'new', 'bundle' or the archive hash being replaced, \
             as inspecting the archive reported it"
                .into(),
        )
    };
    match PluginReplaces::parse(value.ok_or_else(missing)?) {
        PluginReplaces::New => Ok(None),
        PluginReplaces::Bundle => Ok(Some(PluginSource::Bundle)),
        PluginReplaces::Upload(hash) => {
            let hash = common::storage::ContentHash::from_hex(&hash).map_err(|_| missing())?;
            Ok(Some(PluginSource::Upload(hash.to_hex())))
        }
    }
}

fn content_length(headers: &axum::http::HeaderMap) -> Option<usize> {
    headers
        .get(axum::http::header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

/// One archive at a time per server: each holds up to [`MAX_ARCHIVE_BYTES`]
/// in memory while it is read and checked, so uploads and inspections queue
/// here instead of adding up.
static ARCHIVE_INTAKE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

async fn archive_intake() -> Result<tokio::sync::SemaphorePermit<'static>, AppError> {
    ARCHIVE_INTAKE
        .acquire()
        .await
        .map_err(|e| AppError::Internal(format!("archive intake closed: {e}")))
}

#[utoipa::path(
    get,
    path = "/plugins/upload",
    tag = "Admin",
    operation_id = "getPluginUploadPolicy",
    summary = "Whether plugin uploads are accepted",
    description = "Reports whether this server accepts plugin archive uploads (operators can switch them off with `plugin.uploads_enabled`) and the largest archive accepted. Requires `plugin:manage` permission.",
    responses(
        (status = 200, description = "Upload policy", body = PluginUploadPolicy),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn get_plugin_upload_policy(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<PluginUploadPolicy>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    Ok(Json(PluginUploadPolicy {
        enabled: state.config.plugin.uploads_enabled,
        max_archive_bytes: MAX_ARCHIVE_BYTES as u64,
    }))
}

#[utoipa::path(
    post,
    path = "/plugins/upload/inspect",
    tag = "Admin",
    operation_id = "inspectPluginArchive",
    summary = "Check a plugin archive without publishing it",
    description = "Validates a tar.gz plugin archive exactly as an upload would and reports what it would publish: the plugin id, version, the host permissions it requests, what the fleet runs under that id now, and the stored configs of the plugin that the archive's config schemas would refuse. Publishes nothing. Requires `plugin:manage` permission.",
    request_body(content_type = "multipart/form-data", content = PluginArchiveForm),
    responses(
        (status = 200, description = "The archive is valid", body = PluginArchiveSummary),
        (status = 400, description = "Invalid archive (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED, PLUGIN_UPLOAD_DISABLED)", body = ErrorBody),
        (status = 413, description = "Archive too large (PAYLOAD_TOO_LARGE)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, headers, multipart))]
pub async fn inspect_plugin_archive(
    auth_user: AuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    multipart: Multipart,
) -> Result<Json<PluginArchiveSummary>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    let _intake = archive_intake().await?;
    let archive = read_archive(&state, content_length(&headers), multipart)
        .await?
        .archive;
    let current = plugin_entity::Entity::find_by_id(archive.id().as_str())
        .one(&state.db)
        .await?
        .map(|row| PublishedPluginResponse {
            source: PluginSource::of(row.archive_hash.as_deref()).kind(),
            archive_hash: row.archive_hash,
            version: row.version,
            enabled: row.is_enabled,
        });
    let manifest = archive.manifest();
    let incompatible_configs =
        incompatible_configs(&state.db, archive.id().as_str(), manifest).await?;
    Ok(Json(PluginArchiveSummary {
        id: archive.id().to_string(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        description: manifest.description.clone(),
        permissions: manifest
            .server
            .as_ref()
            .map(|s| s.permissions.clone())
            .unwrap_or_default(),
        sql_read: manifest
            .server
            .as_ref()
            .map(crate::models::plugin::sql_read)
            .unwrap_or_default(),
        has_server: manifest.has_server(),
        has_web: manifest.has_web(),
        archive_hash: archive.hash().to_hex(),
        size_bytes: archive.bytes().len() as u64,
        current,
        incompatible_configs,
    }))
}

#[utoipa::path(
    post,
    path = "/plugins/upload",
    tag = "Admin",
    operation_id = "uploadPlugin",
    summary = "Upload and publish a plugin archive",
    description = "Publishes a tar.gz plugin archive to every server replica. The archive is validated, stored in the shared blob store under its content hash, and loaded on the serving replica; only if that load succeeds is it recorded as the plugin's source and enabled, and every other replica then fetches it, verifies its hash and loads it within a few seconds. The archive replaces the plugin's files entirely, and takes precedence over a copy bundled with the nodes until the upload is removed. The form's `replaces` field states what the upload replaces, as inspecting the archive reported it, so an upload never silently replaces a change made meanwhile. A replica that cannot load it keeps serving the version it had. Requires `plugin:manage` permission.",
    request_body(content_type = "multipart/form-data", content = PluginArchiveForm),
    responses(
        (status = 200, description = "Plugin published and loaded on the serving replica", body = PluginUploadResponse),
        (status = 400, description = "Invalid archive (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED, PLUGIN_UPLOAD_DISABLED)", body = ErrorBody),
        (status = 413, description = "Archive too large (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 409, description = "What the archive replaces (`replaces`) is no longer what is published: another change landed since the archive was inspected (CONFLICT); or `replaces` is `new` but a plugin with this id exists (PLUGIN_EXISTS)", body = ErrorBody),
        (status = 422, description = "The plugin failed to load on the serving replica; nothing was published (PLUGIN_LOAD_FAILED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, headers, multipart))]
pub async fn upload_plugin(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    multipart: Multipart,
) -> Result<Json<PluginUploadResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    let _intake = archive_intake().await?;
    let ArchiveForm { archive, replaces } =
        read_archive(&state, content_length(&headers), multipart).await?;
    let replaces = parse_replaces(replaces.as_deref())?;
    let plugin_id = archive.id().to_string();
    let version = archive.manifest().version.clone();
    let archive_hash = archive.hash().to_hex();
    let manifest = archive.manifest().clone();
    crate::plugin_lifecycle::install(&state, archive, replaces).await?;
    let incompatible_configs = incompatible_configs(&state.db, &plugin_id, &manifest).await?;
    Ok(Json(PluginUploadResponse {
        message: format!("Plugin '{plugin_id}' v{version} uploaded and published"),
        plugin_id,
        version,
        archive_hash,
        incompatible_configs,
    }))
}

#[utoipa::path(
    delete,
    path = "/plugins/{id}/upload",
    tag = "Admin",
    operation_id = "removePluginUpload",
    summary = "Remove a plugin's uploaded version",
    description = "Removes the uploaded archive a plugin runs from, on every server replica. If the serving replica's bundle has a copy of the plugin, that copy becomes the source again (loaded on the serving replica first when the plugin is enabled); otherwise the plugin is removed from the fleet and its pending timers are deleted. `expect` names the outcome the admin confirmed; if this replica would do the other, nothing changes and the call answers 409. Allowed even when uploads are switched off. Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID"), RemoveUploadQuery),
    responses(
        (status = 200, description = "Upload removed", body = RemoveUploadResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Plugin not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "The plugin has no uploaded version, it changed through another replica meanwhile, or this replica would do something other than `expect` (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn remove_plugin_upload(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<PluginId>,
    AppQuery(query): AppQuery<RemoveUploadQuery>,
) -> Result<Json<RemoveUploadResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    let outcome = crate::plugin_lifecycle::remove_upload(&state, &id, query.expect).await?;
    let message = match outcome {
        UploadRemoval::BundleRestored => {
            format!("Removed the upload; plugin '{id}' runs its bundled version again")
        }
        UploadRemoval::Removed => format!("Removed plugin '{id}'"),
    };
    Ok(Json(RemoveUploadResponse { outcome, message }))
}
