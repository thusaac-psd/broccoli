//! What plugins provide, and what a unit of judging work needs of them.
//!
//! A plugin provides keys of four kinds ([`Provided`]): contest types it
//! judges, problem types it evaluates, checker formats it resolves and
//! languages it resolves. A judgement or code run needs exactly the plugins
//! it uses ([`Needs`]): its contest type's, its problem type's evaluator,
//! its language's resolver, and its problem's checker format's checker when
//! its evaluator declared that it uses a checker (`uses_checker` at
//! registration). What one plugin registered is a [`Provides`] (its
//! `plugin.governs`, see `plugin_lifecycle::Governs`); what a replica has
//! loaded is a [`Loaded`], each key with the plugins that registered it
//! there ([`Loaded::here`], read from its registries, and each live
//! replica's `plugin_replica.provides`).
//!
//! One rule resolves a key: to its fleet-wide provider, the plugin with the
//! first id among the enabled ones that provide it
//! (`PluginLifecycle::provider`, and `provider` in SQL, over the rows
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
pub const KINDS: [Provided; 4] = [
    Provided::ContestType,
    Provided::ProblemType,
    Provided::CheckerFormat,
    Provided::Language,
];

/// The [`Provides`] field, and JSON key, that lists the keys of `kind`.
pub(crate) fn field(kind: Provided) -> &'static str {
    match kind {
        Provided::ContestType => "contest_types",
        Provided::ProblemType => "problem_types",
        Provided::CheckerFormat => "checker_formats",
        Provided::Language | Provided::CheckerLanguage => "languages",
    }
}

/// The keys of each kind some plugins provide, each list sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provides {
    pub contest_types: Vec<String>,
    pub problem_types: Vec<String>,
    pub checker_formats: Vec<String>,
    pub languages: Vec<String>,
}

impl Provides {
    pub fn keys(&self, kind: Provided) -> &[String] {
        match kind {
            Provided::ContestType => &self.contest_types,
            Provided::ProblemType => &self.problem_types,
            Provided::CheckerFormat => &self.checker_formats,
            Provided::Language | Provided::CheckerLanguage => &self.languages,
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
            languages: keys(&loaded.languages),
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
    pub languages: BTreeMap<String, Vec<String>>,
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
            languages: keys!(language_resolver_registry),
        }
    }

    /// The keys of `kind` with the plugins that registered each.
    pub fn of(&self, kind: Provided) -> &BTreeMap<String, Vec<String>> {
        match kind {
            Provided::ContestType => &self.contest_types,
            Provided::ProblemType => &self.problem_types,
            Provided::CheckerFormat => &self.checker_formats,
            Provided::Language | Provided::CheckerLanguage => &self.languages,
        }
    }

