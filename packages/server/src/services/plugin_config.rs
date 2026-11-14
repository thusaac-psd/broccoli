use axum::{Json, http::StatusCode};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set,
    sea_query::OnConflict,
};

use broccoli_server_sdk::types::{ConfigValidateInput, ConfigValidateOutput};
use plugin_core::config_schema::{self, FieldError};
pub(crate) use plugin_core::manifest::ConfigScope;
use plugin_core::manifest::{ConfigNamespace, PluginManifest};
use plugin_core::registry::PluginRegistry;
use plugin_core::traits::PluginManager;

use crate::entity::plugin_config;
use crate::error::AppError;
use crate::host_funcs::config::{extract_plugin_id, resolve_namespace, strip_namespace_prefix};
use crate::models::plugin_config::{PluginConfigResponse, config_key};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigTarget {
    scope: ConfigScope,
    ref_id: String,
    contest_id: Option<i32>,
    problem_id: Option<i32>,
}

impl ConfigTarget {
    pub(crate) fn plugin(plugin_id: &str) -> Self {
        Self {
            scope: ConfigScope::Plugin,
            ref_id: config_key::plugin(plugin_id),
            contest_id: None,
            problem_id: None,
        }
    }

    pub(crate) fn problem(problem_id: i32) -> Self {
        Self {
            scope: ConfigScope::Problem,
            ref_id: config_key::problem(problem_id),
            contest_id: None,
            problem_id: Some(problem_id),
        }
    }

    pub(crate) fn contest_problem(contest_id: i32, problem_id: i32) -> Self {
        Self {
            scope: ConfigScope::ContestProblem,
            ref_id: config_key::contest_problem(contest_id, problem_id),
            contest_id: Some(contest_id),
            problem_id: Some(problem_id),
        }
    }

    pub(crate) fn contest(contest_id: i32) -> Self {
        Self {
            scope: ConfigScope::Contest,
            ref_id: config_key::contest(contest_id),
            contest_id: Some(contest_id),
            problem_id: None,
        }
    }

    /// The target of a stored row, `None` for a scope or reference the host
    /// never writes.
    fn of_row(scope: &str, ref_id: &str) -> Option<Self> {
        let id = |raw: &str| raw.parse::<i32>().ok();
        match ConfigScope::parse(scope)? {
            ConfigScope::Plugin => Some(Self::plugin(ref_id)),
            ConfigScope::Problem => Some(Self::problem(id(ref_id)?)),
            ConfigScope::Contest => Some(Self::contest(id(ref_id)?)),
            ConfigScope::ContestProblem => {
                let (contest, problem) = ref_id.split_once(':')?;
                Some(Self::contest_problem(id(contest)?, id(problem)?))
            }
        }
    }

    pub(crate) fn scope(&self) -> ConfigScope {
        self.scope
    }

    pub(crate) fn ref_id(&self) -> &str {
        &self.ref_id
    }

