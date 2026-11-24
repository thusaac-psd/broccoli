//! Config issues: what a plugin finds wrong, as things stand, with a config
//! of a problem or contest ([`config_issues`]).
//!
//! A plugin's checks run at every target its configs are stored at, and,
//! for each namespace that declares a `validate` function, at every problem
//! and contest of the types the plugin provides
//! ([`crate::registry::TypeProviders`]), with the namespace's defaults where
//! no config is stored. A check finds refusals, what is wrong with the
//! config itself, which no write stores, and issues, what the target's data
//! does not meet yet (a problem with no test cases), which a write stores;
//! both are listed.
//!
//! Only a replica that loads a plugin can run its checks. A plugin that is
//! enabled fleet-wide (`plugin.is_enabled`) but not loaded here, whose
//! checks reach a target (it stores a config there, or provides the
//! target's type), leaves the answer unknown here ([`Outcome::undecided`]).
//! Unknown reads as having issues and never clears anything: the problem
//! stays flagged and marked with the plugins a replica must load to decide
//! (`problem::Model::config_recheck`), and the first replica that loads them
//! all claims the mark and decides ([`recheck_marked`]). A plugin disabled
//! fleet-wide is not checked anywhere, so its configs count as having no
//! issues.
//!
//! A problem's `config_issues` flag (`problem::Model::config_issues`) records
//! whether anything is listed for it at the problem scope or in a contest
//! that uses it, or the answer is unknown. Judging copies the flag onto every judgement it claims, so
//! it must follow whatever can change the answer:
//!
//! - a test-data change sets it in its own commit
//!   ([`crate::services::test_data::TestDataEdit`]);
//! - every write or delete of a stored config, an admin's, a plugin's or the
//!   host's own as a problem or contest goes, returns the
//!   [`ConfigChange`](crate::services::plugin_config::ConfigChange) that rechecks the problems it reaches once it commits;
//!   so does every change to which checks reach a problem: a problem made,
//!   its types changed, a problem joining or leaving a contest, a contest's
//!   type changed;
//! - a change to a plugin (enabled, disabled, reloaded, upgraded, its upload
//!   removed) changes which checks run and how, so it rechecks the problems
//!   its checks reach and every flagged one ([`recheck_problems_of_plugins`]);
//! - a bundle import restores configs and test data without the plugins
//!   that check them, so it flags and marks each problem it restores for any
//!   replica to decide, and a running server decides the marked problems on
//!   its next reconcile pass ([`recheck_marked`]);
//! - a server start loads plugins that may check configs differently than
//!   the ones that last checked them, so it rechecks every problem a check
//!   reaches, is flagged or is marked ([`recheck_every_problem`]).
//!
//! The rechecks run after the change committed, on the action that made it,
//! never on the judging path. A recheck that fails is logged and leaves the
//! flag and the mark as they were.
//!
//! A contest's own configs are listed with its contest problems' on the
//! contest's config issues, read when asked for; they set no flag, since no
//! judgement reads them.

use std::collections::{BTreeSet, HashSet};

use common::db::{DatabaseConnection, DatabaseTransaction};
use plugin_core::config_schema::{FieldError, FieldErrorKind};
use plugin_core::registry::PluginStatus;
use plugin_core::traits::PluginManager;
use sea_orm::sea_query::{Expr, ExprTrait, LockType};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect,
    TransactionTrait,
};

use crate::entity::{contest, contest_problem, plugin as plugin_entity, plugin_config, problem};
use crate::error::AppError;
use crate::host_funcs::config::{extract_plugin_id, strip_namespace_prefix};
use crate::plugin_lifecycle::Governs;
use crate::registry::TypeProviders;
use crate::services::plugin_config::{
    ConfigScope, ConfigTarget, Findings, IncompatibleConfig, Reads, check_config,
};
use crate::state::AppState;

/// Which way a recheck may move the flag; `Both` covers `ClearOnly`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Recheck {
    /// Only clear a set flag: the change cannot have added an issue.
    ClearOnly,
    /// Set or clear it to match the checks.
    Both,
}

/// What config checks run with: the plugins that check, and which of them
/// provides each type a problem or contest has.
pub(crate) struct ConfigChecker<'a> {
    pub(crate) plugins: &'a dyn PluginManager,
    pub(crate) types: TypeProviders,
}

