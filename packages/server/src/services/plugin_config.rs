use axum::Json;
use sea_orm::sea_query::{LockType, OnConflict};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Set, TransactionSession,
    TransactionTrait,
};

use broccoli_server_sdk::types::{ConfigValidateInput, ConfigValidateOutput, ConfigViolation};
use plugin_core::config_schema::{self, FieldError, FieldErrorKind};
pub(crate) use plugin_core::manifest::ConfigScope;
use plugin_core::manifest::{ConfigNamespace, PluginManifest};
use plugin_core::registry::PluginRegistry;
use plugin_core::traits::PluginManager;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::db::{DatabaseConnection, DatabaseTransaction};

use crate::entity::{contest_problem, plugin_config, problem};
use crate::error::AppError;
use crate::host_funcs::config::{resolve_namespace, strip_namespace_prefix};
use crate::host_funcs::sql::ReadThrough;
use crate::models::plugin_config::{PluginConfigResponse, config_key};
use crate::services::config_issues::{
    ConfigChecker, Recheck, problems_checked_by, recheck_problems,
};

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

    /// The problem the config belongs to, at the `problem` and
    /// `contest_problem` scopes.
    pub(crate) fn problem_id(&self) -> Option<i32> {
        self.problem_id
    }

    /// The key a config of `plugin_id`'s `namespace` is stored under here.
    pub(crate) fn stored_namespace(&self, plugin_id: &str, namespace: &str) -> String {
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

/// What checking a config found.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Findings {
    /// What is wrong with the config itself: what its schema refuses, and
    /// the errors of the namespace's `validate` function. No write with any
    /// is stored.
    pub(crate) refusals: Vec<FieldError>,
    /// What the target's data does not meet yet, the `validate` function's
    /// issues: a write is stored with them, and they are listed on the
    /// target's config issues.
    pub(crate) issues: Vec<FieldError>,
}

impl Findings {
    /// Everything found, the refusals first: what a config issue lists.
    pub(crate) fn into_all(self) -> Vec<FieldError> {
        let mut all = self.refusals;
        all.extend(self.issues);
        all
    }
}