    /// The keys of `kind` with the plugins that registered each, to change.
    pub fn of_mut(&mut self, kind: Provided) -> &mut BTreeMap<String, Vec<String>> {
        match kind {
            Provided::ContestType => &mut self.contest_types,
            Provided::ProblemType => &mut self.problem_types,
            Provided::CheckerFormat => &mut self.checker_formats,
            Provided::Language | Provided::CheckerLanguage => &mut self.languages,
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

    /// Adds every registration `other` has, each key's plugin ids kept
    /// sorted and once.
    pub fn merge(&mut self, other: &Loaded) {
        for kind in KINDS {
            let mine = self.of_mut(kind);
            for (key, plugins) in other.of(kind) {
                let ids = mine.entry(key.clone()).or_default();
                ids.extend(plugins.iter().cloned());
                ids.sort();
                ids.dedup();
            }
        }
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
                "the {role} plugin {plugin_id} for {} {:?} is not loaded on this replica",
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
        Provided::ContestType => "contest",
        Provided::ProblemType => "evaluator",
        Provided::CheckerFormat => "checker",
        Provided::Language => "language",
        Provided::CheckerLanguage => "checker's language",
    }
}

fn noun(kind: Provided) -> &'static str {
    match kind {
        Provided::ContestType => "contest type",
        Provided::ProblemType => "problem type",
        Provided::CheckerFormat => "checker format",
        Provided::Language | Provided::CheckerLanguage => "language",
    }
}

/// The plugin that owns `key` (by `fleet`, its fleet-wide provider), loaded
/// here or not: that provider; nobody for a key every provider of which is
/// disabled; for a key no plugin ever recorded, the first provider loaded
/// here. What judging runs a key on ([`resolve`]) and what owns a key's
/// configs, issues and hooks agree by this one rule.
pub fn provider_id<H: crate::registry::Provided>(
    registry: &crate::registry::Providers<H>,
    fleet: FleetProvider,
    key: &str,
) -> Option<String> {
    match fleet {
        FleetProvider::Plugin(plugin_id) => Some(plugin_id),
        FleetProvider::Closed => None,
        FleetProvider::Unknown => registry.get(key).map(|h| h.plugin_id().to_string()),
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

/// The keys one unit of work needs a provider of, as Rust values: exactly
/// the plugins it uses. `PluginLifecycle::needs` builds it, deciding the
/// checker by what the problem type's fleet-wide evaluator declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Needs<'a> {
    pub contest_type: &'a str,
    pub problem_type: &'a str,
    /// The problem's checker format, when its evaluator uses a checker.
    pub checker_format: Option<&'a str>,
    /// The unit's language, when there is a unit (a problem alone has
    /// none).
    pub language: Option<&'a str>,
    /// The language the problem's checker source compiles in
    /// (`providers::checker_language_of`), when the checker format is a need and its
    /// fleet-wide provider declares its checker compiles: the checker
    /// resolves it to compile its source, so a closed one parks the work
    /// as the unit's own language would.
    pub checker_language: Option<&'a str>,
}

impl<'a> Needs<'a> {
    /// Each need with its kind, in [`KINDS`] order.
    pub fn each(self) -> Vec<(Provided, &'a str)> {
        [
            (Provided::ContestType, Some(self.contest_type)),
            (Provided::ProblemType, Some(self.problem_type)),
            (Provided::CheckerFormat, self.checker_format),
            (Provided::Language, self.language),
            (Provided::CheckerLanguage, self.checker_language),
        ]
        .into_iter()
        .filter_map(|(kind, key)| Some((kind, key?)))
        .collect()
    }
}

/// The needs of one unit as SQL expressions over its row, each with its
/// kind, in [`KINDS`] order: [`Needs`] in SQL. The checker format is NULL
/// (no need) when the problem type's fleet-wide evaluator declared it uses
/// no checker.
pub(crate) fn sql_needs(
    contest_type: &str,
    problem_type: &str,
    checker_format: &str,
    language: &str,
    problem_id: &str,
) -> Vec<(Provided, String)> {
    let checker_format = format!(
        "(CASE WHEN EXISTS (SELECT 1 FROM plugin e \
                             WHERE e.id = {evaluator} \
                               AND e.governs->'unchecked_problem_types' \
                                   @> jsonb_build_array({problem_type})) \
               THEN NULL ELSE {checker_format} END)",
        evaluator = provider(Provided::ProblemType, problem_type),
    );
    let checker_language = checker_language_sql(problem_id, &checker_format);
    vec![
        (Provided::ContestType, contest_type.to_string()),
        (Provided::ProblemType, problem_type.to_string()),
        (Provided::CheckerFormat, checker_format),
        (Provided::Language, language.to_string()),
        (Provided::CheckerLanguage, checker_language),
    ]
}

/// SQL: the language problem `problem_id`'s checker compiles its checker
/// source in, when its `checker_format` (each an SQL expression) has a
/// fleet-wide provider whose checker compiles the source
/// (`Governs::checker_languages` entries of that format): the language of
/// the first file of the problem's stored source, in stored order, whose
/// extension, compared case-insensitively, the provider declares; NULL
/// otherwise. Derived wherever it is needed, never stored, so it follows the
/// source, the declaration and the provider as they are now. The SQL form
/// of `broccoli_types::manifest::checker_language`, pinned to it by
/// `migration::tests::the_checker_language_is_derived_by_the_one_rule`.
/// Both inputs are bound first, in `input`, so the aliases inside (the
/// provider's `p` among them) never shadow what they name.
pub(crate) fn checker_language_sql(problem_id: &str, checker_format: &str) -> String {
    format!(
        "(SELECT l->>'language' \
            FROM (SELECT ({problem_id})::text AS problem, {checker_format} AS format) input \
            JOIN plugin c ON c.id = {checker} \
            CROSS JOIN LATERAL jsonb_array_elements(c.governs->'checker_languages') l \
            JOIN plugin_config pc ON pc.scope = 'problem' \
                                 AND pc.ref_id = input.problem \
                                 AND pc.namespace = c.id || ':' || (l->>'namespace') \
            CROSS JOIN LATERAL jsonb_array_elements( \
                CASE WHEN jsonb_typeof(pc.config->'files') = 'array' \
                     THEN pc.config->'files' ELSE '[]'::jsonb END) \
              WITH ORDINALITY AS f(file, n) \
           WHERE l->>'format' = input.format \
             AND lower(f.file->>'filename') LIKE '%.' || lower(l->>'extension') \
           ORDER BY f.n LIMIT 1)",
        checker = provider(Provided::CheckerFormat, "input.format"),
    )
}

/// The language problem `problem_id`'s checker compiles its source in
/// ([`checker_language_sql`]), as the database holds them now.
pub(crate) async fn checker_language_of<C: sea_orm::ConnectionTrait>(
    db: &C,
    problem_id: i32,
) -> Result<Option<String>, sea_orm::DbErr> {
    Ok(checker_languages_of(db, &[problem_id])
        .await?
        .remove(&problem_id)
        .flatten())
}

/// [`checker_language_of`] for each of `problem_ids`, in one statement.
pub(crate) async fn checker_languages_of<C: sea_orm::ConnectionTrait>(
    db: &C,
    problem_ids: &[i32],
) -> Result<std::collections::HashMap<i32, Option<String>>, sea_orm::DbErr> {
    if problem_ids.is_empty() {
        return Ok(Default::default());
    }
    let rows = db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            format!(
                "SELECT p.id, {language} AS language FROM problem p WHERE p.id = ANY($1)",
                language = checker_language_sql("p.id", "p.checker_format"),
            ),
            [problem_ids.to_vec().into()],
        ))
        .await?;
    rows.iter()
        .map(|row| Ok((row.try_get("", "id")?, row.try_get("", "language")?)))
        .collect()
}