impl<'a> ConfigChecker<'a> {
    pub(crate) fn of(state: &'a AppState) -> Self {
        Self {
            plugins: state.plugins.as_ref(),
            types: state.registries.type_providers(),
        }
    }
}

/// What a config issues listing covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Subject {
    /// A problem's configs at the problem scope and, with `in_contests`, in
    /// every contest that uses it.
    Problem { problem_id: i32, in_contests: bool },
    /// A contest's own configs and those of its problems in it.
    Contest(i32),
}

impl Subject {
    /// What a problem's `config_issues` flag records: its configs at the
    /// problem scope and in every contest that uses it.
    pub(crate) fn flag_of(problem_id: i32) -> Self {
        Self::Problem {
            problem_id,
            in_contests: true,
        }
    }
}

/// A target a listing checks, with the plugins loaded here that provide its
/// types: their namespaces that declare a `validate` function are checked
/// there with their defaults when no config is stored. `contest_types` and
/// `problem_types` are the target's types, by which a plugin not loaded here
/// that provides one is found ([`undecided_at`]).
struct Checked {
    target: ConfigTarget,
    providers: Vec<String>,
    contest_types: Vec<String>,
    problem_types: Vec<String>,
}

/// One config a listing checks: stored, or a namespace's defaults.
struct Check {
    target: ConfigTarget,
    plugin_id: String,
    namespace: String,
    config: serde_json::Value,
    stored: bool,
}

/// A config its plugin found something wrong with, and what.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConfigIssue {
    plugin_id: String,
    target: ConfigTarget,
    namespace: String,
    findings: Findings,
    /// Whether someone saved the config, rather than the namespace's
    /// defaults being checked where none is stored.
    stored: bool,
}

impl ConfigIssue {
    fn as_listed(&self, errors: Vec<FieldError>) -> IncompatibleConfig {
        IncompatibleConfig {
            plugin_id: self.plugin_id.clone(),
            scope: self.target.scope(),
            ref_id: self.target.ref_id().to_string(),
            namespace: self.namespace.clone(),
            errors,
        }
    }

    /// The issue as the API lists it: everything found.
    pub(crate) fn listed(&self) -> IncompatibleConfig {
        self.as_listed(self.findings.clone().into_all())
    }

    /// The issue as the API lists it when the config is one someone saved:
    /// what a change to the data it is checked against breaks.
    pub(crate) fn of_stored(&self) -> Option<IncompatibleConfig> {
        self.stored.then(|| self.listed())
    }
}

/// What the checks of a subject found here.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// The configs a plugin loaded here finds something wrong with.
    pub(crate) issues: Vec<ConfigIssue>,
    /// The plugins enabled fleet-wide but not loaded here whose checks reach
    /// the subject: what they would find is unknown here.
    pub(crate) undecided: BTreeSet<String>,
}

impl Outcome {
    /// Whether the subject has issues, as its flag records it: unknown reads
    /// as having issues.
    pub(crate) fn flagged(&self) -> bool {
        !self.issues.is_empty() || !self.undecided.is_empty()
    }

    /// The mark it leaves: the plugins a replica must load to decide, `None`
    /// once decided.
    fn mark(&self) -> Option<serde_json::Value> {
        (!self.undecided.is_empty()).then(|| serde_json::json!(self.undecided))
    }
}

/// Every config of `subject` its plugin finds something wrong with, as
/// things stand, as the API lists them ([`issues_of`]). A subject a plugin
/// not loaded here would check has no answer here: `PLUGIN_UNAVAILABLE`.
pub(crate) async fn config_issues<C: ConnectionTrait>(
    db: &C,
    checker: &ConfigChecker<'_>,
    subject: Subject,
    reads: Reads<'_>,
) -> Result<Vec<IncompatibleConfig>, AppError> {
    let outcome = issues_of(db, checker, subject, reads).await?;
    if !outcome.undecided.is_empty() {
        tracing::info!(
            ?subject,
            undecided = ?outcome.undecided,
            "config issues are unknown here: a plugin that checks them is not loaded"
        );
        return Err(AppError::PluginUnavailable);
    }
    Ok(outcome.issues.iter().map(ConfigIssue::listed).collect())
}

