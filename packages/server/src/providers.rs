//! What plugins provide, and what a unit of judging work needs of them.
//!
//! A plugin provides keys of three kinds ([`Provided`]): contest types it
//! judges, problem types it evaluates, checker formats it resolves. A
//! judgement or code run needs one key of each: its contest type, its
//! problem's type and its problem's checker format ([`Needs`]). The same
//! record of keys ([`Provides`]) says what one plugin registered (its
//! `plugin.governs`, see `plugin_lifecycle::Governs`), what this replica has
//! loaded ([`Provides::loaded_here`], read from its registries), and what each
//! live replica has loaded (its `plugin_replica.provides`).
//!
//! Two rules read them, alike for every kind:
//!
//! - a unit is runnable while every key it needs has a provider enabled
//!   fleet-wide (`runnable`); work that is not is parked;
//! - a replica takes a unit only when it has every key the unit needs loaded,
//!   or no live replica has that key loaded (then the replica that takes it
//!   fails it visibly instead of it waiting unseen) (`runs_here`).

use serde::{Deserialize, Serialize};

use crate::state::RegistryState;

/// A kind of key plugins provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provided {
    /// A contest type: its plugin judges the unit.
    ContestType,
    /// A problem type: its evaluator plugin runs the unit's test cases.
    ProblemType,
    /// A checker format: its checker plugin checks the unit's output.
    CheckerFormat,
}

impl Provided {
    pub const ALL: [Provided; 3] = [Self::ContestType, Self::ProblemType, Self::CheckerFormat];

    /// The [`Provides`] field, and JSON key, that lists the keys of this kind.
    pub(crate) fn field(self) -> &'static str {
        match self {
            Self::ContestType => "contest_types",
            Self::ProblemType => "problem_types",
            Self::CheckerFormat => "checker_formats",
        }
    }
}

/// The keys of each kind some plugins provide, each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    #[serde(default)]
    pub contest_types: Vec<String>,
    #[serde(default)]
    pub problem_types: Vec<String>,
    #[serde(default)]
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

    /// What this replica has loaded: every key its registries hold.
    pub async fn loaded_here(registries: &RegistryState) -> Self {
        Self::registered(registries, None).await
    }

    /// What `plugin_id` registered in this replica's registries.
    pub async fn registered_by(registries: &RegistryState, plugin_id: &str) -> Self {
        Self::registered(registries, Some(plugin_id)).await
    }

    async fn registered(registries: &RegistryState, plugin_id: Option<&str>) -> Self {
        fn sorted(mut keys: Vec<String>) -> Vec<String> {
            keys.sort();
            keys
        }
        macro_rules! keys {
            ($registry:ident) => {{
                let registry = registries.$registry.read().await;
                sorted(match plugin_id {
                    Some(id) => registry
                        .registered_by(id)
                        .into_iter()
                        .map(|(k, _)| k)
                        .collect(),
                    None => registry.keys().cloned().collect(),
                })
            }};
        }
        Self {
            contest_types: keys!(contest_type_registry),
            problem_types: keys!(evaluator_registry),
            checker_formats: keys!(checker_stage_registry),
        }
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

    /// Each need with its kind, in [`Provided::ALL`] order.
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
                field = kind.field(),
            )
        })
        .collect();
    format!("({})", each.join(" AND "))
}

/// SQL: this replica may take work with `needs` (each an SQL expression,
/// with its kind): for every key, this replica has it loaded (`local`, the
/// placeholder each kind's list of [`Provides::loaded_here`] is bound to), or
/// no live replica has it loaded (`plugin_replica.provides`, live by its
/// heartbeat on the database clock).
pub(crate) fn runs_here(needs: [(Provided, &str); 3], local: &LocalPlaceholders) -> String {
    let each: Vec<String> = needs
        .iter()
        .map(|(kind, key)| {
            format!(
                "({key} = ANY({local}) OR NOT EXISTS (\
                   SELECT 1 FROM plugin_replica r \
                    WHERE r.heartbeat_at > NOW() - make_interval(secs => {live}) \
                      AND r.provides->'{field}' @> jsonb_build_array({key})))",
                local = local.of(*kind),
                field = kind.field(),
                live = crate::plugin_fleet::LIVE_WINDOW_SECS,
            )
        })
        .collect();
    format!("({})", each.join(" AND "))
}

/// The placeholders a statement bound each kind of this replica's loaded keys
/// to (see [`runs_here`]).
pub(crate) struct LocalPlaceholders([String; 3]);

impl LocalPlaceholders {
    /// Binds each kind of `local` with `bind`, which returns the placeholder
    /// of the value it bound.
    pub(crate) fn bind(local: &Provides, mut bind: impl FnMut(Vec<String>) -> String) -> Self {
        Self(Provided::ALL.map(|kind| bind(local.keys(kind).to_vec())))
    }

    fn of(&self, kind: Provided) -> &str {
        let index = Provided::ALL
            .iter()
            .position(|k| *k == kind)
            .expect("every kind is in ALL");
        &self.0[index]
    }
}
