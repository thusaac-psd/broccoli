use axum::{Json, extract::State, response::IntoResponse};
use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};
use tracing::instrument;

use std::collections::HashSet;

use plugin_core::registry::PluginStatus;
use plugin_core::traits::PluginManager;

// visibility-bypass-audited: the global config endpoints require
// perm::PLUGIN_MANAGE (pinned by
// tests/integration/plugin_config.rs::permission_denied_for_non_admin), the
// per-problem endpoints require perm::PROBLEM_EDIT (pinned by
// `user_without_permission_cannot_access_problem_config`), and the
// per-contest/contest-problem endpoints require perm::CONTEST_MANAGE (pinned
// by `user_without_permission_cannot_access_contest_config`). This is
// operator/setter configuration storage, never contestant-facing content.
use crate::entity::plugin_config;
use crate::error::AppError;
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::json::AppJson;
use crate::extractors::path::AppPath;
use crate::host_funcs::config::{extract_plugin_id, strip_namespace_prefix};
use crate::models::plugin_config::{PluginConfigResponse, UpsertPluginConfigRequest};
use crate::services::plugin_config::{
    ConfigScope, ConfigTarget, delete_config, get_config, upsert_config,
};
use crate::state::AppState;
use crate::utils::contest::{find_contest, find_contest_problem};
use crate::utils::problem::find_problem;

fn validate_namespace(ns: &str) -> Result<(), AppError> {
    if ns.is_empty() || ns.len() > 128 {
        return Err(AppError::Validation(
            "Namespace must be 1-128 characters".into(),
        ));
    }
    if !ns
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(AppError::Validation(
            "Namespace must contain only alphanumeric, hyphen, or underscore characters".into(),
        ));
    }
    Ok(())
}

struct AvailableSchema {
    plugin_id: String,
    namespace: String,
    description: Option<String>,
    json_schema: serde_json::Value,
}

fn collect_schemas_for_scope(
    plugins: &dyn PluginManager,
    scope: ConfigScope,
) -> Vec<AvailableSchema> {
    let scope_name = scope.as_str();
    let plugin_list = match plugins.list_plugins() {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!(error = %e, scope = scope_name, "Failed to list plugins for config schema collection");
            return vec![];
        }
    };

    let mut schemas = Vec::new();
    for plugin in &plugin_list {
        if plugin.status != PluginStatus::Loaded {
            continue;
        }
        for (ns_name, ns_config) in &plugin.manifest.config {
            if ns_config.scopes.contains(&scope_name.to_string()) {
                schemas.push(AvailableSchema {
                    plugin_id: plugin.id.clone(),
                    namespace: ns_name.clone(),
                    description: ns_config.description.clone(),
                    json_schema: ns_config.to_json_schema(),
                });
            }
        }
    }
    schemas
}

async fn list_config_inner<C: ConnectionTrait>(
    db: &C,
    target: &ConfigTarget,
    available_schemas: &[AvailableSchema],
) -> Result<Json<Vec<PluginConfigResponse>>, AppError> {
    let rows = plugin_config::Entity::find()
        .filter(plugin_config::Column::Scope.eq(target.scope().as_str()))
        .filter(plugin_config::Column::RefId.eq(target.ref_id()))
        .all(db)
        .await?;

    let mut seen_keys: HashSet<(String, String)> = HashSet::new();

    let mut response: Vec<PluginConfigResponse> = rows
        .into_iter()
        .map(|r| {
            let (plugin_id, namespace) = if target.scope() == ConfigScope::Plugin {
                (target.ref_id().to_string(), r.namespace.clone())
            } else {
                (
                    extract_plugin_id(&r.namespace).to_string(),
                    strip_namespace_prefix(&r.namespace).to_string(),
                )
            };

            let matching_schema = available_schemas
                .iter()
                .find(|s| s.plugin_id == plugin_id && s.namespace == namespace);

            seen_keys.insert((plugin_id.clone(), namespace.clone()));

            PluginConfigResponse {
                plugin_id,
                namespace,
                config: r.config,
                enabled: r.enabled,
                position: r.position,
                updated_at: Some(r.updated_at),
                json_schema: matching_schema.map(|s| s.json_schema.clone()),
                description: matching_schema.and_then(|s| s.description.clone()),
            }
        })
        .collect();

    for schema in available_schemas {
        let key = (schema.plugin_id.clone(), schema.namespace.clone());
        if !seen_keys.contains(&key) {
            response.push(PluginConfigResponse {
                plugin_id: schema.plugin_id.clone(),
                namespace: schema.namespace.clone(),
                config: serde_json::Value::Null,
                enabled: None,
                position: 0,
                updated_at: None,
                json_schema: Some(schema.json_schema.clone()),
                description: schema.description.clone(),
            });
        }
    }

    Ok(Json(response))
}