    fn stored_namespace(&self, plugin_id: &str, namespace: &str) -> String {
        if self.scope == ConfigScope::Plugin {
            namespace.to_string()
        } else {
            resolve_namespace(self.scope.as_str(), plugin_id, namespace)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ConfigTargetPattern {
    scope: ConfigScope,
    ref_id_pattern: String,
}

impl ConfigTargetPattern {
    pub(crate) fn contest_problem_by_problem(problem_id: i32) -> Self {
        Self {
            scope: ConfigScope::ContestProblem,
            ref_id_pattern: config_key::contest_problem_by_problem_like(problem_id),
        }
    }

    pub(crate) fn contest_problem_by_contest(contest_id: i32) -> Self {
        Self {
            scope: ConfigScope::ContestProblem,
            ref_id_pattern: config_key::contest_problem_by_contest_like(contest_id),
        }
    }

    fn scope(&self) -> ConfigScope {
        self.scope
    }

    fn ref_id_pattern(&self) -> &str {
        &self.ref_id_pattern
    }
}

pub(crate) async fn get_config<C: ConnectionTrait>(
    db: &C,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
) -> Result<Json<PluginConfigResponse>, AppError> {
    let stored_namespace = target.stored_namespace(plugin_id, namespace);
    let row = plugin_config::Entity::find_by_id((
        target.scope().as_str().to_string(),
        target.ref_id().to_string(),
        stored_namespace.clone(),
    ))
    .one(db)
    .await?;

    match row {
        Some(r) => {
            let namespace = strip_namespace_prefix(&r.namespace).to_string();
            Ok(Json(PluginConfigResponse {
                plugin_id: plugin_id.to_string(),
                namespace,
                config: r.config,
                enabled: r.enabled,
                position: r.position,
                updated_at: Some(r.updated_at),
                json_schema: None,
                description: None,
            }))
        }
        None => {
            let display_ns = strip_namespace_prefix(&stored_namespace);
            Err(AppError::NotFound(format!(
                "Config '{display_ns}' not found"
            )))
        }
    }
}

/// Checks a config against its schema: `plugin_id` must declare
/// `namespace` for `scope`, and `config` must satisfy that namespace's
/// schema. Every config write passes this; a plugin's writes to its own
/// namespaces pass only this, since the plugin owns those values. Returns
/// the namespace's declaration.
pub(crate) fn check_schema(
    registry: &PluginRegistry,
    scope: ConfigScope,
    plugin_id: &str,
    namespace: &str,
    config: &serde_json::Value,
) -> Result<ConfigNamespace, AppError> {
    let registry = registry
        .read()
        .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?;
    let not_found = |why: String| AppError::ConfigNamespaceNotFound(why);
    let entry = registry
        .get(plugin_id)
        .ok_or_else(|| not_found(format!("Plugin '{plugin_id}' is not installed")))?;
    let ns = entry.manifest.config.get(namespace).ok_or_else(|| {
        not_found(format!(
            "Plugin '{plugin_id}' declares no config namespace '{namespace}'"
        ))
    })?;
    if !ns.scopes.contains(&scope) {
        return Err(not_found(format!(
            "Config namespace '{plugin_id}/{namespace}' cannot be set at the {} scope",
            scope.as_str()
        )));
    }
    config_schema::validate(ns, config).map_err(AppError::InvalidConfig)?;
    Ok(ns.clone())
}

/// Checks an admin's config before it is stored: its schema
/// ([`check_schema`]), then the namespace's `validate` function, if it
/// declares one, for what only the plugin knows about the target. Either
/// refusal is [`AppError::InvalidConfig`] with the offending fields.
pub(crate) async fn validate_config(
    plugins: &dyn PluginManager,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: &serde_json::Value,
) -> Result<(), AppError> {
    let ns = check_schema(
        plugins.get_registry(),
        target.scope(),
        plugin_id,
        namespace,
        config,
    )?;
    let errors = plugin_verdict(plugins, target, plugin_id, namespace, &ns, config).await?;
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::InvalidConfig(errors))
    }
}

/// What the namespace's `validate` function says about a config that passed
/// its schema: nothing when it declares none. A plugin that cannot answer
/// refuses the write rather than letting an unchecked value through.
async fn plugin_verdict(
    plugins: &dyn PluginManager,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    ns: &ConfigNamespace,
    config: &serde_json::Value,
) -> Result<Vec<FieldError>, AppError> {
    let Some(function) = &ns.validate else {
        return Ok(Vec::new());
    };
    let input = ConfigValidateInput {
        namespace: namespace.to_string(),
        scope: target.scope(),
        contest_id: target.contest_id,
        problem_id: target.problem_id,
        config: ns.resolve(Some(config)),
    };
    let input = serde_json::to_vec(&input)
        .map_err(|e| AppError::Internal(format!("config validate input: {e}")))?;
    let output = plugins
        .call_raw(plugin_id, function, input, crate::interactive_call())
        .await
        .map_err(|e| {
            tracing::warn!(plugin_id, function, error = %e, "config validate call failed");
            AppError::from(e)
        })?;
    let output: ConfigValidateOutput = serde_json::from_slice(&output).map_err(|e| {
        tracing::error!(plugin_id, function, error = %e, "config validate answer unreadable");
        AppError::Internal(format!(
            "plugin '{plugin_id}' gave an unreadable answer from '{function}'"
        ))
    })?;
    Ok(output
        .errors
        .into_iter()
        .map(|v| FieldError {
            path: v.path,
            message: v.message,
        })
        .collect())
}

/// Validates and stores an admin's config (see [`validate_config`]).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upsert_config<C: ConnectionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: serde_json::Value,
    enabled: Option<bool>,
    position: i32,
) -> Result<Json<PluginConfigResponse>, AppError> {
    validate_config(plugins, target, plugin_id, namespace, &config).await?;
    store_config(db, target, plugin_id, namespace, config, enabled, position).await
}

