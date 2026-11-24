//! What plugins provide, and what a unit of judging work needs of them.
//!
//! A plugin provides keys of three kinds ([`Provided`]): contest types it
//! judges, problem types it evaluates, checker formats it resolves. A
//! judgement or code run needs one key of each: its contest type, its
//! problem's type and its problem's checker format ([`Needs`]). What one
//! plugin registered is a [`Provides`] (its `plugin.governs`, see
//! `plugin_lifecycle::Governs`); what a replica has loaded is a [`Loaded`],
//! each key with the plugins that registered it there ([`Loaded::here`],
//! read from its registries, and each live replica's
//! `plugin_replica.provides`).
//!
//! One rule resolves a key: to its fleet-wide provider, the plugin with the
//! first id among the enabled ones that provide it
//! (`PluginLifecycle::provider`, and [`provider`] in SQL, over the rows
//! `plugin.is_enabled` and `plugin.governs`). A key no plugin ever recorded
//! resolves among the plugins loaded here. The claim, the dispatch
//! ([`resolve`]) and the parking rule all read it:
//!
//! - a unit is runnable while every key it needs has a provider enabled
//!   fleet-wide (`runnable`); work that is not is parked;
//! - a replica takes a unit only when it has every key's fleet-wide provider
//!   loaded, or no live replica has (then the replica that takes it fails it
//!   visibly instead of it waiting unseen) (`runs_here`);
//! - the dispatch runs each key on its fleet-wide provider only, so one
//!   contest is never judged by two plugins under two configs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::state::RegistryState;

pub use broccoli_api_types::visibility::Provided;

/// Every kind of key, in the order [`Needs::each`] lists a unit's needs.
pub const KINDS: [Provided; 3] = [
    Provided::ContestType,
    Provided::ProblemType,
    Provided::CheckerFormat,
];

/// The [`Provides`] field, and JSON key, that lists the keys of `kind`.
pub(crate) fn field(kind: Provided) -> &'static str {
    match kind {
        Provided::ContestType => "contest_types",
        Provided::ProblemType => "problem_types",
        Provided::CheckerFormat => "checker_formats",
    }
}

/// The keys of each kind some plugins provide, each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    pub contest_types: Vec<String>,
    pub problem_types: Vec<String>,
    pub checker_formats: Vec<String>,
}

impl Provides {
    pub fn keys(&self, kind: Provided) -> &[String] {
        match kind {
            Provided::ContestType => &self.contest_types,
            Provided::ProblemType => &self.problem_types,
            Provided::CheckerFormat => &self.checker_formats,
        }
    }

    pub fn provides(&self, kind: Provided, key: &str) -> bool {
        self.keys(kind).iter().any(|k| k == key)
    }

    /// What `plugin_id` registered in this replica's registries.
    pub async fn registered_by(registries: &RegistryState, plugin_id: &str) -> Self {
        let loaded = Loaded::here(registries).await;
        let keys = |map: &BTreeMap<String, Vec<String>>| -> Vec<String> {
            map.iter()
                .filter(|(_, plugins)| plugins.iter().any(|p| p == plugin_id))
                .map(|(key, _)| key.clone())
                .collect()
        };
        Self {
            contest_types: keys(&loaded.contest_types),
            problem_types: keys(&loaded.problem_types),
            checker_formats: keys(&loaded.checker_formats),
        }
    }
}

/// What one replica has loaded: for each kind, every key with the plugins
/// that registered it there, ids sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Loaded {
    pub contest_types: BTreeMap<String, Vec<String>>,
    pub problem_types: BTreeMap<String, Vec<String>>,
    pub checker_formats: BTreeMap<String, Vec<String>>,
}

impl Loaded {
    /// What this replica has loaded: every registration its registries hold.
    pub async fn here(registries: &RegistryState) -> Self {
        macro_rules! keys {
            ($registry:ident) => {{
                let registry = registries.$registry.read().await;
                registry
                    .keys()
                    .map(|key| (key.clone(), registry.plugins_of(key)))
                    .collect()
            }};
        }
        Self {
            contest_types: keys!(contest_type_registry),
            problem_types: keys!(evaluator_registry),
            checker_formats: keys!(checker_stage_registry),
        }
    }

    fn of(&self, kind: Provided) -> &BTreeMap<String, Vec<String>> {
        match kind {
            Provided::ContestType => &self.contest_types,
            Provided::ProblemType => &self.problem_types,
            Provided::CheckerFormat => &self.checker_formats,
        }
    }

    /// Whether `plugin_id` registered `key` of `kind` here.
    pub fn has(&self, kind: Provided, key: &str, plugin_id: &str) -> bool {
        self.of(kind)
            .get(key)
            .is_some_and(|plugins| plugins.iter().any(|p| p == plugin_id))
    }

    /// Whether any plugin registered `key` of `kind` here.
    pub fn has_key(&self, kind: Provided, key: &str) -> bool {
        self.of(kind).contains_key(key)
    }
}

/// Who resolves a key fleet-wide (`PluginLifecycle::provider`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetProvider {
    /// The enabled provider whose id sorts first.
    Plugin(String),
    /// Plugins recorded the key, and every one is disabled.
    Closed,
    /// No plugin ever recorded the key: it resolves among the plugins loaded
    /// here.
    Unknown,
}

/// Why a key did not resolve to a handler here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unresolved {
    pub kind: Provided,
    pub key: String,
    /// The key's fleet-wide provider, when it has one: not loaded here.
    pub provider: Option<String>,
}