fn validate_plugin_id(id: &str) -> Result<(), AppError> {
    // Delegate to the single canonical validator so the upload path and the
    // config handlers enforce exactly the same rule.
    if crate::handlers::admin::is_valid_plugin_id(id) {
        Ok(())
    } else {
        Err(AppError::Validation(
            "Plugin ID must be 1-128 characters, start with a letter or digit, and contain only \
             alphanumeric, hyphen, or underscore characters"
                .into(),
        ))
    }
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Plugin Config",
    operation_id = "listPluginGlobalConfig",
    summary = "List all config namespaces for a plugin",
    params(("id" = String, Path, description = "Plugin ID")),
    responses(
        (status = 200, description = "Config list", body = Vec<PluginConfigResponse>),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(plugin_id))]
pub async fn list_plugin_global_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(plugin_id): AppPath<String>,
) -> Result<Json<Vec<PluginConfigResponse>>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    let target = ConfigTarget::plugin(&plugin_id);
    list_config_inner(&state.db, &target, &[]).await
}

#[utoipa::path(
    get,
    path = "/{namespace}",
    tag = "Plugin Config",
    operation_id = "getPluginGlobalConfig",
    summary = "Get config for a specific namespace on a plugin",
    params(
        ("id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 200, description = "Config found", body = PluginConfigResponse),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(plugin_id, namespace))]
pub async fn get_plugin_global_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((plugin_id, namespace)): AppPath<(String, String)>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let target = ConfigTarget::plugin(&plugin_id);
    get_config(&state.db, &target, &plugin_id, &namespace).await
}

#[utoipa::path(
    put,
    path = "/{namespace}",
    tag = "Plugin Config",
    operation_id = "upsertPluginGlobalConfig",
    summary = "Create or update config for a namespace on a plugin",
    params(
        ("id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    request_body = UpsertPluginConfigRequest,
    responses(
        (status = 200, description = "Config upserted", body = PluginConfigResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, payload), fields(plugin_id, namespace))]
pub async fn upsert_plugin_global_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((plugin_id, namespace)): AppPath<(String, String)>,
    AppJson(payload): AppJson<UpsertPluginConfigRequest>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let target = ConfigTarget::plugin(&plugin_id);
    upsert_config(
        &state.db,
        &target,
        &plugin_id,
        &namespace,
        payload.config,
        payload.enabled,
        payload.position,
    )
    .await
}

#[utoipa::path(
    delete,
    path = "/{namespace}",
    tag = "Plugin Config",
    operation_id = "deletePluginGlobalConfig",
    summary = "Delete config for a namespace on a plugin",
    params(
        ("id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 204, description = "Config deleted"),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(plugin_id, namespace))]
pub async fn delete_plugin_global_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((plugin_id, namespace)): AppPath<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PLUGIN_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let target = ConfigTarget::plugin(&plugin_id);
    delete_config(&state.db, &target, &plugin_id, &namespace).await
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Plugin Config",
    operation_id = "listProblemConfig",
    summary = "List all config namespaces for a problem",
    params(("id" = i32, Path, description = "Problem ID")),
    responses(
        (status = 200, description = "Config list", body = Vec<PluginConfigResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id))]
pub async fn list_problem_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
) -> Result<Json<Vec<PluginConfigResponse>>, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;
    find_problem(&state.db, problem_id).await?;
    let target = ConfigTarget::problem(problem_id);
    let schemas = collect_schemas_for_scope(&*state.plugins, ConfigScope::Problem);
    list_config_inner(&state.db, &target, &schemas).await
}

