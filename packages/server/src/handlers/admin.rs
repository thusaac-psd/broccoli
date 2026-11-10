use std::io::Read as IoRead;
use std::path::PathBuf;

use axum::extract::DefaultBodyLimit;
use axum::{
    Json,
    extract::{Multipart, State},
    http::StatusCode,
    response::IntoResponse,
};
use broccoli_server_sdk::permissions as perm;
use tracing::instrument;

// visibility-bypass-audited: every handler in this module (list_all_plugins,
// get_plugin_details, enable_plugin, disable_plugin, reload_plugin,
// reload_all_plugins, upload_plugin) requires perm::PLUGIN_MANAGE, pinned by
// tests/integration/plugin.rs's `contestant_cannot_manage_plugins`. This is
// operator tooling over the plugin registry, never a per-row
// Contest/Problem/Submission/Clarification view the kernel governs.
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::path::AppPath;
use crate::models::plugin::{PluginDetailResponse, PluginFullDetailResponse, ReloadAllResponse};
use crate::state::AppState;
use crate::upload_limits::{LARGE_UPLOAD_LIMIT_BYTES, LARGE_UPLOAD_LIMIT_MIB};

#[utoipa::path(
    get,
    path = "/plugins",
    tag = "Admin",
    operation_id = "listAllPlugins",
    summary = "List all discovered plugins",
    description = "Returns a list of all plugins discovered on any server replica (a plugin whose files are not on the serving replica is listed with its id as its name and no manifest details), along with their manifest information, whether each is enabled, and its load state on every live server replica (`fleet`). Requires `plugin:manage` permission.",
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
        .map(|info| PluginDetailResponse::new(info, &fleet))
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
            .map(|id| PluginDetailResponse::without_manifest(id, &fleet)),
    );

    Ok(Json(plugins))
}