/// Every config of `subject` its plugin finds something wrong with, as
/// things stand, the plugins reading as `reads` says: each stored config of
/// a loaded plugin, and the defaults of each namespace with a `validate`
/// function of a plugin that provides the target's types and stores none
/// there; issues ordered by scope, target, plugin and namespace. The plugins
/// whose checks it cannot run here are [`Outcome::undecided`].
pub(crate) async fn issues_of<C: ConnectionTrait>(
    db: &C,
    checker: &ConfigChecker<'_>,
    subject: Subject,
    reads: Reads<'_>,
) -> Result<Outcome, AppError> {
    let targets = targets_of(db, &checker.types, subject).await?;
    let (checks, stored_by_unloaded) = checks_at(db, checker.plugins, &targets).await?;
    let undecided = undecided_at(db, checker.plugins, &targets, stored_by_unloaded).await?;
    let mut issues = Vec::new();
    for check in checks {
        let findings = match check_config(
            checker.plugins,
            &check.target,
            &check.plugin_id,
            &check.namespace,
            &check.config,
            reads,
        )
        .await
        {
            Ok(findings) => findings,
            Err(AppError::ConfigNamespaceNotFound(_)) => Findings {
                refusals: vec![FieldError::new(
                    "",
                    FieldErrorKind::NamespaceUndeclared {
                        scope: check.target.scope().as_str().into(),
                    },
                )],
                issues: Vec::new(),
            },
            Err(other) => return Err(other),
        };
        if findings.refusals.is_empty() && findings.issues.is_empty() {
            continue;
        }
        issues.push(ConfigIssue {
            plugin_id: check.plugin_id,
            target: check.target,
            namespace: check.namespace,
            findings,
            stored: check.stored,
        });
    }
    issues.sort_by(|a, b| {
        let key = |i: &ConfigIssue| {
            (
                i.target.scope().as_str(),
                i.target.ref_id().to_string(),
                i.plugin_id.clone(),
                i.namespace.clone(),
            )
        };
        key(a).cmp(&key(b))
    });
    Ok(Outcome { issues, undecided })
}

/// The targets `subject` covers. A deleted problem or contest has none, and
/// a contest that was deleted holds none of its problems' configs.
async fn targets_of<C: ConnectionTrait>(
    db: &C,
    types: &TypeProviders,
    subject: Subject,
) -> Result<Vec<Checked>, AppError> {
    let mut targets = Vec::new();
    match subject {
        Subject::Problem {
            problem_id,
            in_contests,
        } => {
            let Some(problem) = problem::Entity::find_by_id(problem_id)
                .filter(problem::Column::DeletedAt.is_null())
                .one(db)
                .await?
            else {
                return Ok(targets);
            };
            targets.push(Checked {
                target: ConfigTarget::problem(problem_id),
                providers: types
                    .of_problem(
                        &problem.problem_type,
                        &problem.checker_format,
                        &problem.default_contest_type,
                    )
                    .await,
                contest_types: vec![problem.default_contest_type.clone()],
                problem_types: vec![problem.problem_type.clone()],
            });
            if in_contests {
                let contests: Vec<i32> = contest_problem::Entity::find()
                    .select_only()
                    .column(contest_problem::Column::ContestId)
                    .filter(contest_problem::Column::ProblemId.eq(problem_id))
                    .into_tuple()
                    .all(db)
                    .await?;
                for contest in active_contests(db, contests).await? {
                    targets.push(Checked {
                        target: ConfigTarget::contest_problem(contest.id, problem_id),
                        providers: contest_type_provider(types, &contest).await,
                        contest_types: contest.contest_type.into_iter().collect(),
                        problem_types: Vec::new(),
                    });
                }
            }
        }
        Subject::Contest(contest_id) => {
            let Some(contest) = active_contests(db, [contest_id]).await?.pop() else {
                return Ok(targets);
            };
            let providers = contest_type_provider(types, &contest).await;
            let contest_types: Vec<String> = contest.contest_type.into_iter().collect();
            targets.push(Checked {
                target: ConfigTarget::contest(contest_id),
                providers: providers.clone(),
                contest_types: contest_types.clone(),
                problem_types: Vec::new(),
            });
            let problems: Vec<i32> = contest_problem::Entity::find()
                .select_only()
                .column(contest_problem::Column::ProblemId)
                .filter(contest_problem::Column::ContestId.eq(contest_id))
                .into_tuple()
                .all(db)
                .await?;
            for problem_id in problems {
                targets.push(Checked {
                    target: ConfigTarget::contest_problem(contest_id, problem_id),
                    providers: providers.clone(),
                    contest_types: contest_types.clone(),
                    problem_types: Vec::new(),
                });
            }
        }
    }
    Ok(targets)
}