#[utoipa::path(
    get,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "getProblemConfig",
    summary = "Get config for a specific namespace on a problem",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 200, description = "Config found", body = PluginConfigResponse),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id, plugin_id, namespace))]
pub async fn get_problem_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, plugin_id, namespace)): AppPath<(i32, String, String)>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    find_problem(&state.db, problem_id).await?;
    let target = ConfigTarget::problem(problem_id);
    get_config(&state.db, &target, &plugin_id, &namespace).await
}

#[utoipa::path(
    put,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "upsertProblemConfig",
    summary = "Create or update config for a namespace on a problem",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    request_body = UpsertPluginConfigRequest,
    responses(
        (status = 200, description = "Config upserted", body = PluginConfigResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(
    skip(state, auth_user, payload),
    fields(problem_id, plugin_id, namespace)
)]
pub async fn upsert_problem_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, plugin_id, namespace)): AppPath<(i32, String, String)>,
    AppJson(payload): AppJson<UpsertPluginConfigRequest>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let txn = state.db.begin().await?;
    find_problem(&txn, problem_id).await?;
    let target = ConfigTarget::problem(problem_id);
    let result = upsert_config(
        &txn,
        &target,
        &plugin_id,
        &namespace,
        payload.config,
        payload.enabled,
        payload.position,
    )
    .await?;
    txn.commit().await?;
    Ok(result)
}

#[utoipa::path(
    delete,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "deleteProblemConfig",
    summary = "Delete config for a namespace on a problem",
    params(
        ("id" = i32, Path, description = "Problem ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 204, description = "Config deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(problem_id, plugin_id, namespace))]
pub async fn delete_problem_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((problem_id, plugin_id, namespace)): AppPath<(i32, String, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let txn = state.db.begin().await?;
    find_problem(&txn, problem_id).await?;
    let target = ConfigTarget::problem(problem_id);
    let result = delete_config(&txn, &target, &plugin_id, &namespace).await?;
    txn.commit().await?;
    Ok(result)
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Plugin Config",
    operation_id = "listContestProblemConfig",
    summary = "List all config namespaces for a contest-problem",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
    ),
    responses(
        (status = 200, description = "Config list", body = Vec<PluginConfigResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Contest problem not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, problem_id))]
pub async fn list_contest_problem_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id)): AppPath<(i32, i32)>,
) -> Result<Json<Vec<PluginConfigResponse>>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    find_contest_problem(&state.db, contest_id, problem_id).await?;
    let target = ConfigTarget::contest_problem(contest_id, problem_id);
    let schemas = collect_schemas_for_scope(&*state.plugins, ConfigScope::ContestProblem);
    list_config_inner(&state.db, &target, &schemas).await
}

#[utoipa::path(
    get,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "getContestProblemConfig",
    summary = "Get config for a specific namespace on a contest-problem",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 200, description = "Config found", body = PluginConfigResponse),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Contest problem not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(
    skip(state, auth_user),
    fields(contest_id, problem_id, plugin_id, namespace)
)]
pub async fn get_contest_problem_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id, plugin_id, namespace)): AppPath<(i32, i32, String, String)>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    find_contest_problem(&state.db, contest_id, problem_id).await?;
    let target = ConfigTarget::contest_problem(contest_id, problem_id);
    get_config(&state.db, &target, &plugin_id, &namespace).await
}

#[utoipa::path(
    put,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "upsertContestProblemConfig",
    summary = "Create or update config for a namespace on a contest-problem",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    request_body = UpsertPluginConfigRequest,
    responses(
        (status = 200, description = "Config upserted", body = PluginConfigResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Contest problem not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(
    skip(state, auth_user, payload),
    fields(contest_id, problem_id, plugin_id, namespace)
)]
pub async fn upsert_contest_problem_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id, plugin_id, namespace)): AppPath<(i32, i32, String, String)>,
    AppJson(payload): AppJson<UpsertPluginConfigRequest>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let txn = state.db.begin().await?;
    find_contest_problem(&txn, contest_id, problem_id).await?;
    let target = ConfigTarget::contest_problem(contest_id, problem_id);
    let result = upsert_config(
        &txn,
        &target,
        &plugin_id,
        &namespace,
        payload.config,
        payload.enabled,
        payload.position,
    )
    .await?;
    txn.commit().await?;
    Ok(result)
}