#[utoipa::path(
    get,
    path = "/plugins/{id}",
    tag = "Admin",
    operation_id = "getPluginDetails",
    summary = "Get details of a specific plugin",
    description = "Returns detailed information about a specific plugin, including its manifest, whether it is enabled, and its load state (with any load error) on every live server replica (`fleet`). Requires `plugin:manage` permission.",
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
    AppPath(id): AppPath<String>,
) -> Result<Json<PluginFullDetailResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    let plugin = state
        .plugins
        .list_plugins()
        .map_err(AppError::from)?
        .into_iter()
        .find(|p| p.id == id);
    let fleet = crate::plugin_fleet::read(&state.db).await?;
    match plugin {
        Some(plugin) => Ok(Json(PluginFullDetailResponse::new(plugin, &fleet))),
        None if fleet.plugin_ids().any(|known| known == id) => Ok(Json(
            PluginFullDetailResponse::without_manifest(&id, &fleet),
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
    AppPath(id): AppPath<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    crate::plugin_lifecycle::enable(&state, &id).await?;

    Ok(Json(serde_json::json!({
        "message": format!("Plugin '{}' enabled successfully", id)
    })))
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
    AppPath(id): AppPath<String>,
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
    description = "Reloads a plugin by re-reading its manifest from disk and re-creating the WASM runtime. The plugin must be enabled; it need not be loaded on the serving replica, so this is also how to retry a plugin that failed to load. Reloads on the serving replica first; if that succeeds, every other replica reloads from its own plugins directory within a few seconds. Requires `plugin:manage` permission.",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "Plugin reloaded successfully", body = serde_json::Value),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Plugin not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Plugin not enabled (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(id))]
pub async fn reload_plugin(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<String>,
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
    description = "Reloads all currently loaded plugins and discovers any new plugins in the plugins directory, on every server replica: the serving replica reloads first and the others follow within a few seconds, each from its own plugins directory. The response reports the serving replica. Requires `plugin:manage` permission.",
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

pub fn upload_body_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(LARGE_UPLOAD_LIMIT_BYTES)
}

/// Canonical plugin-id validation, shared by the plugin-upload path and the
/// plugin-config handlers so the two can never disagree (they previously did:
/// one enforced an alphanumeric first char, the other a 128-char cap). A plugin
/// id becomes a filesystem path segment, so it is deliberately strict: non-empty,
/// at most 128 chars, starts with an ASCII alphanumeric, and otherwise contains
/// only ASCII alphanumeric, `_`, or `-`.
pub(crate) fn is_valid_plugin_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 {
        return false;
    }
    let first = id.as_bytes()[0];
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    id.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

const MAX_FILE_SIZE: u64 = LARGE_UPLOAD_LIMIT_BYTES as u64;
const MAX_AGGREGATE_SIZE: u64 = 2 * LARGE_UPLOAD_LIMIT_BYTES as u64;

#[utoipa::path(
    post,
    path = "/plugins/upload",
    tag = "Admin",
    operation_id = "uploadPlugin",
    summary = "Upload a plugin archive",
    description = "Uploads a tar.gz plugin archive. Files are merged into the plugins directory (existing files not in the archive are preserved). The plugin is automatically loaded after upload and enabled on every replica; other replicas load it from their own plugins directory, so the directory must be shared (or the archive installed on each node). Requires `plugin:manage` permission.",
    request_body(content_type = "multipart/form-data", content = Vec<u8>, description = "tar.gz archive with a single top-level directory containing plugin.toml"),
    responses(
        (status = 200, description = "Plugin uploaded and loaded successfully", body = serde_json::Value),
        (status = 400, description = "Invalid archive (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, multipart))]
pub async fn upload_plugin(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;

    let mut archive_bytes: Option<Vec<u8>> = None;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::Validation(format!("Multipart error: {}", e)))?
    {
        let name = field.name().unwrap_or("").to_string();
        if name == "plugin" {
            let data = field
                .bytes()
                .await
                .map_err(|e| AppError::Validation(format!("Failed to read upload: {}", e)))?;
            archive_bytes = Some(data.to_vec());
            break;
        }
    }

    let archive_bytes = archive_bytes
        .ok_or_else(|| AppError::Validation("Missing 'plugin' field in multipart form".into()))?;

    let plugins_dir = &state.plugins.get_config().plugins_dir;

    let mut plugin_id: Option<String> = None;
    let mut has_manifest = false;
    let mut validated_entries: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut aggregate_size: u64 = 0;

    let decoder = flate2::read::GzDecoder::new(&archive_bytes[..]);
    let mut archive = tar::Archive::new(decoder);

    for entry_result in archive
        .entries()
        .map_err(|e| AppError::Validation(format!("Failed to read archive entries: {}", e)))?
    {
        let entry =
            entry_result.map_err(|e| AppError::Validation(format!("Invalid tar entry: {}", e)))?;

        let entry_type = entry.header().entry_type();

        if entry_type != tar::EntryType::Regular && entry_type != tar::EntryType::Directory {
            return Err(AppError::Validation(
                "Archive contains unsupported entry types (only regular files and directories allowed)".into(),
            ));
        }

        let raw_path = entry
            .path()
            .map_err(|e| AppError::Validation(format!("Invalid path in archive: {}", e)))?
            .to_path_buf();

        for component in raw_path.components() {
            if let std::path::Component::ParentDir = component {
                return Err(AppError::Validation(
                    "Archive contains path traversal (.. component)".into(),
                ));
            }
            if let std::path::Component::RootDir = component {
                return Err(AppError::Validation(
                    "Archive contains absolute paths".into(),
                ));
            }
        }

        let components: Vec<_> = raw_path.components().collect();
        if components.is_empty() {
            continue;
        }

        let top_level = components[0].as_os_str().to_string_lossy().to_string();

        if let Some(ref existing_id) = plugin_id {
            if top_level != *existing_id {
                return Err(AppError::Validation(
                    "Archive must contain exactly one top-level directory".into(),
                ));
            }
        } else if !is_valid_plugin_id(&top_level) {
            return Err(AppError::Validation(format!(
                "Invalid plugin ID '{}'. Must match [a-zA-Z0-9][a-zA-Z0-9_-]*",
                top_level
            )));
        } else {
            plugin_id = Some(top_level.clone());
        }

        if entry_type == tar::EntryType::Directory {
            continue;
        }

        if components.len() == 2 && components[1].as_os_str() == "plugin.toml" {
            has_manifest = true;
        }

        let mut limited_reader = entry.take(MAX_FILE_SIZE + 1);
        let mut contents = Vec::new();
        limited_reader
            .read_to_end(&mut contents)
            .map_err(|e| AppError::Validation(format!("Failed to read archive entry: {}", e)))?;

        if contents.len() as u64 > MAX_FILE_SIZE {
            return Err(AppError::Validation(format!(
                "File '{}' exceeds maximum size of {}MB",
                raw_path.display(),
                LARGE_UPLOAD_LIMIT_MIB
            )));
        }

        aggregate_size += contents.len() as u64;
        if aggregate_size > MAX_AGGREGATE_SIZE {
            return Err(AppError::Validation(format!(
                "Total extracted size exceeds maximum of {}MB",
                MAX_AGGREGATE_SIZE / (1024 * 1024)
            )));
        }

        validated_entries.push((raw_path, contents));
    }

    let plugin_id = plugin_id.ok_or_else(|| {
        AppError::Validation("Archive is empty or has no top-level directory".into())
    })?;

    if !has_manifest {
        return Err(AppError::Validation(format!(
            "Archive does not contain {}/plugin.toml",
            plugin_id
        )));
    }

    let dest_root = plugins_dir.join(&plugin_id);
    let dest_root_clone = dest_root.clone();
    tokio::task::spawn_blocking(move || -> Result<(), AppError> {
        for (path, contents) in &validated_entries {
            let relative = path.iter().skip(1).collect::<PathBuf>();
            let dest = dest_root_clone.join(&relative);

            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    AppError::Internal(format!(
                        "Failed to create directory '{}': {}",
                        parent.display(),
                        e
                    ))
                })?;

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let dir_perms = std::fs::Permissions::from_mode(0o755);
                    let mut dir = parent.to_path_buf();
                    while dir.starts_with(&dest_root_clone) {
                        if let Err(e) = std::fs::set_permissions(&dir, dir_perms.clone()) {
                            tracing::warn!(path = %dir.display(), error = %e, "Failed to set plugin directory permissions during unpack");
                        }
                        if !dir.pop() {
                            break;
                        }
                    }
                }
            }

            std::fs::write(&dest, contents).map_err(|e| {
                AppError::Internal(format!("Failed to write '{}': {}", dest.display(), e))
            })?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let perms = std::fs::Permissions::from_mode(0o644);
                if let Err(e) = std::fs::set_permissions(&dest, perms) {
                    tracing::warn!(path = %dest.display(), error = %e, "Failed to set plugin file permissions during unpack");
                }
            }
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o755);
            if let Err(e) = std::fs::set_permissions(&dest_root_clone, perms) {
                tracing::warn!(path = %dest_root_clone.display(), error = %e, "Failed to set plugin root directory permissions during unpack");
            }
        }

        Ok(())
    })
    .await
    .map_err(|e| AppError::Internal(format!("Extraction task panicked: {}", e)))??;

    crate::plugin_lifecycle::install(&state, &plugin_id)
        .await
        .map_err(|e| match e {
            AppError::Internal(detail) => {
                AppError::Internal(format!("Failed to activate plugin after upload: {detail}"))
            }
            other => other,
        })?;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "plugin_id": plugin_id,
            "message": format!("Plugin '{}' uploaded and loaded successfully", plugin_id)
        })),
    ))
}