async fn active_contests<C: ConnectionTrait>(
    db: &C,
    ids: impl IntoIterator<Item = i32>,
) -> Result<Vec<contest::Model>, AppError> {
    Ok(contest::Entity::find()
        .filter(contest::Column::Id.is_in(ids))
        .filter(contest::Column::DeletedAt.is_null())
        .all(db)
        .await?)
}

async fn contest_type_provider(types: &TypeProviders, contest: &contest::Model) -> Vec<String> {
    match &contest.contest_type {
        Some(contest_type) => types
            .of_contest_type(contest_type)
            .await
            .into_iter()
            .collect(),
        None => Vec::new(),
    }
}

/// The configs to check at `targets`: those stored there by a loaded
/// plugin, then the defaults of each namespace with a `validate` function at
/// the target's scope of a loaded plugin that provides the target's types,
/// where none is stored. Also the plugins not loaded here that store a
/// config at one of the targets.
async fn checks_at<C: ConnectionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    targets: &[Checked],
) -> Result<(Vec<Check>, BTreeSet<String>), AppError> {
    if targets.is_empty() {
        return Ok((Vec::new(), BTreeSet::new()));
    }
    let at = targets.iter().fold(Condition::any(), |any, checked| {
        any.add(
            Condition::all()
                .add(plugin_config::Column::Scope.eq(checked.target.scope().as_str()))
                .add(plugin_config::Column::RefId.eq(checked.target.ref_id())),
        )
    });
    let rows = plugin_config::Entity::find().filter(at).all(db).await?;

    let registry = plugins
        .get_registry()
        .read()
        .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?;
    let loaded = |plugin_id: &str| {
        registry
            .get(plugin_id)
            .filter(|entry| entry.status == PluginStatus::Loaded)
    };
    let mut stored = HashSet::new();
    let mut stored_by_unloaded = BTreeSet::new();
    let mut checks = Vec::new();
    for row in rows {
        let Some(checked) = targets.iter().find(|checked| {
            checked.target.scope().as_str() == row.scope && checked.target.ref_id() == row.ref_id
        }) else {
            continue;
        };
        stored.insert((row.scope.clone(), row.ref_id.clone(), row.namespace.clone()));
        let plugin_id = extract_plugin_id(&row.namespace);
        if loaded(plugin_id).is_none() {
            stored_by_unloaded.insert(plugin_id.to_string());
            continue;
        }
        checks.push(Check {
            target: checked.target.clone(),
            plugin_id: plugin_id.to_string(),
            namespace: strip_namespace_prefix(&row.namespace).to_string(),
            config: row.config,
            stored: true,
        });
    }
    for checked in targets {
        let scope = checked.target.scope();
        for plugin_id in &checked.providers {
            let Some(entry) = loaded(plugin_id) else {
                continue;
            };
            for (name, ns) in &entry.manifest.config {
                let key = (
                    scope.as_str().to_string(),
                    checked.target.ref_id().to_string(),
                    checked.target.stored_namespace(plugin_id, name),
                );
                if ns.validate.is_none() || !ns.scopes.contains(&scope) || stored.contains(&key) {
                    continue;
                }
                checks.push(Check {
                    target: checked.target.clone(),
                    plugin_id: plugin_id.clone(),
                    namespace: name.clone(),
                    config: ns.defaults(),
                    stored: false,
                });
            }
        }
    }
    Ok((checks, stored_by_unloaded))
}

