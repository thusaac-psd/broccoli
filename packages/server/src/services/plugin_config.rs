use axum::Json;
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Set, sea_query::OnConflict,
};

use broccoli_server_sdk::types::{ConfigValidateInput, ConfigValidateOutput};
use plugin_core::config_schema::{self, FieldError, FieldErrorKind};
pub(crate) use plugin_core::manifest::ConfigScope;
use plugin_core::manifest::{ConfigNamespace, PluginManifest};
use plugin_core::registry::{PluginRegistry, PluginStatus};
use plugin_core::traits::PluginManager;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::db::{DatabaseConnection, DatabaseTransaction};

use crate::entity::{contest_problem, plugin_config};
use crate::error::AppError;
use crate::host_funcs::config::{extract_plugin_id, resolve_namespace, strip_namespace_prefix};
use crate::host_funcs::sql::ReadThrough;
use crate::models::plugin_config::{PluginConfigResponse, config_key};
use crate::services::config_issues::{Recheck, recheck_problems};

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
    pub(crate) fn of_row(scope: &str, ref_id: &str) -> Option<Self> {
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
struct ConfigTargetPattern {
    scope: ConfigScope,
    ref_id_pattern: String,
}

impl ConfigTargetPattern {
    fn contest_problem_by_problem(problem_id: i32) -> Self {
        Self {
            scope: ConfigScope::ContestProblem,
            ref_id_pattern: config_key::contest_problem_by_problem_like(problem_id),
        }
    }

    fn contest_problem_by_contest(contest_id: i32) -> Self {
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
/// schema. Returns the namespace's declaration.
fn check_schema(
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

/// What the plugins a config check calls read through the host.
#[derive(Clone, Copy)]
pub(crate) enum Reads<'a> {
    /// The committed data.
    Committed,
    /// The data as a change the request holds in this transaction leaves it,
    /// before it commits. Only reads the host makes for a plugin
    /// (`host.submission.query_test_cases`) see the change; a plugin's own
    /// SQL reads committed data.
    Through(&'a Arc<DatabaseTransaction>),
    /// What the plugin call a host function serves reads: the check runs as
    /// a call nested under it.
    InCall,
}

impl Reads<'_> {
    fn call(self) -> plugin_core::executor::CallOptions {
        match self {
            Reads::Committed => crate::interactive_call(),
            Reads::Through(txn) => crate::interactive_call().with_locals(ReadThrough::locals(txn)),
            Reads::InCall => plugin_core::executor::CallOptions::nested(),
        }
    }
}

/// Checks a config before it is stored: its schema ([`check_schema`]), then
/// the namespace's `validate` function, if it declares one, for what only the
/// plugin knows about the target, the plugin reading as `reads` says. Either
/// refusal is [`AppError::InvalidConfig`] with the offending fields.
async fn check_config(
    plugins: &dyn PluginManager,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: &serde_json::Value,
    reads: Reads<'_>,
) -> Result<(), AppError> {
    let ns = check_schema(
        plugins.get_registry(),
        target.scope(),
        plugin_id,
        namespace,
        config,
    )?;
    let errors = plugin_verdict(plugins, target, plugin_id, namespace, &ns, config, reads).await?;
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
    reads: Reads<'_>,
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
        .call_raw(plugin_id, function, input, reads.call())
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
    Ok(plugin_field_errors(plugin_id, output))
}

/// A `validate` function's refusals as field errors. A key outside the
/// plugin's own id would show a host string, or another plugin's, in place of
/// the plugin's words, so it is dropped and the message shows.
fn plugin_field_errors(plugin_id: &str, output: ConfigValidateOutput) -> Vec<FieldError> {
    let own = format!("{plugin_id}.");
    output
        .errors
        .into_iter()
        .map(|v| FieldError {
            path: v.path,
            message: v.message,
            kind: FieldErrorKind::Plugin {
                key: v.key.filter(|key| key.starts_with(&own)),
                params: v.params,
            },
        })
        .collect()
}

/// What a config write sets on its row besides the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowSettings {
    /// An admin's write: the row's `enabled` toggle and its position.
    Set {
        enabled: Option<bool>,
        position: i32,
    },
    /// A write that leaves a stored row's toggle and position as they are,
    /// and gives a new row none and position 0.
    Kept,
}

/// The stored configs a write or delete changed, as the problems whose
/// `config_issues` flag the change can move and which way it can move them
/// (see [`crate::services::config_issues`]). Only the writes and deletes of
/// this module make one, and [`ConfigChange::recheck`] spends it once the
/// change commits.
#[must_use = "recheck the config issues a config change reaches once it commits"]
#[derive(Debug)]
pub(crate) struct ConfigChange {
    problems: BTreeSet<i32>,
    recheck: Recheck,
}

impl ConfigChange {
    /// The change of a write to, or a delete of, a config of `target`. A
    /// problem or contest-problem config is checked before it is stored and
    /// a deleted one is checked no more, so the change can only resolve that
    /// problem's issues. A contest or plugin-scope config is what its
    /// problems' configs may be checked against, so the change can move
    /// their flags either way.
    async fn of<C: ConnectionTrait>(db: &C, target: &ConfigTarget) -> Result<Self, AppError> {
        Ok(match target.scope {
            ConfigScope::Problem | ConfigScope::ContestProblem => Self {
                problems: target.problem_id.into_iter().collect(),
                recheck: Recheck::ClearOnly,
            },
            ConfigScope::Contest => Self {
                problems: problems_of_contest(db, target.contest_id).await?,
                recheck: Recheck::Both,
            },
            ConfigScope::Plugin => Self {
                problems: problems_with_configs_of(db, Some(&[target.ref_id()])).await?,
                recheck: Recheck::Both,
            },
        })
    }

    fn and(mut self, other: Self) -> Self {
        self.problems.extend(other.problems);
        self.recheck = self.recheck.max(other.recheck);
        self
    }

    /// Rechecks the flags the change reaches, the plugins reading as `reads`
    /// says. Runs after the change commits, never on the judging path; a
    /// recheck that fails is logged and leaves the flag as it was.
    pub(crate) async fn recheck(
        self,
        db: &DatabaseConnection,
        plugins: &dyn PluginManager,
        reads: Reads<'_>,
    ) {
        recheck_problems(db, plugins, self.problems, self.recheck, reads).await;
    }
}

/// The problems of contest `contest_id`; none for no contest.
async fn problems_of_contest<C: ConnectionTrait>(
    db: &C,
    contest_id: Option<i32>,
) -> Result<BTreeSet<i32>, AppError> {
    let Some(contest_id) = contest_id else {
        return Ok(BTreeSet::new());
    };
    let problems = contest_problem::Entity::find()
        .select_only()
        .column(contest_problem::Column::ProblemId)
        .filter(contest_problem::Column::ContestId.eq(contest_id))
        .into_tuple::<i32>()
        .all(db)
        .await?;
    Ok(problems.into_iter().collect())
}

/// Checks a config ([`check_config`], its plugin reading as `reads` says) and
/// stores it at `target`. Every config write, an admin's or a plugin's,
/// goes through here, so no config is stored unchecked; the returned change
/// rechecks the flags the write reaches once it commits.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_config<C: ConnectionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    reads: Reads<'_>,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: serde_json::Value,
    settings: RowSettings,
) -> Result<(PluginConfigResponse, ConfigChange), AppError> {
    check_config(plugins, target, plugin_id, namespace, &config, reads).await?;
    let (enabled, position, kept) = match settings {
        RowSettings::Set { enabled, position } => (enabled, position, false),
        RowSettings::Kept => (None, 0, true),
    };
    let mut updated = vec![
        plugin_config::Column::Config,
        plugin_config::Column::UpdatedAt,
    ];
    if !kept {
        updated.extend([
            plugin_config::Column::Enabled,
            plugin_config::Column::Position,
        ]);
    }
    let row = plugin_config::Entity::insert(plugin_config::ActiveModel {
        scope: Set(target.scope().as_str().to_string()),
        ref_id: Set(target.ref_id().to_string()),
        namespace: Set(target.stored_namespace(plugin_id, namespace)),
        config: Set(config),
        enabled: Set(enabled),
        position: Set(position),
        updated_at: sea_orm::NotSet,
    })
    .on_conflict(
        OnConflict::columns([
            plugin_config::Column::Scope,
            plugin_config::Column::RefId,
            plugin_config::Column::Namespace,
        ])
        .update_columns(updated)
        .to_owned(),
    )
    .exec_with_returning(db)
    .await?;
    let change = ConfigChange::of(db, target).await?;
    Ok((
        PluginConfigResponse {
            plugin_id: plugin_id.to_string(),
            namespace: strip_namespace_prefix(&row.namespace).to_string(),
            config: row.config,
            enabled: row.enabled,
            position: row.position,
            updated_at: Some(row.updated_at),
            json_schema: None,
            description: None,
        },
        change,
    ))
}

/// Deletes one stored config; [`AppError::NotFound`] when there is none.
pub(crate) async fn delete_config<C: ConnectionTrait>(
    db: &C,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
) -> Result<ConfigChange, AppError> {
    let stored_namespace = target.stored_namespace(plugin_id, namespace);
    let deleted = plugin_config::Entity::delete_many()
        .filter(plugin_config::Column::Scope.eq(target.scope().as_str()))
        .filter(plugin_config::Column::RefId.eq(target.ref_id()))
        .filter(plugin_config::Column::Namespace.eq(&stored_namespace))
        .exec(db)
        .await?;
    if deleted.rows_affected == 0 {
        let display_ns = strip_namespace_prefix(&stored_namespace);
        return Err(AppError::NotFound(format!(
            "Config '{display_ns}' not found"
        )));
    }
    ConfigChange::of(db, target).await
}

/// Deletes every config of problem `problem_id`, at the problem scope and in
/// each contest, as the problem goes.
pub(crate) async fn delete_configs_of_problem<C: ConnectionTrait>(
    db: &C,
    problem_id: i32,
) -> Result<ConfigChange, AppError> {
    let target = ConfigTarget::problem(problem_id);
    delete_configs_at(db, &target).await?;
    delete_configs_like(
        db,
        &ConfigTargetPattern::contest_problem_by_problem(problem_id),
    )
    .await?;
    ConfigChange::of(db, &target).await
}

/// Deletes every config of contest `contest_id`, its own and those of its
/// problems in it, as the contest goes.
pub(crate) async fn delete_configs_of_contest<C: ConnectionTrait>(
    db: &C,
    contest_id: i32,
) -> Result<ConfigChange, AppError> {
    let target = ConfigTarget::contest(contest_id);
    let change = ConfigChange::of(db, &target).await?;
    delete_configs_at(db, &target).await?;
    delete_configs_like(
        db,
        &ConfigTargetPattern::contest_problem_by_contest(contest_id),
    )
    .await?;
    Ok(change)
}

/// Deletes the configs of problems `problem_ids` in contest `contest_id`, as
/// they leave it.
pub(crate) async fn delete_configs_of_contest_problems<C: ConnectionTrait>(
    db: &C,
    contest_id: i32,
    problem_ids: &[i32],
) -> Result<ConfigChange, AppError> {
    let mut change = ConfigChange {
        problems: BTreeSet::new(),
        recheck: Recheck::ClearOnly,
    };
    let targets: Vec<ConfigTarget> = problem_ids
        .iter()
        .map(|&problem_id| ConfigTarget::contest_problem(contest_id, problem_id))
        .collect();
    if targets.is_empty() {
        return Ok(change);
    }
    plugin_config::Entity::delete_many()
        .filter(plugin_config::Column::Scope.eq(ConfigScope::ContestProblem.as_str()))
        .filter(plugin_config::Column::RefId.is_in(targets.iter().map(ConfigTarget::ref_id)))
        .exec(db)
        .await?;
    for target in &targets {
        change = change.and(ConfigChange::of(db, target).await?);
    }
    Ok(change)
}

async fn delete_configs_at<C: ConnectionTrait>(
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

async fn delete_configs_like<C: ConnectionTrait>(
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
            None => vec![config_schema::FieldError::new(
                "",
                config_schema::FieldErrorKind::NamespaceUndeclared {
                    scope: scope.as_str().into(),
                },
            )],
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

/// The problems that have a stored config, at the problem scope or in a
/// contest, of any plugin in `plugin_ids` (of any plugin at all for `None`):
/// those whose config issues a change to one of these plugins can change.
pub(crate) async fn problems_with_configs_of<C: ConnectionTrait>(
    db: &C,
    plugin_ids: Option<&[&str]>,
) -> Result<BTreeSet<i32>, AppError> {
    let rows = plugin_config::Entity::find()
        .filter(plugin_config::Column::Scope.is_in([
            ConfigScope::Problem.as_str(),
            ConfigScope::ContestProblem.as_str(),
        ]))
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|row| plugin_ids.is_none_or(|ids| ids.contains(&extract_plugin_id(&row.namespace))))
        .filter_map(|row| ConfigTarget::of_row(&row.scope, &row.ref_id)?.problem_id)
        .collect())
}

/// Every stored config of a problem, at the problem scope and, with
/// `include_contests`, in each contest that uses it, that its namespace would
/// refuse if an admin wrote it now, its plugin reading as `reads` says: test
/// data a config refers to may have changed since it was stored. Configs of
/// plugins not loaded here are skipped; nothing reads them.
pub(crate) async fn problem_config_issues<C: ConnectionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    problem_id: i32,
    include_contests: bool,
    reads: Reads<'_>,
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
        let loaded = plugins
            .get_registry()
            .read()
            .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?
            .get(plugin_id)
            .is_some_and(|entry| entry.status == PluginStatus::Loaded);
        if !loaded {
            continue;
        }
        let errors =
            match check_config(plugins, &target, plugin_id, namespace, &row.config, reads).await {
                Ok(()) => continue,
                Err(AppError::InvalidConfig(errors)) => errors,
                Err(AppError::ConfigNamespaceNotFound(_)) => vec![FieldError::new(
                    "",
                    FieldErrorKind::NamespaceUndeclared {
                        scope: target.scope().as_str().into(),
                    },
                )],
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

#[cfg(test)]
mod tests {
    use broccoli_server_sdk::types::ConfigViolation;

    use super::*;

    #[test]
    fn a_refusal_keeps_only_a_key_of_the_plugins_own_pack() {
        let output = ConfigValidateOutput {
            errors: vec![
                ConfigViolation::new("subtasks[0].name", "taken")
                    .key("ioi.config.task.nameTaken")
                    .param("name", "a"),
                ConfigViolation::new("", "spoofed").key("apiError.NOT_FOUND"),
                ConfigViolation::new("", "other plugin").key("ioix.config.x"),
                ConfigViolation::new("", "no key"),
            ],
        };
        let kinds: Vec<_> = plugin_field_errors("ioi", output)
            .into_iter()
            .map(|e| (e.message, e.kind))
            .collect();
        let plugin = |key: Option<&str>, params: &[(&str, &str)]| FieldErrorKind::Plugin {
            key: key.map(Into::into),
            params: params
                .iter()
                .map(|(k, v)| ((*k).into(), serde_json::Value::from(*v)))
                .collect(),
        };
        assert_eq!(
            kinds,
            vec![
                (
                    "taken".into(),
                    plugin(Some("ioi.config.task.nameTaken"), &[("name", "a")])
                ),
                ("spoofed".into(), plugin(None, &[])),
                ("other plugin".into(), plugin(None, &[])),
                ("no key".into(), plugin(None, &[])),
            ]
        );
    }
}