/// Stores a config row as given. Only for records the host itself owns and
/// shapes; anything an admin or a plugin writes goes through
/// [`upsert_config`].
pub(crate) async fn store_config<C: ConnectionTrait>(
    db: &C,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: serde_json::Value,
    enabled: Option<bool>,
    position: i32,
) -> Result<Json<PluginConfigResponse>, AppError> {
    let namespace_str = target.stored_namespace(plugin_id, namespace);

    let active = plugin_config::ActiveModel {
        scope: Set(target.scope().as_str().to_string()),
        ref_id: Set(target.ref_id().to_string()),
        namespace: Set(namespace_str.clone()),
        config: Set(config.clone()),
        enabled: Set(enabled),
        position: Set(position),
        updated_at: sea_orm::NotSet,
    };

    let row = plugin_config::Entity::insert(active)
        .on_conflict(
            OnConflict::columns([
                plugin_config::Column::Scope,
                plugin_config::Column::RefId,
                plugin_config::Column::Namespace,
            ])
            .update_columns([
                plugin_config::Column::Config,
                plugin_config::Column::Enabled,
                plugin_config::Column::Position,
                plugin_config::Column::UpdatedAt,
            ])
            .to_owned(),
        )
        .exec_with_returning(db)
        .await?;

    Ok(Json(PluginConfigResponse {
        plugin_id: plugin_id.to_string(),
        namespace: strip_namespace_prefix(&namespace_str).to_string(),
        config,
        enabled,
        position,
        updated_at: Some(row.updated_at),
        json_schema: None,
        description: None,
    }))
}

pub(crate) async fn delete_config<C: ConnectionTrait>(
    db: &C,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
) -> Result<StatusCode, AppError> {
    let stored_namespace = target.stored_namespace(plugin_id, namespace);
    let row = plugin_config::Entity::find_by_id((
        target.scope().as_str().to_string(),
        target.ref_id().to_string(),
        stored_namespace.clone(),
    ))
    .one(db)
    .await?;

    match row {
        Some(r) => {
            let active: plugin_config::ActiveModel = r.into();
            active.delete(db).await?;
            Ok(StatusCode::NO_CONTENT)
        }
        None => {
            let display_ns = strip_namespace_prefix(&stored_namespace);
            Err(AppError::NotFound(format!(
                "Config '{display_ns}' not found"
            )))
        }
    }
}

pub(crate) async fn delete_config_by_target<C: ConnectionTrait>(
    db: &C,
    target: &ConfigTarget,
) -> Result<(), AppError> {
    plugin_config::Entity::delete_many()
        .filter(plugin_config::Column::Scope.eq(target.scope().as_str()))
        .filter(plugin_config::Column::RefId.eq(target.ref_id()))
        .exec(db)
        .await?;
    Ok(())
}

pub(crate) async fn delete_config_by_target_pattern<C: ConnectionTrait>(
    db: &C,
    target_pattern: &ConfigTargetPattern,
) -> Result<(), AppError> {
    plugin_config::Entity::delete_many()
        .filter(plugin_config::Column::Scope.eq(target_pattern.scope().as_str()))
        .filter(plugin_config::Column::RefId.like(target_pattern.ref_id_pattern()))
        .exec(db)
        .await?;
    Ok(())
}

pub use broccoli_api_types::plugin::IncompatibleConfig;