/// The plugins whose checks reach `targets` but cannot run here: enabled
/// fleet-wide and not loaded here, and either storing a config at a target
/// (`stored_by_unloaded`) or providing a target's type, as the replica that
/// last loaded them recorded (`plugin.governs`). A provider whose manifest
/// this replica knows is counted only when it declares a namespace with a
/// `validate` function at the target's scope, as a loaded one is checked.
async fn undecided_at<C: ConnectionTrait>(
    db: &C,
    plugins: &dyn PluginManager,
    targets: &[Checked],
    stored_by_unloaded: BTreeSet<String>,
) -> Result<BTreeSet<String>, AppError> {
    if targets.is_empty() {
        return Ok(BTreeSet::new());
    }
    let enabled: Vec<(String, Option<serde_json::Value>)> = plugin_entity::Entity::find()
        .select_only()
        .columns([plugin_entity::Column::Id, plugin_entity::Column::Governs])
        .filter(plugin_entity::Column::IsEnabled.eq(true))
        .into_tuple()
        .all(db)
        .await?;
    let registry = plugins
        .get_registry()
        .read()
        .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?;
    let mut undecided = BTreeSet::new();
    for (plugin_id, governs) in enabled {
        let entry = registry.get(&plugin_id);
        if entry.is_some_and(|entry| entry.status == PluginStatus::Loaded) {
            continue;
        }
        if stored_by_unloaded.contains(&plugin_id) {
            undecided.insert(plugin_id);
            continue;
        }
        let governs: Governs = governs
            .and_then(|g| serde_json::from_value(g).ok())
            .unwrap_or_default();
        let checks_at = |scope: ConfigScope| {
            entry.is_none_or(|entry| {
                entry
                    .manifest
                    .config
                    .values()
                    .any(|ns| ns.validate.is_some() && ns.scopes.contains(&scope))
            })
        };
        let provides = targets.iter().any(|t| {
            let of_its_types = t
                .contest_types
                .iter()
                .any(|key| governs.provides.contest_types.contains(key))
                || t.problem_types
                    .iter()
                    .any(|key| governs.provides.problem_types.contains(key));
            of_its_types && checks_at(t.target.scope())
        });
        if provides {
            undecided.insert(plugin_id);
        }
    }
    Ok(undecided)
}

/// The problems the checks of `plugin_ids` (of every plugin for `None`)
/// reach, those whose flag a change to these plugins or what they read can
/// move: each with a stored config of one of them, at the problem scope or
/// in a contest, and each a plugin's defaults are checked at, a problem of a
/// type it provides or in a contest of a contest type it provides, where
/// the plugin declares a namespace with a `validate` function at that scope.
pub(crate) async fn problems_checked_by<C: ConnectionTrait>(
    db: &C,
    checker: &ConfigChecker<'_>,
    plugin_ids: Option<&[String]>,
) -> Result<BTreeSet<i32>, AppError> {
    let rows: Vec<(String, String, String)> = plugin_config::Entity::find()
        .select_only()
        .columns([
            plugin_config::Column::Scope,
            plugin_config::Column::RefId,
            plugin_config::Column::Namespace,
        ])
        .filter(plugin_config::Column::Scope.is_in([
            ConfigScope::Problem.as_str(),
            ConfigScope::ContestProblem.as_str(),
        ]))
        .into_tuple()
        .all(db)
        .await?;
    let named = |plugin_id: &str| plugin_ids.is_none_or(|ids| ids.iter().any(|id| id == plugin_id));
    let mut problems: BTreeSet<i32> = rows
        .into_iter()
        .filter(|(_, _, namespace)| named(extract_plugin_id(namespace)))
        .filter_map(|(scope, ref_id, _)| ConfigTarget::of_row(&scope, &ref_id)?.problem_id())
        .collect();

    // The loaded plugins with a namespace that has a `validate` function at
    // the problem scope, and at the contest-problem scope.
    let (at_problem, at_contest_problem) = {
        let registry = checker
            .plugins
            .get_registry()
            .read()
            .map_err(|_| AppError::Internal("plugin registry lock poisoned".into()))?;
        let mut at_problem = Vec::new();
        let mut at_contest_problem = Vec::new();
        for (plugin_id, entry) in registry.iter() {
            if entry.status != PluginStatus::Loaded || !named(plugin_id) {
                continue;
            }
            let validates_at = |scope: ConfigScope| {
                entry
                    .manifest
                    .config
                    .values()
                    .any(|ns| ns.validate.is_some() && ns.scopes.contains(&scope))
            };
            if validates_at(ConfigScope::Problem) {
                at_problem.push(plugin_id.clone());
            }
            if validates_at(ConfigScope::ContestProblem) {
                at_contest_problem.push(plugin_id.clone());
            }
        }
        (at_problem, at_contest_problem)
    };
    for plugin_id in at_problem {
        let provided = checker.types.provided_by(&plugin_id).await;
        let of_its_types: Vec<i32> = problem::Entity::find()
            .select_only()
            .column(problem::Column::Id)
            .filter(problem::Column::DeletedAt.is_null())
            .filter(
                Condition::any()
                    .add(problem::Column::ProblemType.is_in(provided.problem_types))
                    .add(problem::Column::CheckerFormat.is_in(provided.checker_formats))
                    .add(problem::Column::DefaultContestType.is_in(provided.contest_types)),
            )
            .into_tuple()
            .all(db)
            .await?;
        problems.extend(of_its_types);
    }
    for plugin_id in at_contest_problem {
        let contest_types = checker.types.provided_by(&plugin_id).await.contest_types;
        if contest_types.is_empty() {
            continue;
        }
        let in_its_contests: Vec<i32> = contest_problem::Entity::find()
            .select_only()
            .column(contest_problem::Column::ProblemId)
            .inner_join(contest::Entity)
            .filter(contest::Column::DeletedAt.is_null())
            .filter(contest::Column::ContestType.is_in(contest_types))
            .into_tuple()
            .all(db)
            .await?;
        problems.extend(in_its_contests);
    }
    Ok(problems)
}