/// SQL: the unit may run now. For every key of `needs` (each an SQL
/// expression, with its kind; a NULL key is no need): its fleet-wide
/// [`provider`] is loaded on a live replica; or, with no provider enabled,
/// no plugin ever recorded the key (an unknown key is not closed, so
/// whoever takes the work fails it visibly). A key every recorded provider
/// of which is disabled, or whose provider is loaded on no live replica,
/// parks the work. The SQL form of `PluginLifecycle::closed_by`, over the
/// rows it reads (`plugin.is_enabled`, `plugin.governs`,
/// `plugin_replica.provides`).
pub(crate) fn runnable(needs: &[(Provided, String)]) -> String {
    bound(needs, |kind, key, provider| {
        format!(
            "({key} IS NULL OR CASE \
               WHEN {provider} IS NOT NULL THEN EXISTS ({live} AND {there}) \
               ELSE NOT EXISTS (SELECT 1 FROM plugin p \
                                 WHERE p.governs->'{field}' @> jsonb_build_array({key})) \
             END)",
            live = live_replicas(),
            there = loads(kind, key, provider, "r.provides"),
            field = field(kind),
        )
    })
}

/// SQL: `predicate` over every need of `needs` (each an SQL expression, with
/// its kind), AND-ed, with each need's key and its fleet-wide [`provider`]
/// bound once (`predicate` gets the kind and the SQL names of the bound key
/// and provider): a key derived by a subquery (a problem's checker language)
/// is computed once per row, however often the predicate reads it.
fn bound(
    needs: &[(Provided, String)],
    predicate: impl Fn(Provided, &str, &str) -> String,
) -> String {
    if needs.is_empty() {
        return "TRUE".into();
    }
    let mut binds = Vec::new();
    let mut each = Vec::new();
    for (i, (kind, key)) in needs.iter().enumerate() {
        let (key_name, provider_name) = (format!("need{i}.k"), format!("need{i}.p"));
        binds.push(format!(
            "LATERAL (SELECT k, {provider} AS p FROM (SELECT {key} AS k) bound_key) need{i}",
            provider = provider(*kind, "bound_key.k"),
        ));
        each.push(predicate(*kind, &key_name, &provider_name));
    }
    format!(
        "(SELECT {} FROM {})",
        each.join(" AND "),
        binds.join(" CROSS JOIN ")
    )
}