/// Every stored config of `plugin_id` that `manifest` would refuse, so an
/// admin sees what an upgrade breaks before publishing it.
pub(crate) async fn incompatible_configs<C: ConnectionTrait>(
    db: &C,
    plugin_id: &str,
    manifest: &PluginManifest,
) -> Result<Vec<IncompatibleConfig>, AppError> {
    let prefix = format!("{plugin_id}:");
    // `_` in a plugin id is a LIKE wildcard, so the prefix is checked again
    // exactly below.
    let rows = plugin_config::Entity::find()
        .filter(
            sea_orm::Condition::any()
                .add(
                    sea_orm::Condition::all()
                        .add(plugin_config::Column::Scope.eq(ConfigScope::Plugin.as_str()))
                        .add(plugin_config::Column::RefId.eq(plugin_id)),
                )
                .add(
                    sea_orm::Condition::all()
                        .add(plugin_config::Column::Scope.ne(ConfigScope::Plugin.as_str()))
                        .add(plugin_config::Column::Namespace.like(format!("{prefix}%"))),
                ),
        )
        .all(db)
        .await?;
    let mut incompatible = Vec::new();
    for row in rows {
        let Some(scope) = ConfigScope::parse(&row.scope) else {
            continue;
        };
        let namespace = if scope == ConfigScope::Plugin {
            row.namespace.as_str()
        } else {
            match row.namespace.strip_prefix(&prefix) {
                Some(raw) => raw,
                None => continue,
            }
        };
        let errors = match manifest
            .config
            .get(namespace)
            .filter(|ns| ns.scopes.contains(&scope))
        {
            Some(ns) => match config_schema::validate(ns, &row.config) {
                Ok(()) => continue,
                Err(errors) => errors,
            },
            None => vec![config_schema::FieldError {
                path: String::new(),
                message: format!(
                    "the plugin no longer declares this namespace at the {} scope",
                    scope.as_str()
                ),
            }],
        };
        incompatible.push(IncompatibleConfig {
            plugin_id: plugin_id.to_string(),
            scope,
            ref_id: row.ref_id,
            namespace: namespace.to_string(),
            errors,
        });
    }
    incompatible.sort_by(|a, b| {
        (a.scope.as_str(), &a.ref_id, &a.namespace).cmp(&(
            b.scope.as_str(),
            &b.ref_id,
            &b.namespace,
        ))
    });
    Ok(incompatible)
}

/// Every stored config of a problem, at the problem scope and, with
/// `include_contests`, in each contest that uses it, that its namespace would
/// refuse if an admin wrote it now: test data a config refers to may have
/// changed since it was stored. Configs of plugins not installed here are
/// skipped; nothing reads them.
pub(crate) async fn problem_config_issues<C: ConnectionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    problem_id: i32,
    include_contests: bool,
) -> Result<Vec<IncompatibleConfig>, AppError> {
    let mut targets = sea_orm::Condition::any().add(
        sea_orm::Condition::all()
            .add(plugin_config::Column::Scope.eq(ConfigScope::Problem.as_str()))
            .add(plugin_config::Column::RefId.eq(config_key::problem(problem_id))),
    );
    if include_contests {
        targets = targets.add(
            sea_orm::Condition::all()
                .add(plugin_config::Column::Scope.eq(ConfigScope::ContestProblem.as_str()))
                .add(
                    plugin_config::Column::RefId
                        .like(config_key::contest_problem_by_problem_like(problem_id)),
                ),
        );
    }
    let rows = plugin_config::Entity::find()
        .filter(targets)
        .all(db)
        .await?;
    let mut issues = Vec::new();
    for row in rows {
        let Some(target) = ConfigTarget::of_row(&row.scope, &row.ref_id)
            .filter(|t| t.problem_id == Some(problem_id))
        else {
            continue;
        };
        let plugin_id = extract_plugin_id(&row.namespace);
        let namespace = strip_namespace_prefix(&row.namespace);
        let installed = plugins
            .get_registry()
            .read()
            .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?
            .contains_key(plugin_id);
        if !installed {
            continue;
        }
        let errors =
            match validate_config(plugins, &target, plugin_id, namespace, &row.config).await {
                Ok(()) => continue,
                Err(AppError::InvalidConfig(errors)) => errors,
                Err(AppError::ConfigNamespaceNotFound(why)) => vec![FieldError {
                    path: String::new(),
                    message: why,
                }],
                Err(other) => return Err(other),
            };
        issues.push(IncompatibleConfig {
            plugin_id: plugin_id.to_string(),
            scope: target.scope(),
            ref_id: row.ref_id.clone(),
            namespace: namespace.to_string(),
            errors,
        });
    }
    issues.sort_by(|a, b| {
        (a.scope.as_str(), &a.ref_id, &a.plugin_id, &a.namespace).cmp(&(
            b.scope.as_str(),
            &b.ref_id,
            &b.plugin_id,
            &b.namespace,
        ))
    });
    Ok(issues)
}