/// Records what the checks of problem `problem_id` found, in `txn`: its
/// flag, and the mark an undecided outcome leaves for a replica that can
/// decide. The one writer of both.
pub(crate) async fn record_outcome(
    txn: &DatabaseTransaction,
    problem_id: i32,
    outcome: &Outcome,
) -> Result<(), AppError> {
    problem::Entity::update_many()
        .col_expr(
            problem::Column::ConfigIssues,
            Expr::value(outcome.flagged()),
        )
        .col_expr(problem::Column::ConfigRecheck, Expr::value(outcome.mark()))
        .filter(problem::Column::Id.eq(problem_id))
        .exec(txn)
        .await?;
    Ok(())
}

/// The problems whose flag is set.
async fn flagged(db: &DatabaseConnection) -> Result<Vec<i32>, AppError> {
    Ok(problem::Entity::find()
        .select_only()
        .column(problem::Column::Id)
        .filter(problem::Column::ConfigIssues.eq(true))
        .into_tuple()
        .all(db)
        .await?)
}

/// The problems marked for a recheck that a replica loading `loaded` can
/// decide (every problem marked, for `None`): those whose mark names no
/// plugin outside `loaded`.
async fn marked(db: &DatabaseConnection, loaded: Option<&[String]>) -> Result<Vec<i32>, AppError> {
    let mut query = problem::Entity::find()
        .select_only()
        .column(problem::Column::Id)
        .filter(problem::Column::ConfigRecheck.is_not_null());
    if let Some(loaded) = loaded {
        query = query.filter(
            Expr::col((problem::Entity, problem::Column::ConfigRecheck)).binary(
                sea_orm::sea_query::BinOper::Custom("<@"),
                Expr::val(serde_json::json!(loaded)).cast_as("jsonb"),
            ),
        );
    }
    Ok(query.into_tuple().all(db).await?)
}