impl std::fmt::Display for Unresolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let role = role(self.kind);
        match &self.provider {
            Some(plugin_id) => write!(
                f,
                "{role} plugin {plugin_id} for {} {:?} is not loaded on this replica",
                noun(self.kind),
                self.key
            ),
            None => write!(
                f,
                "no {role} plugin provides {} {:?}",
                noun(self.kind),
                self.key
            ),
        }
    }
}

/// What a plugin providing a key of `kind` is to the unit.
pub fn role(kind: Provided) -> &'static str {
    match kind {
        Provided::ContestType => "the contest",
        Provided::ProblemType => "the evaluator",
        Provided::CheckerFormat => "the checker",
    }
}

fn noun(kind: Provided) -> &'static str {
    match kind {
        Provided::ContestType => "contest type",
        Provided::ProblemType => "problem type",
        Provided::CheckerFormat => "checker format",
    }
}

/// The handler `key` (a `kind` of key) resolves to here: its fleet-wide
/// provider's registration (`fleet`), or, for a key no plugin ever recorded,
/// the first loaded here. The one place a dispatch picks a handler.
pub fn resolve<H: crate::registry::Provided>(
    registry: &crate::registry::Providers<H>,
    fleet: FleetProvider,
    kind: Provided,
    key: &str,
) -> Result<H, Unresolved> {
    let unresolved = |provider| Unresolved {
        kind,
        key: key.to_string(),
        provider,
    };
    match fleet {
        FleetProvider::Plugin(plugin_id) => registry
            .registration(key, &plugin_id)
            .cloned()
            .ok_or_else(|| unresolved(Some(plugin_id))),
        FleetProvider::Closed => Err(unresolved(None)),
        FleetProvider::Unknown => registry.get(key).cloned().ok_or_else(|| unresolved(None)),
    }
}

/// The keys one unit of work needs a provider of, as Rust values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs<'a> {
    pub contest_type: &'a str,
    pub problem_type: &'a str,
    pub checker_format: &'a str,
}

impl<'a> Needs<'a> {
    /// The needs of work of `contest_type` on `problem`.
    pub fn of(contest_type: &'a str, problem: &'a crate::entity::problem::Model) -> Self {
        Self {
            contest_type,
            problem_type: &problem.problem_type,
            checker_format: &problem.checker_format,
        }
    }

    /// Each need with its kind, in [`KINDS`] order.
    pub fn each(self) -> [(Provided, &'a str); 3] {
        [
            (Provided::ContestType, self.contest_type),
            (Provided::ProblemType, self.problem_type),
            (Provided::CheckerFormat, self.checker_format),
        ]
    }
}

/// SQL: every key of `needs` (each an SQL expression, with its kind) has a
/// provider enabled fleet-wide, or no plugin ever recorded it (an unknown key
/// is not disabled, so whoever takes the work fails it visibly). The SQL
/// form of `PluginLifecycle::closed_by`, over the rows it reads
/// (`plugin.is_enabled`, `plugin.governs`).
pub(crate) fn runnable(needs: [(Provided, &str); 3]) -> String {
    let each: Vec<String> = needs
        .iter()
        .map(|(kind, key)| {
            format!(
                "COALESCE((SELECT bool_or(p.is_enabled) FROM plugin p \
                            WHERE p.governs->'{field}' @> jsonb_build_array({key})), TRUE)",
                field = field(*kind),
            )
        })
        .collect();
    format!("({})", each.join(" AND "))
}

/// SQL: the fleet-wide provider of `key` (an SQL expression) of `kind`: the
/// enabled plugin providing it whose id sorts first, bytewise as Rust sorts
/// it; NULL when none is enabled. The SQL form of
/// `PluginLifecycle::provider`.
pub(crate) fn provider(kind: Provided, key: &str) -> String {
    format!(
        "(SELECT min(p.id COLLATE \"C\") FROM plugin p \
           WHERE p.is_enabled AND p.governs->'{field}' @> jsonb_build_array({key}))",
        field = field(kind),
    )
}

/// SQL: this replica may take work with `needs` (each an SQL expression,
/// with its kind): for every key, this replica has its fleet-wide
/// [`provider`] loaded (`local`, the placeholder [`Loaded::here`] is bound
/// to), or no live replica has (`plugin_replica.provides`, live by its
/// heartbeat on the database clock). A key no plugin provides fleet-wide
/// goes by whether any plugin loaded it.
pub(crate) fn runs_here(needs: [(Provided, &str); 3], local: &str) -> String {
    let each: Vec<String> = needs
        .iter()
        .map(|(kind, key)| {
            format!(
                "({here} OR NOT EXISTS (\
                   SELECT 1 FROM plugin_replica r \
                    WHERE r.heartbeat_at > NOW() - make_interval(secs => {live}) \
                      AND {there}))",
                here = loads(*kind, key, local),
                there = loads(*kind, key, "r.provides"),
                live = crate::plugin_fleet::LIVE_WINDOW_SECS,
            )
        })
        .collect();
    format!("({})", each.join(" AND "))
}

/// SQL: `loaded` has `key` of `kind` loaded on its fleet-wide provider, or,
/// for a key none provides fleet-wide, on any plugin.
fn loads(kind: Provided, key: &str, loaded: &str) -> String {
    format!(
        "COALESCE(CASE WHEN {provider} IS NULL \
                       THEN ({loaded})->'{field}' ? ({key}) \
                       ELSE ({loaded})->'{field}'->({key}) ? {provider} END, FALSE)",
        provider = provider(kind, key),
        field = field(kind),
    )
}