/// Checks a config: its schema ([`check_schema`]), then the namespace's
/// `validate` function, if it declares one, for what only the plugin knows
/// about the target, the plugin reading as `reads` says. The config's
/// namespace must exist at the target's scope
/// ([`AppError::ConfigNamespaceNotFound`] otherwise).
pub(crate) async fn check_config(
    plugins: &dyn PluginManager,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: &serde_json::Value,
    reads: Reads<'_>,
) -> Result<Findings, AppError> {
    let ns = match check_schema(
        plugins.get_registry(),
        target.scope(),
        plugin_id,
        namespace,
        config,
    ) {
        Ok(ns) => ns,
        Err(AppError::InvalidConfig(refusals)) => {
            return Ok(Findings {
                refusals,
                issues: Vec::new(),
            });
        }
        Err(other) => return Err(other),
    };
    plugin_verdict(plugins, target, plugin_id, namespace, &ns, config, reads).await
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
) -> Result<Findings, AppError> {
    let Some(function) = &ns.validate else {
        return Ok(Findings::default());
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
    Ok(Findings {
        refusals: plugin_field_errors(plugin_id, output.errors),
        issues: plugin_field_errors(plugin_id, output.issues),
    })
}

/// A `validate` function's violations as field errors. A key outside the
/// plugin's own id would show a host string, or another plugin's, in place of
/// the plugin's words, so it is dropped and the message shows.
fn plugin_field_errors(plugin_id: &str, violations: Vec<ConfigViolation>) -> Vec<FieldError> {
    let own = format!("{plugin_id}.");
    violations
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

/// A change to the configs, or to what they are checked against, as the
/// problems whose `config_issues` flag it can move and which way it can move
/// them (see [`crate::services::config_issues`]). [`ConfigChange::recheck`]
/// spends it once the change commits.
#[must_use = "recheck the config issues a config change reaches once it commits"]
#[derive(Debug)]
pub(crate) struct ConfigChange {
    problems: BTreeSet<i32>,
    /// Plugins whose checks the change reaches wherever they run: a
    /// plugin-scope config is what each of their checks may read. Resolved
    /// to problems when the recheck runs ([`problems_checked_by`]).
    plugins: Vec<String>,
    recheck: Recheck,
}

impl ConfigChange {
    /// The change of a write to, or a delete of, a config of `target`. A
    /// problem or contest-problem config is checked at that problem, with
    /// the namespace's defaults once none is stored, and a stored one may
    /// carry issues its data does not meet yet, so the change moves that
    /// problem's flag either way. A contest or plugin-scope config is what
    /// the configs of its problems or its plugin's checks may be checked
    /// against, so the change moves their flags either way.
    async fn of<C: ConnectionTrait>(db: &C, target: &ConfigTarget) -> Result<Self, AppError> {
        Ok(match target.scope {
            ConfigScope::Problem | ConfigScope::ContestProblem => Self {
                problems: target.problem_id.into_iter().collect(),
                plugins: Vec::new(),
                recheck: Recheck::Both,
            },
            ConfigScope::Contest => Self::of_contest(db, target.contest_id).await?,
            ConfigScope::Plugin => Self {
                problems: BTreeSet::new(),
                plugins: vec![target.ref_id().to_string()],
                recheck: Recheck::Both,
            },
        })
    }

    /// A change to which checks reach problem `problem_id`, or to what they
    /// read: the problem is new, its types changed, or it joined a contest.
    pub(crate) fn of_problem(problem_id: i32) -> Self {
        Self {
            problems: BTreeSet::from([problem_id]),
            plugins: Vec::new(),
            recheck: Recheck::Both,
        }
    }

    /// The same for every problem of contest `contest_id` (none for no
    /// contest): the contest's type changed, or one of its configs did.
    pub(crate) async fn of_contest<C: ConnectionTrait>(
        db: &C,
        contest_id: impl Into<Option<i32>>,
    ) -> Result<Self, AppError> {
        Ok(Self {
            problems: problems_of_contest(db, contest_id.into()).await?,
            plugins: Vec::new(),
            recheck: Recheck::Both,
        })
    }

    /// Problems `problem_ids` left the checks that reached them: they left a
    /// contest, or were deleted. Nothing new is checked, so the change can
    /// only resolve their issues.
    fn left(problem_ids: impl IntoIterator<Item = i32>) -> Self {
        Self {
            problems: problem_ids.into_iter().collect(),
            plugins: Vec::new(),
            recheck: Recheck::ClearOnly,
        }
    }

    /// Rechecks the flags the change reaches, the plugins reading as `reads`
    /// says. Runs after the change commits, never on the judging path; a
    /// recheck that fails is logged and leaves the flag as it was.
    pub(crate) async fn recheck(
        mut self,
        db: &DatabaseConnection,
        checker: &ConfigChecker<'_>,
        reads: Reads<'_>,
    ) {
        if !self.plugins.is_empty() {
            match problems_checked_by(db, checker, Some(&self.plugins)).await {
                Ok(problems) => self.problems.extend(problems),
                Err(error) => tracing::warn!(
                    plugins = ?self.plugins,
                    ?error,
                    "could not find the problems a plugin-scope config change reaches"
                ),
            }
        }
        recheck_problems(db, checker, self.problems, self.recheck, reads).await;
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
/// stores it at `target` unless a refusal was found. Every config write, an
/// admin's or a plugin's, goes through here, so no config is stored
/// unchecked; the returned change rechecks the flags the write reaches once
/// it commits.
///
/// A problem's configs are checked against its test data, so a write to a
/// problem or contest-problem config holds the problem's row lock, the lock
/// a test-data change holds ([`crate::services::test_data::TestDataEdit`]),
/// from its check until the write commits: no test-data change lands between
/// the check and the stored config.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn write_config<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    reads: Reads<'_>,
    target: &ConfigTarget,
    plugin_id: &str,
    namespace: &str,
    config: serde_json::Value,
    settings: RowSettings,
) -> Result<(PluginConfigResponse, ConfigChange), AppError> {
    let txn = db.begin().await?;
    if let Some(problem_id) = target.problem_id {
        problem::Entity::find_by_id(problem_id)
            .lock(LockType::Update)
            .one(&txn)
            .await?;
    }
    let findings = check_config(plugins, target, plugin_id, namespace, &config, reads).await?;
    if !findings.refusals.is_empty() {
        return Err(AppError::InvalidConfig(findings.refusals));
    }
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
    .exec_with_returning(&txn)
    .await?;
    let change = ConfigChange::of(&txn, target).await?;
    txn.commit().await?;
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
    delete_configs_at(db, &ConfigTarget::problem(problem_id)).await?;
    delete_configs_like(
        db,
        &ConfigTargetPattern::contest_problem_by_problem(problem_id),
    )
    .await?;
    Ok(ConfigChange::left([problem_id]))
}

/// Deletes every config of contest `contest_id`, its own and those of its
/// problems in it, as the contest goes.
pub(crate) async fn delete_configs_of_contest<C: ConnectionTrait>(
    db: &C,
    contest_id: i32,
) -> Result<ConfigChange, AppError> {
    let target = ConfigTarget::contest(contest_id);
    let change = ConfigChange::of_contest(db, contest_id).await?;
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
    if !problem_ids.is_empty() {
        plugin_config::Entity::delete_many()
            .filter(plugin_config::Column::Scope.eq(ConfigScope::ContestProblem.as_str()))
            .filter(
                plugin_config::Column::RefId.is_in(
                    problem_ids
                        .iter()
                        .map(|&problem_id| config_key::contest_problem(contest_id, problem_id)),
                ),
            )
            .exec(db)
            .await?;
    }
    Ok(ConfigChange::left(problem_ids.iter().copied()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_keeps_only_a_key_of_the_plugins_own_pack() {
        let violations = vec![
            ConfigViolation::new("subtasks[0].name", "taken")
                .key("ioi.config.task.nameTaken")
                .param("name", "a"),
            ConfigViolation::new("", "spoofed").key("apiError.NOT_FOUND"),
            ConfigViolation::new("", "other plugin").key("ioix.config.x"),
            ConfigViolation::new("", "no key"),
        ];
        let kinds: Vec<_> = plugin_field_errors("ioi", violations)
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