#[utoipa::path(
    delete,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "deleteContestProblemConfig",
    summary = "Delete config for a namespace on a contest-problem",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("problem_id" = i32, Path, description = "Problem ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 204, description = "Config deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(
    skip(state, auth_user),
    fields(contest_id, problem_id, plugin_id, namespace)
)]
pub async fn delete_contest_problem_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, problem_id, plugin_id, namespace)): AppPath<(i32, i32, String, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let txn = state.db.begin().await?;
    find_contest_problem(&txn, contest_id, problem_id).await?;
    let target = ConfigTarget::contest_problem(contest_id, problem_id);
    let result = delete_config(&txn, &target, &plugin_id, &namespace).await?;
    txn.commit().await?;
    Ok(result)
}

#[utoipa::path(
    get,
    path = "/",
    tag = "Plugin Config",
    operation_id = "listContestConfig",
    summary = "List all config namespaces for a contest",
    params(("id" = i32, Path, description = "Contest ID")),
    responses(
        (status = 200, description = "Config list", body = Vec<PluginConfigResponse>),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id))]
pub async fn list_contest_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
) -> Result<Json<Vec<PluginConfigResponse>>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    find_contest(&state.db, contest_id).await?;
    let target = ConfigTarget::contest(contest_id);
    let schemas = collect_schemas_for_scope(&*state.plugins, ConfigScope::Contest);
    list_config_inner(&state.db, &target, &schemas).await
}

#[utoipa::path(
    get,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "getContestConfig",
    summary = "Get config for a specific namespace on a contest",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 200, description = "Config found", body = PluginConfigResponse),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, plugin_id, namespace))]
pub async fn get_contest_config(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, plugin_id, namespace)): AppPath<(i32, String, String)>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    find_contest(&state.db, contest_id).await?;
    let target = ConfigTarget::contest(contest_id);
    get_config(&state.db, &target, &plugin_id, &namespace).await
}

#[utoipa::path(
    put,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "upsertContestConfig",
    summary = "Create or update config for a namespace on a contest",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    request_body = UpsertPluginConfigRequest,
    responses(
        (status = 200, description = "Config upserted", body = PluginConfigResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Contest not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(
    skip(state, auth_user, payload),
    fields(contest_id, plugin_id, namespace)
)]
pub async fn upsert_contest_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, plugin_id, namespace)): AppPath<(i32, String, String)>,
    AppJson(payload): AppJson<UpsertPluginConfigRequest>,
) -> Result<Json<PluginConfigResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let txn = state.db.begin().await?;
    find_contest(&txn, contest_id).await?;
    let target = ConfigTarget::contest(contest_id);
    let result = upsert_config(
        &txn,
        &target,
        &plugin_id,
        &namespace,
        payload.config,
        payload.enabled,
        payload.position,
    )
    .await?;
    txn.commit().await?;
    Ok(result)
}

#[utoipa::path(
    delete,
    path = "/{plugin_id}/{namespace}",
    tag = "Plugin Config",
    operation_id = "deleteContestConfig",
    summary = "Delete config for a namespace on a contest",
    params(
        ("id" = i32, Path, description = "Contest ID"),
        ("plugin_id" = String, Path, description = "Plugin ID"),
        ("namespace" = String, Path, description = "Config namespace"),
    ),
    responses(
        (status = 204, description = "Config deleted"),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = crate::error::ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = crate::error::ErrorBody),
        (status = 404, description = "Config not found (NOT_FOUND)", body = crate::error::ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id, plugin_id, namespace))]
pub async fn delete_contest_config(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath((contest_id, plugin_id, namespace)): AppPath<(i32, String, String)>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    validate_plugin_id(&plugin_id)?;
    validate_namespace(&namespace)?;
    let txn = state.db.begin().await?;
    find_contest(&txn, contest_id).await?;
    let target = ConfigTarget::contest(contest_id);
    let result = delete_config(&txn, &target, &plugin_id, &namespace).await?;
    txn.commit().await?;
    Ok(result)
}