/// SQL: the live replicas, as `r`, a `SELECT 1 FROM plugin_replica r WHERE`
/// to extend with `AND`. Live by heartbeat on the database clock.
fn live_replicas() -> String {
    format!(
        "SELECT 1 FROM plugin_replica r \
          WHERE r.heartbeat_at > NOW() - make_interval(secs => {live})",
        live = crate::plugin_fleet::LIVE_WINDOW_SECS,
    )
}

/// SQL: the fleet-wide provider of `key` (an SQL expression) of `kind`: the
/// enabled plugin providing it whose id sorts first, bytewise as Rust sorts
/// it; NULL when none is enabled. The SQL form of
/// `PluginLifecycle::provider`. The key is bound before the plugin table is
/// named, so a caller's alias in `key` is never shadowed.
pub(crate) fn provider(kind: Provided, key: &str) -> String {
    format!(
        "(SELECT min(provider_plugin.id COLLATE \"C\") \
            FROM (SELECT {key} AS key) provider_key \
            JOIN plugin provider_plugin \
              ON provider_plugin.is_enabled \
             AND provider_plugin.governs->'{field}' @> jsonb_build_array(provider_key.key))",
        field = field(kind),
    )
}

/// SQL: this replica may take work with `needs` (each an SQL expression,
/// with its kind; a NULL key is no need): for every key, this replica has
/// its fleet-wide [`provider`] loaded (`local`, the placeholder
/// [`Loaded::here`] is bound to). A key no plugin provides fleet-wide goes
/// by whether any plugin loaded it here, or no live replica has: whoever
/// takes it fails it visibly. A runnable unit ([`runnable`]) has every
/// provider loaded on some live replica, so it waits for one of them.
pub(crate) fn runs_here(needs: &[(Provided, String)], local: &str) -> String {
    bound(needs, |kind, key, provider| {
        format!(
            "({key} IS NULL OR CASE \
               WHEN {provider} IS NOT NULL THEN {here} \
               ELSE {here} OR NOT EXISTS ({live} AND {there}) \
             END)",
            here = loads(kind, key, provider, local),
            live = live_replicas(),
            there = loads(kind, key, provider, "r.provides"),
        )
    })
}

/// SQL: `loaded` has `key` of `kind` loaded on its fleet-wide `provider` (the
/// SQL of [`provider`] for it, bound by the caller), or,
/// for a key none provides fleet-wide, on any plugin.
fn loads(kind: Provided, key: &str, provider: &str, loaded: &str) -> String {
    format!(
        "COALESCE(CASE WHEN {provider} IS NULL \
                       THEN ({loaded})->'{field}' ? ({key}) \
                       ELSE ({loaded})->'{field}'->({key}) ? {provider} END, FALSE)",
        field = field(kind),
    )
}