/// The plugins loaded here.
fn loaded_here(plugins: &dyn PluginManager) -> Vec<String> {
    plugins
        .get_registry()
        .read()
        .map(|registry| {
            registry
                .iter()
                .filter(|(_, entry)| entry.status == PluginStatus::Loaded)
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Rechecks the flag of every problem the checks of `plugin_ids` reach, and
/// of every flagged problem, after a change to those plugins: a disabled
/// plugin no longer provides the types its checks reached.
pub(crate) async fn recheck_problems_of_plugins(
    db: &DatabaseConnection,
    checker: &ConfigChecker<'_>,
    plugin_ids: &[&str],
) {
    let plugin_ids: Vec<String> = plugin_ids.iter().map(|id| id.to_string()).collect();
    let problems = async {
        let mut problems = problems_checked_by(db, checker, Some(&plugin_ids)).await?;
        problems.extend(flagged(db).await?);
        Ok::<_, AppError>(problems)
    };
    match problems.await {
        Ok(problems) => {
            recheck_problems(db, checker, problems, Recheck::Both, Reads::Committed).await
        }
        Err(error) => tracing::warn!(
            ?plugin_ids,
            ?error,
            "could not find the problems whose config issues a plugin change affects"
        ),
    }
}

/// Rechecks the flag of every problem a check reaches, is flagged or is
/// marked, at a server start.
pub(crate) async fn recheck_every_problem(db: &DatabaseConnection, checker: &ConfigChecker<'_>) {
    let problems = async {
        let mut problems = problems_checked_by(db, checker, None).await?;
        problems.extend(flagged(db).await?);
        problems.extend(marked(db, None).await?);
        Ok::<_, AppError>(problems)
    };
    match problems.await {
        Ok(problems) => {
            recheck_problems(db, checker, problems, Recheck::Both, Reads::Committed).await
        }
        Err(error) => tracing::warn!(
            ?error,
            "could not find the problems whose config issues a server start rechecks"
        ),
    }
}

/// Decides every marked problem this replica can: one whose mark names only
/// plugins loaded here. Every running server runs it on each reconcile pass;
/// a replica that cannot decide a mark leaves it alone, and a problem
/// another server is deciding is skipped, not decided twice. Deciding
/// clears the mark, or replaces it when another plugin is missing here now.
pub async fn recheck_marked(state: &AppState) {
    let checker = ConfigChecker::of(state);
    let loaded = loaded_here(checker.plugins);
    match marked(&state.db, Some(&loaded)).await {
        Ok(problems) => {
            for problem_id in problems {
                if let Err(error) = try_recheck(
                    &state.db,
                    &checker,
                    problem_id,
                    Claim::Mark,
                    Reads::Committed,
                )
                .await
                {
                    tracing::warn!(
                        problem_id,
                        ?error,
                        "could not decide the problem's marked config issues"
                    );
                }
            }
        }
        Err(error) => tracing::warn!(
            ?error,
            "could not find the problems marked for a config issues recheck"
        ),
    }
}

/// Rechecks the flag of each of `problems`, moving it as `recheck` allows,
/// the plugins reading as `reads` says.
pub(crate) async fn recheck_problems(
    db: &DatabaseConnection,
    checker: &ConfigChecker<'_>,
    problems: BTreeSet<i32>,
    recheck: Recheck,
    reads: Reads<'_>,
) {
    for problem_id in problems {
        if let Err(error) =
            try_recheck(db, checker, problem_id, Claim::Change(recheck), reads).await
        {
            tracing::warn!(
                problem_id,
                ?error,
                "could not recheck the problem's config issues"
            );
        }
    }
}

/// Why a recheck runs, which decides how it takes the problem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    /// A change reached the problem: wait for its row lock, and move the
    /// flag as the [`Recheck`] allows.
    Change(Recheck),
    /// The problem is marked: claim it, skipping it when another server
    /// holds it, and decide it only if it is still marked.
    Mark,
}

/// Rechecks one problem under its row lock, the lock a test-data change and
/// a write of one of its configs hold, so a recheck never races the data it
/// checks against. An undecided outcome only ever sets the flag and the mark
/// ([`Outcome::flagged`]); a decided one replaces both. A marked problem this
/// replica cannot decide keeps the plugins its mark named, so the mark only
/// grows until one replica loads every plugin it takes.
async fn try_recheck(
    db: &DatabaseConnection,
    checker: &ConfigChecker<'_>,
    problem_id: i32,
    claim: Claim,
    reads: Reads<'_>,
) -> Result<(), AppError> {
    let txn = db.begin().await?;
    let lock = match claim {
        Claim::Change(_) => problem::Entity::find_by_id(problem_id).lock(LockType::Update),
        Claim::Mark => problem::Entity::find_by_id(problem_id).lock_with_behavior(
            LockType::Update,
            sea_orm::sea_query::LockBehavior::SkipLocked,
        ),
    };
    let Some(row) = lock.one(&txn).await? else {
        return Ok(());
    };
    match claim {
        Claim::Change(Recheck::ClearOnly) if !row.config_issues => return Ok(()),
        Claim::Mark if row.config_recheck.is_none() => return Ok(()),
        _ => {}
    }
    let mut outcome = issues_of(&txn, checker, Subject::flag_of(problem_id), reads).await?;
    if claim == Claim::Mark && !outcome.undecided.is_empty() {
        // The plugins of the mark this replica claimed are loaded here, and
        // deciding still takes them: a replica that loads only the plugins
        // missing here would leave a mark naming these, so the two would hand
        // the problem back and forth. The mark names both, for a replica that
        // loads them all.
        let claimed: BTreeSet<String> = row
            .config_recheck
            .as_ref()
            .and_then(|mark| serde_json::from_value(mark.clone()).ok())
            .unwrap_or_default();
        outcome.undecided.extend(claimed);
    }
    let record = match claim {
        // A change that can only resolve issues never sets the flag.
        Claim::Change(Recheck::ClearOnly) => !outcome.flagged() || outcome.mark().is_some(),
        _ => true,
    };
    if record && (outcome.flagged(), outcome.mark()) != (row.config_issues, row.config_recheck) {
        record_outcome(&txn, problem_id, &outcome).await?;
        txn.commit().await?;
    }
    Ok(())
}
