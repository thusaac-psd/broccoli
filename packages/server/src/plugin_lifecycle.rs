//! Fleet-wide plugin lifecycle.
//!
//! Several identical server replicas share one database, so a plugin's
//! lifecycle cannot live in any one process. The desired state is the
//! `plugin` table: `is_enabled`, the published source (bundled files or an
//! uploaded archive, see [`crate::plugin_distribution`]), plus a `generation`
//! that every lifecycle change (enable, disable, reload, upload, removing an
//! upload) bumps. Each replica converges its local registry onto that table:
//!
//! - [`run`] polls the table every [`RECONCILE_INTERVAL`] and applies any row
//!   whose generation this replica has not applied yet. The table holds one
//!   row per installed plugin, so a poll is one tiny indexless scan.
//! - The admin endpoints call the `enable` / `disable` / `reload` /
//!   `reload_all` / `install` / `remove_upload` functions here, which apply
//!   the change on the serving replica first and only then publish it (one
//!   write to the row), so a change that cannot even load here never reaches
//!   the rest of the fleet. Every peer then picks it up on its next poll. A row that
//!   disappears (an upload removed with no bundled copy to fall back to)
//!   unloads the plugin everywhere.
//!
//! A local failure stays local: an `init()` that fails is retried with
//! exponential backoff instead of leaving the plugin `Failed` until restart,
//! and nothing fleet-wide (timers, rows) is ever deleted because of it.
//! [`not_ready`] reports plugins that are desired but not loaded here, for
//! the health endpoints and the visibility kernel's fail-closed check.
//!
//! All of this is plugin-agnostic: the host only knows ids, the enabled flag,
//! and what each manifest declares.

use crate::providers::{Needs, Provided, Provides};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use broccoli_api_types::visibility::{ClosedBy, ClosedLanguage, PluginOutage};
use plugin_core::error::PluginError;
use plugin_core::registry::PluginStatus;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, EntityTrait, Statement};

use crate::entity::plugin as plugin_entity;
use crate::error::AppError;
use crate::models::plugin::{ReloadAllResponse, ReloadFailure};
use crate::plugin_archive::PluginArchive;
use crate::plugin_distribution::{self, PluginSource};
use crate::plugin_fleet::{self, ReplicaPluginState, ReplicaPluginStatus, ReplicaReport};
use crate::plugin_id::PluginId;
use crate::state::AppState;
use crate::utils::plugin::{
    self as plugin_utils, Fingerprint, LoadFailure, PluginActivationFailure,
    purge_plugin_registrations,
};

/// How often every replica re-reads the desired plugin state.
pub const RECONCILE_INTERVAL: Duration = Duration::from_secs(5);

/// First retry delay after a failed load; doubles per consecutive failure.
const RETRY_BASE: Duration = Duration::from_secs(5);
/// Ceiling for the retry delay, so a plugin that recovers is picked up within
/// a few minutes.
const RETRY_MAX: Duration = Duration::from_secs(300);

/// Per-replica plugin lifecycle state, shared through [`AppState`].
#[derive(Default)]
pub struct PluginLifecycle {
    /// What this replica has applied, per plugin. The mutex also serializes
    /// every local apply: the reconcile loop and admin requests never load or
    /// unload a plugin concurrently.
    applied: tokio::sync::Mutex<HashMap<String, Applied>>,
    /// The desired `is_enabled` per plugin as of the last apply. Read on hot
    /// paths (visibility, health), so it is a plain lock over a small map.
    desired: RwLock<HashMap<String, bool>>,
    /// This replica's latest report (also read by [`not_ready`]), and
    /// whether it reached the `plugin_replica` row.
    reported: std::sync::Mutex<Option<Reported>>,
    /// The recorded [`Governs`] per plugin, as of the last reconcile.
    governs: RwLock<HashMap<String, Governs>>,
    /// What the live replicas have loaded, each key with the plugins that
    /// registered it on one of them, as of the last reconcile
    /// (`plugin_replica.provides`); `None` before the first read.
    fleet_loaded: RwLock<Option<crate::providers::Loaded>>,
    /// What each other live replica has loaded, as of the last reconcile
    /// (its `plugin_replica.provides`), for [`Self::ready`].
    fleet_peers: RwLock<Vec<crate::providers::Loaded>>,
    /// Set by [`stop`] at shutdown: no further report writes.
    stopped: std::sync::atomic::AtomicBool,
}

struct Reported {
    report: ReplicaReport,
    provides: crate::providers::Loaded,
    /// Whether a write of an equal report succeeded.
    written: bool,
}

#[derive(Debug, Clone)]
struct Applied {
    generation: i64,
    enabled: bool,
    /// Set while the last load of this generation failed.
    retry: Option<Retry>,
    /// Set when that failed load was a reload and the previously loaded
    /// version is still serving: the error the new version failed with.
    kept_previous: Option<String>,
    /// What is loaded here.
    fingerprint: Option<Fingerprint>,
    /// Set when what is loaded here is not what the fleet published (loaded
    /// anyway because nothing else was running here).
    mismatch: Option<String>,
    /// Set when the last load failed before the plugin reached this
    /// replica's registry (its files could not be located or fetched): the
    /// error, since there is no registry entry to carry it.
    unavailable: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    attempts: u32,
    at: Instant,
}

#[derive(Debug, Clone)]
struct Desired {
    enabled: bool,
    generation: i64,
    /// What the fleet published for this plugin, if anything yet.
    published: Option<Fingerprint>,
    /// Where the published files come from.
    source: PluginSource,
}

impl Desired {
    fn of(row: &plugin_entity::Model) -> Self {
        Self {
            enabled: row.is_enabled,
            generation: row.generation,
            source: PluginSource::of(row.archive_hash.as_deref()),
            published: row
                .version
                .clone()
                .zip(row.content_hash.clone())
                .map(|(version, hash)| Fingerprint { version, hash }),
        }
    }
}

/// Why `local` is not what the fleet published, if it is not.
fn mismatch(published: Option<&Fingerprint>, local: Option<&Fingerprint>) -> Option<String> {
    let (published, local) = (published?, local?);
    (published.hash != local.hash).then(|| {
        format!(
            "this server's files (v{}, {}) differ from the published version (v{}, {})",
            local.version,
            &local.hash[..12.min(local.hash.len())],
            published.version,
            &published.hash[..12.min(published.hash.len())],
        )
    })
}

/// What a plugin governs, recorded fleet-wide whenever a replica loads it.
/// A replica that cannot load the plugin (it failed, or the files are not
/// on its disk) uses this to fail closed for exactly what the plugin would
/// have decided, instead of for everything or nothing, and to name the
/// identifiers the plugin last provided.
///
/// One shape on both sides: Rust needs every key (`visibility` and each
/// kind's list, named by `providers::field`), and the `plugin_governs_shape`
/// check (migration m0017) stores only rows with exactly those keys, so the
/// SQL that reads `plugin.governs` and this struct never see different rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Governs {
    /// It declares a `[[server.queries]] topic = "visibility"`.
    pub visibility: bool,
    /// The contest types, problem types, checker formats and languages its
    /// `init()` registered, each sorted.
    #[serde(flatten)]
    pub provides: Provides,
    /// The problem types it registered an evaluator for that uses no
    /// checker (`uses_checker` false), sorted: work on them needs no checker
    /// plugin.
    pub unchecked_problem_types: Vec<String>,
    /// The hooks its manifest declares (`[[server.hooks]]`), as topic and
    /// scope, sorted and without repeats: a replica that does not load it
    /// still knows which background events it is owed
    /// ([`PluginLifecycle::hook_plugins`]).
    pub hooks: Vec<HookTopic>,
    /// The config namespaces its manifest declares judging reads
    /// (`ConfigNamespace::is_judging_input`), sorted: a change to one of
    /// their stored configs counts in the inputs version of the problems it
    /// reaches (the `broccoli_inputs_changed` trigger, migration m0018).
    pub judging_inputs: Vec<String>,
}

/// One topic a plugin hooks, and where its hook runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct HookTopic {
    pub topic: String,
    pub scope: plugin_core::hook::HookScope,
}

/// The config namespaces `manifest` declares judging reads, as
/// [`Governs::judging_inputs`] records them.
pub fn declared_judging_inputs(manifest: &plugin_core::manifest::PluginManifest) -> Vec<String> {
    let mut inputs: Vec<String> = manifest
        .config
        .iter()
        .filter(|(_, namespace)| namespace.is_judging_input())
        .map(|(name, _)| name.clone())
        .collect();
    inputs.sort();
    inputs
}

/// The hooks `manifest` declares, as [`Governs::hooks`] records them.
pub fn declared_hooks(manifest: &plugin_core::manifest::PluginManifest) -> Vec<HookTopic> {
    let mut hooks: Vec<HookTopic> = manifest
        .server
        .iter()
        .flat_map(|server| &server.hooks)
        .map(|decl| HookTopic {
            topic: decl.topic.clone(),
            scope: decl.scope,
        })
        .collect();
    hooks.sort();
    hooks.dedup();
    hooks
}

/// What closes new work on a problem ([`PluginLifecycle::problem_closure`]):
/// the `closed_by` and `closed_languages` of its response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProblemClosure {
    pub closed_by: Option<ClosedBy>,
    pub closed_languages: Vec<ClosedLanguage>,
}

/// A plugin that is enabled fleet-wide but not loaded on this replica.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginNotReady {
    /// Plugin id.
    pub id: String,
    /// Why it is not loaded here.
    pub reason: String,
}

impl Retry {
    /// The retry after one more failed attempt (following `previous`, if the
    /// last attempt failed too).
    fn after(previous: Option<Retry>, now: Instant) -> Self {
        let attempts = previous.map_or(1, |r| r.attempts.saturating_add(1));
        Retry {
            attempts,
            at: now + retry_delay(attempts),
        }
    }
}

fn retry_delay(attempts: u32) -> Duration {
    common::retry::doubling_backoff(attempts, RETRY_BASE, RETRY_MAX)
}

impl PluginLifecycle {
    /// Whether `plugin_id` is enabled in the desired state this replica last
    /// applied. Unknown ids are not enabled.
    pub fn is_desired_enabled(&self, plugin_id: &str) -> bool {
        self.desired
            .read()
            .map(|d| d.get(plugin_id).copied().unwrap_or(false))
            .unwrap_or(false)
    }

    pub(crate) fn desired_enabled_ids(&self) -> Vec<String> {
        self.desired
            .read()
            .map(|d| {
                d.iter()
                    .filter(|(_, enabled)| **enabled)
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every plugin the fleet's desired state names, with whether it is
    /// enabled.
    pub(crate) fn desired_plugins(&self) -> Vec<(String, bool)> {
        self.desired
            .read()
            .map(|d| {
                d.iter()
                    .map(|(id, enabled)| (id.clone(), *enabled))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// What `plugin_id` governs, as recorded by the replica that last loaded
    /// it; `None` if no replica ever has.
    pub fn governs(&self, plugin_id: &str) -> Option<Governs> {
        self.governs.read().ok()?.get(plugin_id).cloned()
    }

    /// What keeps a submission with `needs`, in `status`, parked, if
    /// anything: one not yet finished that cannot run now
    /// ([`Self::closed_by`]) waits queued until a provider is enabled (see
    /// `providers::runnable`).
    pub fn parked_by(
        &self,
        needs: Needs<'_>,
        status: common::SubmissionStatus,
    ) -> Option<ClosedBy> {
        if status.is_terminal() {
            return None;
        }
        self.closed_by(needs)
    }

    /// What keeps work with `needs` from running: for the first of its
    /// keys, in [`crate::providers::KINDS`] order, that cannot run, the
    /// plugin and why, with what it provides for the work. A key whose
    /// providers are all disabled is closed by the first of them
    /// ([`Self::disabled_provider`], `Disabled`); a key whose fleet-wide
    /// provider ([`Self::provider`]) is loaded on no live replica is closed
    /// by that provider (`NotLoaded`), so the work waits for a replica that
    /// loads it. `None` when every key can run, or was never recorded by any
    /// plugin. `providers::runnable` states the same rule in SQL.
    pub fn closed_by(&self, needs: Needs<'_>) -> Option<ClosedBy> {
        use crate::providers::FleetProvider;
        use broccoli_api_types::visibility::ClosedReason;
        needs
            .each()
            .into_iter()
            .find_map(|(provides, key)| match self.provider(provides, key) {
                FleetProvider::Plugin(plugin_id) => {
                    (!self.loaded_somewhere(provides, key, &plugin_id)).then_some(ClosedBy {
                        plugin_id,
                        provides,
                        reason: ClosedReason::NotLoaded,
                    })
                }
                FleetProvider::Closed => {
                    self.disabled_provider(provides, key)
                        .map(|plugin_id| ClosedBy {
                            plugin_id,
                            provides,
                            reason: ClosedReason::Disabled,
                        })
                }
                FleetProvider::Unknown => None,
            })
    }

    /// Whether `plugin_id` has `key` (of `kind`) loaded on a live replica,
    /// as of the last reconcile's read of the fleet. Before the first read
    /// every provider counts as loaded somewhere, so a replica that just
    /// started refuses nothing it cannot judge yet.
    pub fn loaded_somewhere(&self, kind: Provided, key: &str, plugin_id: &str) -> bool {
        self.fleet_loaded
            .read()
            .ok()
            .and_then(|loaded| loaded.as_ref().map(|l| l.has(kind, key, plugin_id)))
            .unwrap_or(true)
    }

    /// Records what the live replicas have loaded (see
    /// [`Self::loaded_somewhere`]).
    pub(crate) fn set_fleet_loaded(&self, loaded: crate::providers::Loaded) {
        if let Ok(mut slot) = self.fleet_loaded.write() {
            *slot = Some(loaded);
        }
    }

    /// Records what each other live replica has loaded (see
    /// [`Self::ready`]).
    pub(crate) fn set_fleet_peers(&self, peers: Vec<crate::providers::Loaded>) {
        if let Ok(mut slot) = self.fleet_peers.write() {
            *slot = peers;
        }
    }

    /// The `kind` keys whose fleet-wide provider ([`Self::provider`]) is not
    /// loaded in `loaded`, each with that provider, sorted: the one "provider
    /// missing here" rule. Work on such a key that reaches a replica with
    /// `loaded` waits for another replica, or is refused there
    /// (`CONTEST_TYPE_UNAVAILABLE` for a contest type, [`accepting_handler`]).
    pub fn missing_here(
        &self,
        kind: Provided,
        loaded: &crate::providers::Loaded,
    ) -> std::collections::BTreeSet<(String, String)> {
        use crate::providers::FleetProvider;
        let Ok(governs) = self.governs.read() else {
            return Default::default();
        };
        let keys: std::collections::BTreeSet<String> = governs
            .values()
            .flat_map(|g| g.provides.keys(kind))
            .cloned()
            .collect();
        drop(governs);
        keys.into_iter()
            .filter_map(|key| match self.provider(kind, &key) {
                FleetProvider::Plugin(id) if !loaded.has(kind, &key, &id) => Some((key, id)),
                _ => None,
            })
            .collect()
    }

    /// Whether this replica, with `local` loaded, should take browsers: no
    /// live peer misses only a strict part of the contest types it misses
    /// ([`Self::missing_here`]). A peer that does serves every submission
    /// this replica takes and more, so a gateway that health-checks
    /// readiness re-pins this replica's browsers there instead of leaving
    /// them refused. The order is partial, so some replica is always ready:
    /// replicas that each lack a different type, or a type no replica has
    /// loaded, are all ready, and a plugin failing everywhere never takes
    /// the fleet out of rotation. Only contest types count, since they are
    /// what a request on this replica needs loaded here; the other keys only
    /// steer which replica claims judging work.
    pub fn ready(&self, local: &crate::providers::Loaded) -> bool {
        let mine = self.missing_here(Provided::ContestType, local);
        if mine.is_empty() {
            return true;
        }
        let Ok(peers) = self.fleet_peers.read() else {
            return true;
        };
        !peers.iter().any(|peer| {
            let theirs = self.missing_here(Provided::ContestType, peer);
            theirs.len() < mine.len() && theirs.is_subset(&mine)
        })
    }

    /// What closes new work on a problem whose `needs` name no language yet
    /// (a problem view, before the contestant picks one): each language it
    /// accepts (recorded fleet-wide, and among `accepted` when the problem
    /// names its languages) whose providers are all disabled, and the work
    /// outright when what it needs besides a language is closed
    /// ([`Self::closed_by`]) or none of its languages is left open (the
    /// first closed one's plugin). A fleet that recorded none of them closes
    /// nothing here.
    pub fn problem_closure(
        &self,
        needs: Needs<'_>,
        accepted: Option<&HashSet<&str>>,
    ) -> ProblemClosure {
        let languages: std::collections::BTreeSet<String> = self
            .governs
            .read()
            .map(|governs| {
                governs
                    .values()
                    .flat_map(|g| g.provides.languages.iter())
                    .filter(|language| accepted.is_none_or(|a| a.contains(language.as_str())))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let closed_languages: Vec<ClosedLanguage> = languages
            .iter()
            .filter_map(|language| {
                let plugin_id = self.disabled_provider(Provided::Language, language)?;
                Some(ClosedLanguage {
                    language: language.clone(),
                    closed_by: ClosedBy {
                        plugin_id,
                        provides: Provided::Language,
                        reason: broccoli_api_types::visibility::ClosedReason::Disabled,
                    },
                })
            })
            .collect();
        let none_open = !languages.is_empty() && closed_languages.len() == languages.len();
        let closed_by = self.closed_by(needs).or_else(|| {
            none_open
                .then(|| closed_languages.first().map(|l| l.closed_by.clone()))
                .flatten()
        });
        ProblemClosure {
            closed_by,
            closed_languages,
        }
    }

    /// The plugin that keeps `key` (a `kind` of key) closed: every plugin
    /// that last registered the key is disabled fleet-wide, and this is the
    /// first of them by id. `None` when an enabled plugin provides the key,
    /// or no plugin ever registered it (an unknown key is not disabled).
    pub fn disabled_provider(&self, kind: Provided, key: &str) -> Option<String> {
        let governs = self.governs.read().ok()?;
        let mut providers: Vec<&String> = governs
            .iter()
            .filter(|(_, g)| g.provides.provides(kind, key))
            .map(|(id, _)| id)
            .collect();
        if providers.iter().any(|id| self.is_desired_enabled(id)) {
            return None;
        }
        providers.sort();
        providers.first().map(|id| (*id).clone())
    }

    /// Who resolves `key` (a `kind` of key) fleet-wide: the enabled plugin
    /// providing it whose id sorts first; `Closed` when every plugin that
    /// recorded it is disabled; `Unknown` when none ever recorded it. The
    /// one rule every replica resolves a key by (see `providers::resolve`);
    /// `providers::provider` states it in SQL.
    pub fn provider(&self, kind: Provided, key: &str) -> crate::providers::FleetProvider {
        use crate::providers::FleetProvider;
        let Ok(governs) = self.governs.read() else {
            return FleetProvider::Unknown;
        };
        let mut recorded = false;
        let mut first: Option<&String> = None;
        for (id, g) in governs.iter() {
            if !g.provides.provides(kind, key) {
                continue;
            }
            recorded = true;
            if self.is_desired_enabled(id) && first.is_none_or(|f| id < f) {
                first = Some(id);
            }
        }
        match (first, recorded) {
            (Some(id), _) => FleetProvider::Plugin(id.clone()),
            (None, true) => FleetProvider::Closed,
            (None, false) => FleetProvider::Unknown,
        }
    }

    /// What work of `contest_type` on `problem`, in `language` (none for the
    /// problem alone), needs: exactly the plugins it uses. Its problem's
    /// checker format is a need unless the problem type's fleet-wide
    /// evaluator declared it uses no checker. `providers::sql_needs` states
    /// the same in SQL.
    pub fn needs<'a>(
        &self,
        contest_type: &'a str,
        problem_type: &'a str,
        checker_format: &'a str,
        language: Option<&'a str>,
    ) -> Needs<'a> {
        use crate::providers::FleetProvider;
        let unchecked = match self.provider(Provided::ProblemType, problem_type) {
            FleetProvider::Plugin(evaluator) => self
                .governs(&evaluator)
                .is_some_and(|g| g.unchecked_problem_types.iter().any(|t| t == problem_type)),
            FleetProvider::Closed | FleetProvider::Unknown => false,
        };
        Needs {
            contest_type,
            problem_type,
            checker_format: (!unchecked).then_some(checker_format),
            language,
        }
    }

    /// [`Self::needs`] of work of `contest_type` on `problem`.
    pub fn needs_on<'a>(
        &self,
        contest_type: &'a str,
        problem: &'a crate::entity::problem::Model,
        language: Option<&'a str>,
    ) -> Needs<'a> {
        self.needs(
            contest_type,
            &problem.problem_type,
            &problem.checker_format,
            language,
        )
    }

    /// Whether `local` has every key of `needs` loaded on its fleet-wide
    /// provider ([`Self::provider`]), or, for a key none ever recorded, on
    /// any plugin. `providers::runs_here` reads the same in SQL.
    pub fn all_loaded(&self, local: &crate::providers::Loaded, needs: Needs<'_>) -> bool {
        use crate::providers::FleetProvider;
        needs
            .each()
            .into_iter()
            .all(|(kind, key)| match self.provider(kind, key) {
                FleetProvider::Plugin(id) => local.has(kind, key, &id),
                FleetProvider::Unknown => local.has_key(kind, key),
                FleetProvider::Closed => false,
            })
    }

    // `providers::runnable` and `providers::provider` state these rules in
    // SQL over the rows they read (`plugin.is_enabled`, `plugin.governs`);
    // change them together.

    /// Why plugin `plugin_id`, known not to be loaded here, cannot serve:
    /// the fleet disabled it, or it is enabled and missing on this replica
    /// for now. This is the one place the two are told apart.
    pub fn outage(&self, plugin_id: &str) -> PluginOutage {
        if self.is_desired_enabled(plugin_id) {
            PluginOutage::Unavailable
        } else {
            PluginOutage::Disabled {
                plugin_id: plugin_id.to_string(),
            }
        }
    }

    pub(crate) fn set_desired(&self, plugin_id: &str, enabled: bool) {
        if let Ok(mut d) = self.desired.write() {
            d.insert(plugin_id.to_string(), enabled);
        }
    }

    /// The plugins enabled fleet-wide whose recorded hooks run for a
    /// `topic` event with `enabled_plugins`: every one with a global hook on
    /// it, and every one with a resource hook on it that the event's
    /// resource enables. A background event is complete once each has it
    /// delivered (`hooks::dispatch_hooks_once`).
    pub fn hook_plugins(
        &self,
        topic: &str,
        enabled_plugins: Option<&crate::hooks::ResourceEnablements>,
    ) -> std::collections::BTreeSet<String> {
        let desired = self.desired.read().map(|d| d.clone()).unwrap_or_default();
        let Ok(governs) = self.governs.read() else {
            return Default::default();
        };
        governs
            .iter()
            .filter(|(id, _)| desired.get(id.as_str()).copied().unwrap_or(false))
            .filter(|(id, g)| {
                g.hooks.iter().any(|hook| {
                    hook.topic == topic
                        && match hook.scope {
                            plugin_core::hook::HookScope::Global => true,
                            plugin_core::hook::HookScope::Resource => {
                                enabled_plugins.is_some_and(|e| e.contains_key(id.as_str()))
                            }
                        }
                })
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Every plugin's recorded [`Governs`], in plugin id order.
    pub fn recorded_governs(&self) -> Vec<(String, Governs)> {
        let mut recorded: Vec<(String, Governs)> = self
            .governs
            .read()
            .map(|map| map.iter().map(|(id, g)| (id.clone(), g.clone())).collect())
            .unwrap_or_default();
        recorded.sort_by(|a, b| a.0.cmp(&b.0));
        recorded
    }

    pub(crate) fn set_governs(&self, plugin_id: &str, governs: Governs) {
        if let Ok(mut map) = self.governs.write() {
            map.insert(plugin_id.to_string(), governs);
        }
    }

    /// Drop everything recorded about a plugin the fleet no longer has.
    fn forget(&self, plugin_id: &str) {
        if let Ok(mut d) = self.desired.write() {
            d.remove(plugin_id);
        }
        if let Ok(mut g) = self.governs.write() {
            g.remove(plugin_id);
        }
    }
}

/// Why contests of `contest_type` cannot be served on this replica, if they
/// cannot: the plugin providing the type is disabled fleet-wide, or the
/// type's fleet-wide provider is not loaded here (`providers::resolve`). Submissions to such a contest are refused
/// and its results are unavailable.
pub async fn contest_type_outage(state: &AppState, contest_type: &str) -> Option<PluginOutage> {
    if let Some(plugin_id) = state
        .plugin_lifecycle
        .disabled_provider(Provided::ContestType, contest_type)
    {
        return Some(PluginOutage::Disabled { plugin_id });
    }
    let resolved = crate::providers::resolve(
        &*state.registries.contest_type_registry.read().await,
        state
            .plugin_lifecycle
            .provider(Provided::ContestType, contest_type),
        Provided::ContestType,
        contest_type,
    );
    resolved.is_err().then_some(PluginOutage::Unavailable)
}

/// The handler a new submission or code run with `needs` is accepted
/// through, or why none can take it now: the one acceptance rule of every
/// submit path, contest or practice. Work that would be parked is refused,
/// by the same rule (`PluginLifecycle::closed_by`): a contest type whose
/// providers are all disabled (`CONTEST_TYPE_DISABLED`), any other key whose
/// providers are (`PLUGIN_DISABLED`), or a key whose fleet-wide provider is
/// loaded on no live replica (`PLUGIN_NOT_LOADED`), each naming the plugin
/// and retried by the client. Otherwise the handler is the contest type's
/// fleet-wide provider (`providers::resolve`), the plugin that admits the
/// work through its `before_submission` hook; a replica that does not have
/// it loaded is briefly unavailable (`CONTEST_TYPE_UNAVAILABLE`, retry
/// later), and never admits the work through another provider.
pub async fn accepting_handler(
    state: &AppState,
    needs: Needs<'_>,
) -> Result<crate::registry::ContestTypeHandlers, AppError> {
    use broccoli_api_types::visibility::ClosedReason;
    let contest_type = needs.contest_type;
    match state.plugin_lifecycle.closed_by(needs) {
        Some(ClosedBy {
            provides: Provided::ContestType,
            reason: ClosedReason::Disabled,
            ..
        }) => return Err(AppError::ContestTypeDisabled(contest_type.to_string())),
        Some(ClosedBy {
            plugin_id,
            reason: ClosedReason::Disabled,
            ..
        }) => return Err(AppError::PluginDisabled(plugin_id)),
        Some(ClosedBy {
            plugin_id,
            reason: ClosedReason::NotLoaded,
            ..
        }) => return Err(AppError::PluginNotLoaded(plugin_id)),
        None => {}
    }
    crate::providers::resolve(
        &*state.registries.contest_type_registry.read().await,
        state
            .plugin_lifecycle
            .provider(Provided::ContestType, contest_type),
        Provided::ContestType,
        contest_type,
    )
    .map_err(|_| AppError::ContestTypeUnavailable(contest_type.to_string()))
}

/// Plugins that are enabled in the desired state but not `Loaded` here,
/// sorted by id. In-memory only, so it is cheap enough for every health probe.
pub fn not_ready(state: &AppState) -> Vec<PluginNotReady> {
    let mut out = Vec::new();
    let Ok(registry) = state.plugins.get_registry().read() else {
        return vec![PluginNotReady {
            id: "*".into(),
            reason: "plugin registry lock poisoned".into(),
        }];
    };
    let outdated: HashMap<String, String> = match state.plugin_lifecycle.reported.lock() {
        Ok(reported) => reported
            .iter()
            .flat_map(|r| &r.report)
            .filter(|(_, p)| p.status == ReplicaPluginStatus::Outdated)
            .map(|(id, p)| (id.clone(), p.error.clone().unwrap_or_default()))
            .collect(),
        Err(_) => HashMap::new(),
    };
    for id in state.plugin_lifecycle.desired_enabled_ids() {
        let outdated = outdated.get(&id).cloned();
        let reason = match registry.get(&id).map(|e| &e.status) {
            Some(PluginStatus::Loaded) => match outdated {
                Some(error) => format!("still on the previous version: {error}"),
                None => continue,
            },
            Some(PluginStatus::Failed(error)) => error.clone(),
            Some(PluginStatus::Unloaded) => "not loaded yet".to_string(),
            None => "not installed on this replica".to_string(),
        };
        out.push(PluginNotReady { id, reason });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Boot: discover every plugin on disk, record new ones as enabled, and load
/// whatever the desired state says. Returns the plugins that failed to load
/// (they are retried by [`run`]). The plugins loaded may check configs
/// differently than the ones that last checked them, so boot then rechecks
/// every problem's config issues (see `services::config_issues`).
pub async fn boot(state: &AppState) -> anyhow::Result<Vec<PluginActivationFailure>> {
    state.plugins.discover_plugins()?;
    let ids: Vec<String> = state
        .plugins
        .list_plugins()?
        .into_iter()
        .map(|p| p.id)
        .collect();
    register_rows(&state.db, &registrations(state, &ids)).await?;
    let failures = with_applied(state, async |applied| {
        reconcile_locked(state, applied).await
    })
    .await?;
    crate::services::config_issues::recheck_every_problem(
        &state.db,
        &crate::services::config_issues::ConfigChecker::of(state),
    )
    .await;
    Ok(failures)
}

/// Converge this replica onto the desired state once.
pub async fn reconcile(state: &AppState) -> Result<Vec<PluginActivationFailure>, DbErr> {
    with_applied(state, async |applied| {
        reconcile_locked(state, applied).await
    })
    .await
}

/// Run `f` with this replica's applied state locked, then write the
/// replica's report. Every load and unload goes through here, so the report
/// the admin pages read can never lag a local change by more than one write.
async fn with_applied<T>(
    state: &AppState,
    f: impl AsyncFnOnce(&mut HashMap<String, Applied>) -> T,
) -> T {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    let out = f(&mut applied).await;
    write_report(state, &applied).await;
    out
}

/// What this replica reports for each plugin it knows (see
/// [`crate::plugin_fleet`]).
fn build_report(
    state: &AppState,
    applied: &HashMap<String, Applied>,
    now: Instant,
) -> ReplicaReport {
    let Ok(registry) = state.plugins.get_registry().read() else {
        return ReplicaReport::new();
    };
    let ids: std::collections::BTreeSet<&String> = applied.keys().chain(registry.keys()).collect();
    ids.into_iter()
        .map(|id| {
            let a = applied.get(id);
            let kept_previous = a.and_then(|a| a.kept_previous.clone());
            let (status, error) = match registry.get(id).map(|e| &e.status) {
                Some(PluginStatus::Loaded) if kept_previous.is_some() => {
                    (ReplicaPluginStatus::Outdated, kept_previous)
                }
                Some(PluginStatus::Loaded) if a.is_some_and(|a| a.mismatch.is_some()) => (
                    ReplicaPluginStatus::Mismatched,
                    a.and_then(|a| a.mismatch.clone()),
                ),
                Some(PluginStatus::Loaded) => (ReplicaPluginStatus::Loaded, None),
                Some(PluginStatus::Failed(e)) => (ReplicaPluginStatus::Failed, Some(e.clone())),
                None if a.is_some_and(|a| a.enabled) => (
                    ReplicaPluginStatus::NotInstalled,
                    a.and_then(|a| a.unavailable.clone()),
                ),
                _ => (ReplicaPluginStatus::Unloaded, None),
            };
            let retry_in_ms = a
                .and_then(|a| a.retry)
                .filter(|_| status != ReplicaPluginStatus::Loaded)
                .map(|r| r.at.saturating_duration_since(now).as_millis() as u64);
            let state = ReplicaPluginState {
                status,
                error,
                generation: a.map_or(0, |a| a.generation),
                retry_in_ms,
                version: a
                    .and_then(|a| a.fingerprint.as_ref())
                    .filter(|_| {
                        matches!(
                            status,
                            ReplicaPluginStatus::Loaded
                                | ReplicaPluginStatus::Outdated
                                | ReplicaPluginStatus::Mismatched
                        )
                    })
                    .map(|f| f.version.clone()),
            };
            (id.clone(), state)
        })
        .collect()
}

/// Record the report in memory, and write it if it changed. Best-effort: a
/// failed write only makes the admin view stale, and is retried on the next
/// pass. The row's heartbeat is [`heartbeat`]'s.
async fn write_report(state: &AppState, applied: &HashMap<String, Applied>) {
    let report = build_report(state, applied, Instant::now());
    let provides = crate::providers::Loaded::here(&state.registries).await;
    if !record_report(state, &report, &provides) {
        return;
    }
    match plugin_fleet::report(&state.db, &state.config.server.id, &report, &provides).await {
        Ok(()) => {
            if let Ok(mut reported) = state.plugin_lifecycle.reported.lock()
                && let Some(last) = reported.as_mut()
                && same_report(&last.report, &report)
            {
                last.written = true;
            }
        }
        Err(e) => tracing::warn!(error = %e, "Failed to write this replica's plugin report"),
    }
}

/// Keep `report` as this replica's latest, and say whether it is due to be
/// written: it changed, or no write of it has succeeded yet. Never due once
/// [`stop`] ran.
fn record_report(
    state: &AppState,
    report: &ReplicaReport,
    provides: &crate::providers::Loaded,
) -> bool {
    let Ok(mut reported) = state.plugin_lifecycle.reported.lock() else {
        return false;
    };
    let written = reported.as_ref().is_some_and(|last| {
        last.written && same_report(&last.report, report) && last.provides == *provides
    });
    *reported = Some(Reported {
        report: report.clone(),
        provides: provides.clone(),
        written,
    });
    !written
        && !state
            .plugin_lifecycle
            .stopped
            .load(std::sync::atomic::Ordering::SeqCst)
}

/// Reports are equal for write purposes when they differ at most in how long
/// is left until a retry: that shrinks on every call, and is only rewritten
/// along with a real change (a new attempt).
fn same_report(a: &ReplicaReport, b: &ReplicaReport) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|((ka, va), (kb, vb))| {
            ka == kb
                && va.status == vb.status
                && va.error == vb.error
                && va.generation == vb.generation
                && va.version == vb.version
                && va.retry_in_ms.is_some() == vb.retry_in_ms.is_some()
        })
}

/// Stop writing this replica's report, for shutdown: once this returns, no
/// write is in progress and none will follow, so the row can be retired
/// without a reconcile tick re-creating it.
pub async fn stop(state: &AppState) {
    state
        .plugin_lifecycle
        .stopped
        .store(true, std::sync::atomic::Ordering::SeqCst);
    // Every write happens under this lock (`with_applied`): taking it waits
    // out one already under way.
    drop(state.plugin_lifecycle.applied.lock().await);
}

/// This replica's heartbeat loop: touches its `plugin_replica` row every
/// [`plugin_fleet::HEARTBEAT_INTERVAL`] until [`stop`]. It never takes the
/// lifecycle's lock, so a long load or reload here does not make peers
/// treat this replica as dead and take the contest types it judges.
pub async fn heartbeat(state: AppState) {
    let mut interval = tokio::time::interval(plugin_fleet::HEARTBEAT_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        if state
            .plugin_lifecycle
            .stopped
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        if let Err(e) = plugin_fleet::heartbeat(&state.db, &state.config.server.id).await {
            tracing::warn!(error = %e, "Failed to touch this replica's heartbeat");
        }
    }
}

/// The per-replica reconcile loop. Runs until [`stop`].
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; boot has just reconciled.
    interval.tick().await;
    loop {
        interval.tick().await;
        if state
            .plugin_lifecycle
            .stopped
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        if let Err(e) = reconcile(&state).await {
            tracing::warn!(error = %e, "Plugin reconcile failed; retrying next tick");
        }
        // A bundle import restores problems no running server checked.
        crate::services::config_issues::recheck_marked(&state).await;
    }
}

async fn reconcile_locked(
    state: &AppState,
    applied: &mut HashMap<String, Applied>,
) -> Result<Vec<PluginActivationFailure>, DbErr> {
    let mut rows = plugin_entity::Entity::find().all(&state.db).await?;

    // A generation this replica has not applied means the fleet changed
    // (an admin action somewhere, or a peer recorded a new plugin): look for
    // plugin directories that appeared on this replica's disk since.
    let fleet_changed = rows.iter().any(|r| {
        applied
            .get(&r.id)
            .is_none_or(|a| a.generation != r.generation)
    });
    if fleet_changed && register_new_dirs(state).await? {
        rows = plugin_entity::Entity::find().all(&state.db).await?;
    }

    let now = Instant::now();
    let mut failures = Vec::new();
    // Rows deleted since the last pass: the fleet no longer has the plugin.
    let gone: Vec<String> = applied
        .keys()
        .filter(|id| !rows.iter().any(|r| &r.id == *id))
        .cloned()
        .collect();
    for id in &gone {
        forget(state, applied, id).await;
    }
    let published_archives = published_archives(&rows);
    for row in rows {
        if let Some(governs) = row.governs.clone() {
            match serde_json::from_value::<Governs>(governs) {
                Ok(governs) => state.plugin_lifecycle.set_governs(&row.id, governs),
                // The table's shape check stores only rows this reads.
                Err(e) => tracing::error!(
                    plugin_id = %row.id,
                    error = %e,
                    "A plugin's recorded governs does not have the one shape both readers share"
                ),
            }
        }
        let desired = Desired::of(&row);
        // Record the desired state BEFORE applying it: while an enabled
        // plugin is still loading here, the visibility kernel must already
        // treat it as enabled-but-not-loaded (fail closed), not as disabled.
        state.plugin_lifecycle.set_desired(&row.id, row.is_enabled);
        let Some(id) = row_id(&row) else { continue };
        if let Err(error) = apply(state, applied, &id, desired, now).await {
            failures.push(PluginActivationFailure {
                plugin_id: row.id.clone(),
                error,
            });
        }
    }
    if fleet_changed || !gone.is_empty() {
        retain_cache(state, published_archives).await;
    }
    // What the live replicas have loaded, this one's own registrations
    // included before its next report lands.
    let replicas = plugin_fleet::live_replicas(&state.db).await?;
    let mut loaded = crate::providers::Loaded::here(&state.registries).await;
    for (_, theirs) in &replicas {
        loaded.merge(theirs);
    }
    state.plugin_lifecycle.set_fleet_loaded(loaded);
    state.plugin_lifecycle.set_fleet_peers(
        replicas
            .into_iter()
            .filter(|(server_id, _)| *server_id != state.config.server.id)
            .map(|(_, theirs)| theirs)
            .collect(),
    );
    Ok(failures)
}

/// The row's plugin id, if it is a valid one. A row with an invalid id was
/// not written by this host; it is never applied, so its id never reaches a
/// path.
fn row_id(row: &plugin_entity::Model) -> Option<PluginId> {
    PluginId::parse(row.id.as_str())
        .inspect_err(|e| tracing::warn!(error = %e, "Ignoring a plugin row"))
        .ok()
}

/// The cache entries every published upload maps to.
fn published_archives(rows: &[plugin_entity::Model]) -> HashSet<String> {
    rows.iter()
        .filter_map(|r| plugin_distribution::cache_entry_for(r.archive_hash.as_deref()?))
        .collect()
}

/// Drop cached archives that are neither in `keep` (published) nor loaded
/// here: a version kept serving after a failed update is still loaded, so
/// its files stay until it is replaced.
async fn retain_cache(state: &AppState, mut keep: HashSet<String>) {
    if let Ok(registry) = state.plugins.get_registry().read() {
        keep.extend(
            registry
                .values()
                .filter_map(|e| plugin_distribution::cache_entry_of(state, &e.root_dir)),
        );
    }
    plugin_distribution::retain(state, keep).await;
}

/// Unload a plugin whose row was deleted and forget it on this replica. It
/// unloads through [`deactivate`], as every unload does, so its judging
/// sessions end and its SQL pool closes now; then this replica drops what it
/// recorded about it.
async fn forget(state: &AppState, applied: &mut HashMap<String, Applied>, plugin_id: &str) {
    if let Err(e) = deactivate(state, plugin_id).await {
        tracing::warn!(plugin_id, error = %e, "Could not unload a plugin the fleet forgot");
    }
    if let Ok(mut registry) = state.plugins.get_registry().write() {
        registry.remove(plugin_id);
    }
    plugin_utils::update_translations(state).await;
    applied.remove(plugin_id);
    state.plugin_lifecycle.forget(plugin_id);
    tracing::info!(plugin_id, "Plugin removed from the fleet; unloaded it here");
}

/// Locate the files `desired` publishes and fingerprint them. An uploaded
/// copy that does not match the published fingerprint is a damaged cache
/// entry (the archive itself was verified by hash when it was fetched), so
/// it is dropped and fetched once more.
async fn locate_published(
    state: &AppState,
    id: &PluginId,
    desired: &Desired,
) -> Result<(std::path::PathBuf, Option<Fingerprint>), PluginError> {
    let root = plugin_distribution::locate(state, id, &desired.source).await?;
    let local = plugin_utils::fingerprint(&root).await;
    let damaged = matches!(desired.source, PluginSource::Upload(_))
        && mismatch(desired.published.as_ref(), local.as_ref()).is_some();
    if !damaged {
        return Ok((root, local));
    }
    tracing::warn!(
        plugin_id = %id,
        "This server's copy of the uploaded plugin differs from the published one; fetching it again"
    );
    plugin_distribution::evict(state, &desired.source).await;
    let root = plugin_distribution::locate(state, id, &desired.source).await?;
    let local = plugin_utils::fingerprint(&root).await;
    Ok((root, local))
}

/// Bring one plugin to `desired` on this replica, honouring the retry backoff
/// of a previously failed load.
async fn apply(
    state: &AppState,
    applied: &mut HashMap<String, Applied>,
    id: &PluginId,
    desired: Desired,
    now: Instant,
) -> Result<(), PluginError> {
    let plugin_id = id.as_str();
    let loaded = state.plugins.is_plugin_loaded(plugin_id).unwrap_or(false);

    if !desired.enabled {
        if loaded {
            deactivate(state, plugin_id).await?;
        }
        let changed = applied
            .get(plugin_id)
            .is_none_or(|a| a.generation != desired.generation);
        if changed {
            // Keep the published version's manifest listed here while it is
            // off, so the admin pages describe what enabling would load.
            match plugin_distribution::locate(state, id, &desired.source).await {
                Ok(root) => plugin_utils::register_unloaded(state, plugin_id, &root),
                Err(e) => {
                    tracing::debug!(plugin_id, error = %e, "Disabled plugin's files are not available here");
                }
            }
        }
        applied.insert(
            plugin_id.to_string(),
            Applied {
                generation: desired.generation,
                enabled: false,
                retry: None,
                kept_previous: None,
                fingerprint: None,
                mismatch: None,
                unavailable: None,
            },
        );
        return Ok(());
    }

    let same = applied
        .get(plugin_id)
        .filter(|a| a.enabled && a.generation == desired.generation);
    let retry = same.and_then(|a| a.retry);
    match (same, retry) {
        (Some(_), None) if loaded => return Ok(()),
        (Some(_), Some(r)) if now < r.at => return Ok(()),
        _ => {}
    }

    let running = applied.get(plugin_id).and_then(|a| a.fingerprint.clone());
    // Never swap a running version for files that are not what the fleet
    // published: this replica's copy may be stale or half-synced. With
    // nothing running here, load them anyway and flag the difference, so a
    // fleet restarted onto new files without a Reload still serves.
    let (local, differs, result) = match locate_published(state, id, &desired).await {
        Err(error) => (
            None,
            None,
            Err(LoadFailure {
                error,
                kept_previous: loaded,
            }),
        ),
        Ok((root, local)) => {
            let differs = mismatch(desired.published.as_ref(), local.as_ref());
            let result = match &differs {
                Some(reason) if loaded => Err(LoadFailure {
                    error: PluginError::LoadFailed(reason.clone()),
                    kept_previous: true,
                }),
                _ => plugin_utils::load_from_dir(state, plugin_id, &root)
                    .await
                    .map(drop),
            };
            (local, differs, result)
        }
    };
    if result.is_ok() && differs.is_none() {
        record_governs(state, plugin_id, desired.generation).await;
    }
    let entry = match &result {
        Ok(()) => Applied {
            generation: desired.generation,
            enabled: true,
            retry: None,
            kept_previous: None,
            fingerprint: local,
            mismatch: differs,
            unavailable: None,
        },
        Err(failure) => {
            let r = Retry::after(retry, now);
            tracing::warn!(
                plugin_id,
                attempts = r.attempts,
                retry_in_secs = retry_delay(r.attempts).as_secs(),
                kept_previous = failure.kept_previous,
                error = %failure.error,
                "Plugin failed to load on this replica; will retry"
            );
            Applied {
                generation: desired.generation,
                enabled: true,
                retry: Some(r),
                kept_previous: failure.kept_previous.then(|| failure.error.to_string()),
                fingerprint: if failure.kept_previous { running } else { None },
                mismatch: None,
                unavailable: (!failure.kept_previous).then(|| failure.error.to_string()),
            }
        }
    };
    applied.insert(plugin_id.to_string(), entry);
    result.map_err(|f| f.error)
}

/// Unload a plugin for good (a disable, or the fleet forgot it). Its judging
/// sessions end now rather than as their results arrive: each ends as its
/// plugin being gone does (`fail_dispatch`), which hands the judgement back
/// while its type is parked. Every replica unloads through here, the one
/// that took the change and every one that applies it in its reconcile.
async fn deactivate(state: &AppState, plugin_id: &str) -> Result<(), PluginError> {
    purge_plugin_registrations(&state.registries, plugin_id).await;
    match state.plugins.unload_plugin(plugin_id) {
        // Never registered here: nothing of it runs, but its leftovers go.
        Ok(()) | Err(PluginError::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    let cut = state.registries.judging_sessions.cut(plugin_id);
    if cut > 0 {
        tracing::info!(
            plugin_id,
            sessions = cut,
            "Cut the judging sessions of an unloaded plugin"
        );
    }
    state.plugin_sql.forget(plugin_id).await;
    plugin_utils::update_translations(state).await;
    Ok(())
}

/// Record plugins found on this replica's disk that have no row yet, as
/// enabled (a newly installed plugin is on by default), each with the locale
/// it provides as a language pack (`plugins`: id and locale). A pack for a
/// locale another enabled plugin provides is recorded disabled instead,
/// since one enabled pack provides a locale, and logged; of two new packs for
/// one locale the one whose id sorts first is enabled.
///
/// Idempotent, so replicas booting together cannot fail on the primary key,
/// and in id order, so they cannot deadlock either: each insert locks its
/// rows in the order it lists them, and replicas that list the same rows in
/// one order wait for each other instead of each holding a row the other
/// wants.
async fn register_rows(
    db: &impl ConnectionTrait,
    plugins: &[(String, Option<String>)],
) -> Result<(), DbErr> {
    // A bundle directory whose name is not a valid id is never a plugin.
    let plugins: std::collections::BTreeMap<&String, &Option<String>> = plugins
        .iter()
        .filter(|(id, _)| {
            PluginId::parse(id.as_str())
                .inspect_err(|e| tracing::warn!(error = %e, "Ignoring a bundled plugin directory"))
                .is_ok()
        })
        .map(|(id, locale)| (id, locale))
        .collect();
    if plugins.is_empty() {
        return Ok(());
    }
    let insert = |enabled: bool, conflict: &str| {
        let rows: Vec<String> = (0..plugins.len())
            .map(|i| format!("(${}, {enabled}, 0, ${})", 2 * i + 1, 2 * i + 2))
            .collect();
        let values = plugins
            .iter()
            .flat_map(|(id, locale)| [id.as_str().into(), locale.as_deref().into()]);
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "INSERT INTO plugin (id, is_enabled, generation, host_locale) VALUES {} \
                 ON CONFLICT {conflict} DO NOTHING RETURNING id",
                rows.join(", ")
            ),
            values.collect::<Vec<sea_orm::Value>>(),
        )
    };
    // Enabled where nothing conflicts: neither the id nor, for a pack, the
    // locale an enabled plugin provides (the locale index).
    db.query_all_raw(insert(true, "")).await?;
    // What is still missing was kept out by a taken locale.
    for row in db.query_all_raw(insert(false, "(id)")).await? {
        let id: String = row.try_get("", "id")?;
        tracing::warn!(
            plugin_id = %id,
            locale = ?plugins.iter().find(|(p, _)| p.as_str() == id).and_then(|(_, l)| l.as_deref()),
            "Registered a language pack disabled: another enabled plugin provides its locale"
        );
    }
    Ok(())
}

/// What [`register_rows`] records for each of `ids` registered here: the id
/// and the locale its manifest provides as a language pack.
fn registrations(state: &AppState, ids: &[String]) -> Vec<(String, Option<String>)> {
    ids.iter()
        .map(|id| (id.clone(), registered_host_locale(state, id)))
        .collect()
}

/// Pick up plugin directories added to this replica's disk. Returns whether
/// any were found.
async fn register_new_dirs(state: &AppState) -> Result<bool, DbErr> {
    let new_ids = state.plugins.rediscover_plugins().unwrap_or_else(|e| {
        tracing::error!(error = %e, "Failed to rediscover plugins");
        Vec::new()
    });
    register_rows(&state.db, &registrations(state, &new_ids)).await?;
    Ok(!new_ids.is_empty())
}

/// The locale `plugin_id`'s manifest registered here provides as a
/// language pack.
fn registered_host_locale(state: &AppState, plugin_id: &str) -> Option<String> {
    state
        .plugins
        .get_registry()
        .read()
        .ok()?
        .get(plugin_id)?
        .manifest
        .host_locale()
        .map(str::to_string)
}

/// What the plugin loaded here governs.
async fn loaded_governs(state: &AppState, plugin_id: &str) -> Governs {
    let visibility = state
        .plugins
        .get_registry()
        .read()
        .ok()
        .and_then(|r| r.get(plugin_id).map(|e| declares_visibility(&e.manifest)))
        .unwrap_or(false);
    let mut unchecked_problem_types: Vec<String> = state
        .registries
        .evaluator_registry
        .read()
        .await
        .registered_by(plugin_id)
        .into_iter()
        .filter(|(_, evaluator)| !evaluator.uses_checker)
        .map(|(problem_type, _)| problem_type)
        .collect();
    unchecked_problem_types.sort();
    let (hooks, judging_inputs) = state
        .plugins
        .get_registry()
        .read()
        .ok()
        .and_then(|r| {
            r.get(plugin_id).map(|e| {
                (
                    declared_hooks(&e.manifest),
                    declared_judging_inputs(&e.manifest),
                )
            })
        })
        .unwrap_or_default();
    Governs {
        visibility,
        provides: Provides::registered_by(&state.registries, plugin_id).await,
        unchecked_problem_types,
        hooks,
        judging_inputs,
    }
}

/// Record what the published version of a plugin, just loaded here at
/// `generation`, governs, if that changed: the first load of a bundled
/// plugin, which no change published, records it this way. Fenced on the
/// generation, so a replica applying an older generation never overwrites
/// what a newer change published with it ([`publish`]). Best-effort: a
/// stale record only makes a peer that cannot load the plugin fail closed
/// for a slightly different scope.
async fn record_governs(state: &AppState, plugin_id: &str, generation: i64) {
    let governs = loaded_governs(state, plugin_id).await;
    if state.plugin_lifecycle.governs(plugin_id).as_ref() == Some(&governs) {
        return;
    }
    let Ok(value) = serde_json::to_value(&governs) else {
        return;
    };
    let result = state
        .db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE plugin SET governs = $2 \
              WHERE id = $1 AND generation = $3 AND governs IS DISTINCT FROM $2",
            [plugin_id.into(), value.into(), generation.into()],
        ))
        .await;
    match result {
        Ok(done) if done.rows_affected() > 0 => {
            state.plugin_lifecycle.set_governs(plugin_id, governs)
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(plugin_id, error = %e, "Failed to record what a plugin governs"),
    }
}

/// Whether a manifest declares a visibility query.
pub fn declares_visibility(manifest: &plugin_core::manifest::PluginManifest) -> bool {
    manifest
        .server
        .as_ref()
        .is_some_and(|s| s.queries.iter().any(|q| q.topic == "visibility"))
}

/// A lifecycle change to record in the desired state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Change<'a> {
    /// Enable (creating the row if needed). Refused if another replica
    /// published a different source meanwhile, so an enable never undoes an
    /// upload it did not load.
    Enable,
    /// Disable. Refused unless the plugin is enabled, checked in the same
    /// statement so it sees every replica's latest change. It changes only
    /// `is_enabled` and keeps whichever source is published, so it cannot
    /// undo another change.
    Disable,
    /// Re-apply an enabled plugin. Never enables: if another admin disabled
    /// the plugin meanwhile, the reload is refused instead of undoing that;
    /// likewise if the published source changed.
    Reload,
    /// Publish an uploaded archive as the plugin's source, and enable it.
    /// Carries what the uploader saw published when it checked the archive
    /// (`None`: no such plugin yet), and is refused if that changed since.
    Upload(Option<&'a PluginSource>),
    /// Remove the uploaded archive with this hash, so the bundled copy is
    /// the source again. Refused if a different upload was published since.
    RemoveUpload(&'a str),
}

/// Record `change` for one plugin and bump its generation, so every replica
/// re-applies it. Returns the new generation, or `None` when the change was
/// refused (see [`Change`]).
///
/// `loaded` is what the serving replica loaded from `source`: its
/// fingerprint becomes the version every other replica checks its own copy
/// against, and what it governs is recorded in the same statement, so the
/// record only ever describes a published version, and so is the locale
/// it provides as a language pack: a second enabled pack for a locale breaks
/// the plugin table's unique index, which [`locale_taken`] reads. `None` (a
/// disable, or publishing a source that is not loaded) keeps the recorded
/// ones.
async fn publish(
    db: &impl ConnectionTrait,
    plugin_id: &str,
    change: Change<'_>,
    loaded: Loaded<'_>,
    source: &PluginSource,
) -> Result<Option<i64>, DbErr> {
    let version = loaded.fingerprint.map(|f| f.version.clone());
    let hash = loaded.fingerprint.map(|f| f.hash.clone());
    let governs = loaded.governs.and_then(|g| serde_json::to_value(g).ok());
    let host_locale = loaded.host_locale.map(str::to_string);
    let archive = source.archive_hash().map(str::to_string);
    let statement = match change {
        Change::Enable => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO plugin (id, is_enabled, generation, updated_at, version, content_hash, archive_hash, governs, host_locale)
               VALUES ($1, TRUE, 1, NOW(), $2, $3, $4, $5, $6)
               ON CONFLICT (id) DO UPDATE
                 SET is_enabled = TRUE,
                     generation = plugin.generation + 1,
                     updated_at = NOW(),
                     version = COALESCE(EXCLUDED.version, plugin.version),
                     content_hash = COALESCE(EXCLUDED.content_hash, plugin.content_hash),
                     governs = COALESCE(EXCLUDED.governs, plugin.governs),
                     host_locale = CASE WHEN EXCLUDED.governs IS NULL THEN plugin.host_locale
                                        ELSE EXCLUDED.host_locale END
                 WHERE plugin.archive_hash IS NOT DISTINCT FROM EXCLUDED.archive_hash
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                archive.into(),
                governs.into(),
                host_locale.into(),
            ],
        ),
        Change::Disable => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE plugin
                  SET is_enabled = FALSE, generation = generation + 1, updated_at = NOW()
               WHERE id = $1 AND is_enabled
               RETURNING generation"#,
            [plugin_id.into()],
        ),
        // Inserts only when the uploader expected a new plugin, updates only
        // when it expected exactly the source that is published. The
        // candidate row exists whenever a plugin row does, so the conflict
        // clause decides; without either, nothing is written.
        Change::Upload(replaces) => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO plugin (id, is_enabled, generation, updated_at, version, content_hash, archive_hash, governs, host_locale)
               SELECT $1, TRUE, 1, NOW(), $2, $3, $4, $7, $8
                WHERE $5 OR EXISTS (SELECT 1 FROM plugin WHERE id = $1)
               ON CONFLICT (id) DO UPDATE
                 SET is_enabled = TRUE,
                     generation = plugin.generation + 1,
                     updated_at = NOW(),
                     version = COALESCE(EXCLUDED.version, plugin.version),
                     content_hash = COALESCE(EXCLUDED.content_hash, plugin.content_hash),
                     archive_hash = EXCLUDED.archive_hash,
                     governs = COALESCE(EXCLUDED.governs, plugin.governs),
                     host_locale = CASE WHEN EXCLUDED.governs IS NULL THEN plugin.host_locale
                                        ELSE EXCLUDED.host_locale END
                 WHERE NOT $5 AND plugin.archive_hash IS NOT DISTINCT FROM $6
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                archive.into(),
                replaces.is_none().into(),
                replaces
                    .and_then(PluginSource::archive_hash)
                    .map(str::to_string)
                    .into(),
                governs.into(),
                host_locale.into(),
            ],
        ),
        Change::Reload => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE plugin
                  SET generation = generation + 1, updated_at = NOW(),
                      version = COALESCE($2, version),
                      content_hash = COALESCE($3, content_hash),
                      governs = COALESCE($5, governs),
                      host_locale = CASE WHEN $5 IS NULL THEN host_locale ELSE $6 END
               WHERE id = $1 AND is_enabled AND archive_hash IS NOT DISTINCT FROM $4
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                archive.into(),
                governs.into(),
                host_locale.into(),
            ],
        ),
        Change::RemoveUpload(removed) => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE plugin
                  SET generation = generation + 1, updated_at = NOW(), archive_hash = NULL,
                      version = COALESCE($2, version),
                      content_hash = COALESCE($3, content_hash),
                      governs = COALESCE($5, governs),
                      host_locale = CASE WHEN $5 IS NULL THEN host_locale ELSE $6 END
               WHERE id = $1 AND archive_hash = $4
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                removed.into(),
                governs.into(),
                host_locale.into(),
            ],
        ),
    };
    match db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get("", "generation")?)),
        None => Ok(None),
    }
}

/// The unique index that keeps one enabled language pack per locale
/// (migration m0016).
const LOCALE_INDEX: &str = "plugin_one_enabled_per_host_locale";

/// Whether `err` is a write refused because another enabled plugin provides
/// its row's locale ([`LOCALE_INDEX`]).
fn breaks_locale_index(err: &DbErr) -> bool {
    match err {
        DbErr::Exec(sea_orm::RuntimeErr::SqlxError(e))
        | DbErr::Query(sea_orm::RuntimeErr::SqlxError(e)) => e
            .as_database_error()
            .and_then(|db| db.constraint())
            .is_some_and(|constraint| constraint == LOCALE_INDEX),
        _ => false,
    }
}

/// The enabled plugin other than `plugin_id` that provides `locale`'s host
/// text, if any.
pub(crate) async fn locale_taken(
    db: &impl ConnectionTrait,
    plugin_id: &str,
    locale: &str,
) -> Result<Option<String>, DbErr> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM plugin WHERE is_enabled AND host_locale = $1 AND id <> $2",
            [locale.into(), plugin_id.into()],
        ))
        .await?;
    row.map(|row| row.try_get("", "id")).transpose()
}

/// What the serving replica loaded for a change it publishes.
#[derive(Debug, Clone, Copy, Default)]
struct Loaded<'a> {
    fingerprint: Option<&'a Fingerprint>,
    governs: Option<&'a Governs>,
    /// The locale its manifest provides as a language pack; recorded with
    /// `governs`.
    host_locale: Option<&'a str>,
}

/// Why a lifecycle change did not go through.
#[derive(Debug)]
enum ChangeError {
    /// The plugin could not be loaded on this replica, so nothing was
    /// published.
    Load(PluginError),
    /// Loaded here, but writing the desired state failed; this replica was
    /// returned to the old version.
    Db(DbErr),
    /// A reload of a plugin that is no longer enabled (disabled through
    /// another replica since this one last looked).
    NotEnabled(String),
    /// Another replica published a different source for the plugin since
    /// this one read it.
    Stale(String),
    /// An upload meant to add a new plugin, but a plugin with its id exists.
    AlreadyExists(String),
    /// A language pack for a locale another enabled pack provides.
    LanguagePackTaken { locale: String, by: String },
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeError::Load(e) => e.fmt(f),
            ChangeError::Db(e) => e.fmt(f),
            ChangeError::NotEnabled(id) => write!(f, "Plugin '{id}' is not enabled"),
            ChangeError::Stale(id) => write!(f, "Plugin '{id}' changed on another server"),
            ChangeError::AlreadyExists(id) => write!(f, "Plugin '{id}' already exists"),
            ChangeError::LanguagePackTaken { locale, by } => {
                write!(f, "The language pack '{by}' already provides '{locale}'")
            }
        }
    }
}

impl From<ChangeError> for AppError {
    fn from(e: ChangeError) -> Self {
        match e {
            ChangeError::Load(e) => e.into(),
            ChangeError::Db(e) => e.into(),
            ChangeError::NotEnabled(id) => {
                AppError::Conflict(format!("Plugin '{id}' is not enabled. Use enable instead."))
            }
            ChangeError::Stale(id) => AppError::Conflict(format!(
                "Plugin '{id}' was changed through another server meanwhile; refresh and try again"
            )),
            ChangeError::AlreadyExists(id) => AppError::PluginExists(id),
            ChangeError::LanguagePackTaken { locale, by } => AppError::LanguagePackTaken {
                locale,
                plugin_id: by,
            },
        }
    }
}

/// Load on this replica, then publish: the shared path behind enable,
/// reload, upload and removing an upload. A plugin that cannot load here is
/// not pushed to the fleet; the reconcile loop keeps retrying it locally
/// against the old desired state.
async fn load_then_publish(
    state: &AppState,
    applied: &mut HashMap<String, Applied>,
    id: &PluginId,
    change: Change<'_>,
    source: &PluginSource,
) -> Result<(), ChangeError> {
    let plugin_id = id.as_str();
    let before = Registered::now(state, plugin_id);
    let loaded = async {
        let root = plugin_distribution::locate(state, id, source)
            .await
            .map_err(|error| LoadFailure {
                error,
                kept_previous: false,
            })?;
        let previous = plugin_utils::load_from_dir(state, plugin_id, &root).await?;
        Ok::<_, LoadFailure>((root, previous))
    }
    .await;
    let (root, previous) = match loaded {
        Ok(loaded) => loaded,
        Err(failure) => {
            if let Some(a) = applied.get_mut(plugin_id) {
                a.retry = Some(Retry::after(a.retry, Instant::now()));
            }
            if !failure.kept_previous {
                before.restore(state, plugin_id).await;
            }
            return Err(ChangeError::Load(failure.error));
        }
    };
    let fingerprint = plugin_utils::fingerprint(&root).await;
    let governs = loaded_governs(state, plugin_id).await;
    let host_locale = registered_host_locale(state, plugin_id);
    let loaded = Loaded {
        fingerprint: fingerprint.as_ref(),
        governs: Some(&governs),
        host_locale: host_locale.as_deref(),
    };
    let published = match publish(&state.db, plugin_id, change, loaded, source).await {
        Ok(Some(generation)) => Ok(generation),
        Ok(None) => Err(refused(state, plugin_id, change).await),
        Err(e) if breaks_locale_index(&e) => {
            let locale = host_locale.clone().unwrap_or_default();
            match locale_taken(&state.db, plugin_id, &locale).await {
                Ok(Some(by)) => Err(ChangeError::LanguagePackTaken { locale, by }),
                Ok(None) => Err(ChangeError::Stale(plugin_id.to_string())),
                Err(e) => Err(ChangeError::Db(e)),
            }
        }
        Err(e) => Err(ChangeError::Db(e)),
    };
    let generation = match published {
        Ok(generation) => generation,
        Err(e) => {
            // Not published, so the rest of the fleet stays on the old
            // version: so must this replica, or it alone runs the new code.
            match previous {
                Some(previous) => plugin_utils::roll_back(state, plugin_id, previous).await,
                None => {
                    if let Err(e) = deactivate(state, plugin_id).await {
                        tracing::warn!(plugin_id, error = %e, "Failed to unload an unpublished plugin");
                    }
                    before.restore(state, plugin_id).await;
                }
            }
            return Err(e);
        }
    };
    state.plugin_lifecycle.set_governs(plugin_id, governs);
    applied.insert(
        plugin_id.to_string(),
        Applied {
            generation,
            enabled: true,
            retry: None,
            kept_previous: None,
            fingerprint,
            mismatch: None,
            unavailable: None,
        },
    );
    state.plugin_lifecycle.set_desired(plugin_id, true);
    Ok(())
}

/// Whether the fleet's desired state has `plugin_id` enabled.
async fn enabled_in_db(state: &AppState, plugin_id: &str) -> Result<bool, DbErr> {
    Ok(plugin_entity::Entity::find_by_id(plugin_id.to_string())
        .one(&state.db)
        .await?
        .is_some_and(|row| row.is_enabled))
}

/// What this replica had registered for a plugin that was not loaded, so a
/// change that publishes nothing can leave the registry as it found it
/// (a loaded plugin is put back by [`plugin_utils::roll_back`] instead).
enum Registered {
    /// Loaded, or registered from the directory a failed load used.
    Kept,
    /// Registered, unloaded, from this directory.
    Unloaded(std::path::PathBuf),
    /// Not registered at all.
    Absent,
}

impl Registered {
    fn now(state: &AppState, plugin_id: &str) -> Self {
        let Ok(registry) = state.plugins.get_registry().read() else {
            return Registered::Kept;
        };
        match registry.get(plugin_id) {
            None => Registered::Absent,
            Some(entry) if entry.status == PluginStatus::Loaded => Registered::Kept,
            Some(entry) => Registered::Unloaded(entry.root_dir.clone()),
        }
    }

    async fn restore(self, state: &AppState, plugin_id: &str) {
        match self {
            Registered::Kept => {}
            Registered::Unloaded(root) => plugin_utils::register_unloaded(state, plugin_id, &root),
            Registered::Absent => {
                purge_plugin_registrations(&state.registries, plugin_id).await;
                if let Ok(mut registry) = state.plugins.get_registry().write() {
                    registry.remove(plugin_id);
                }
            }
        }
    }
}

/// Why [`publish`] refused `change`.
async fn refused(state: &AppState, plugin_id: &str, change: Change<'_>) -> ChangeError {
    if change == Change::Reload {
        let enabled = plugin_entity::Entity::find_by_id(plugin_id)
            .one(&state.db)
            .await
            .ok()
            .flatten()
            .is_some_and(|row| row.is_enabled);
        if !enabled {
            return ChangeError::NotEnabled(plugin_id.to_string());
        }
    }
    // An upload of a new plugin is refused only when its row exists: the
    // statement inserts whenever none does.
    if change == Change::Upload(None) {
        return ChangeError::AlreadyExists(plugin_id.to_string());
    }
    ChangeError::Stale(plugin_id.to_string())
}

/// The source the fleet publishes for `plugin_id`: the row's, or this
/// replica's bundled copy for a plugin with no row yet.
async fn published_source(state: &AppState, id: &PluginId) -> Result<PluginSource, AppError> {
    let plugin_id = id.as_str();
    match plugin_entity::Entity::find_by_id(plugin_id)
        .one(&state.db)
        .await?
    {
        Some(row) => Ok(PluginSource::of(row.archive_hash.as_deref())),
        None if plugin_distribution::bundled_root(state, id).is_some() => Ok(PluginSource::Bundle),
        None => Err(AppError::NotFound(format!(
            "Plugin '{plugin_id}' not found"
        ))),
    }
}

/// A change to these plugins can change what their config checks find, so it
/// rechecks the problems those checks reach (see
/// [`crate::services::config_issues`]).
async fn recheck_config_issues(state: &AppState, plugin_ids: &[&str]) {
    crate::services::config_issues::recheck_problems_of_plugins(
        &state.db,
        &crate::services::config_issues::ConfigChecker::of(state),
        plugin_ids,
    )
    .await;
}

/// Enable a plugin fleet-wide.
///
/// Also the retry for a plugin that is enabled but failed on this replica.
pub async fn enable(state: &AppState, id: &PluginId) -> Result<(), AppError> {
    let plugin_id = id.as_str();
    let result = with_applied(state, async |applied| {
        let source = published_source(state, id).await?;
        // This replica may not have applied a peer's disable yet, so the
        // desired state comes from the database, not the local cache.
        if enabled_in_db(state, plugin_id).await?
            && state.plugins.is_plugin_loaded(plugin_id).unwrap_or(false)
        {
            return Err(AppError::Conflict(format!(
                "Plugin '{plugin_id}' is already enabled"
            )));
        }
        Ok(load_then_publish(state, applied, id, Change::Enable, &source).await?)
    })
    .await;
    if result.is_ok() {
        recheck_config_issues(state, &[plugin_id]).await;
    }
    result
}

/// Disable a plugin fleet-wide. Its pending timers are deleted: the plugin is
/// off everywhere, so they can never be delivered.
///
/// Keyed on the desired state, not on this replica's load state, so a plugin
/// that failed to load here can still be turned off.
pub async fn disable(state: &AppState, id: &PluginId) -> Result<(), AppError> {
    let plugin_id = id.as_str();
    let result = with_applied(state, async |applied| {
        let source = published_source(state, id).await?;
        let Some(generation) = publish(
            &state.db,
            plugin_id,
            Change::Disable,
            Loaded::default(),
            &source,
        )
        .await?
        else {
            return Err(AppError::Conflict(format!(
                "Plugin '{plugin_id}' is not currently enabled"
            )));
        };
        state.plugin_lifecycle.set_desired(plugin_id, false);
        let desired = Desired {
            enabled: false,
            generation,
            published: None,
            source,
        };
        apply(state, applied, id, desired, Instant::now()).await?;
        delete_timers(state, plugin_id).await;
        Ok(())
    })
    .await;
    if result.is_ok() {
        recheck_config_issues(state, &[plugin_id]).await;
    }
    result
}

/// Delete a plugin's pending timers once it is off or gone everywhere: they
/// can never be delivered. Best-effort.
async fn delete_timers(state: &AppState, plugin_id: &str) {
    if let Err(e) =
        crate::dispatcher::plugin_timer::delete_timers_for_plugin(&state.db, plugin_id).await
    {
        tracing::error!(
            plugin_id,
            error = %e,
            "Failed to delete pending timers for a plugin that is off"
        );
    }
}

/// Reload an enabled plugin from its published source, fleet-wide. This is
/// also the retry for a plugin that failed to load: it is accepted whatever
/// this replica's own load state, and a successful load here bumps the
/// generation, so every failing replica retries at once instead of waiting
/// out its backoff.
pub async fn reload(state: &AppState, id: &PluginId) -> Result<(), AppError> {
    let plugin_id = id.as_str();
    let result = with_applied(state, async |applied| {
        let source = published_source(state, id).await?;
        // Checked against the database so a peer's recent enable or disable
        // counts; `publish` re-checks atomically for a change in between.
        if !enabled_in_db(state, plugin_id).await? {
            return Err(ChangeError::NotEnabled(plugin_id.to_string()).into());
        }
        Ok(load_then_publish(state, applied, id, Change::Reload, &source).await?)
    })
    .await;
    if result.is_ok() {
        recheck_config_issues(state, &[plugin_id]).await;
    }
    result
}

/// Publish an uploaded plugin archive fleet-wide: store it in the blob
/// store, load it here, then record it as the plugin's source and enable it.
/// Every other replica fetches it from the blob store on its next poll.
///
/// `replaces` is the source the admin saw published when the archive was
/// checked (`None`: a new plugin). If another change landed since, nothing
/// is published and the upload is refused as a conflict. A load failure
/// here is returned as [`AppError::PluginLoadFailed`] and publishes nothing.
pub async fn install(
    state: &AppState,
    archive: PluginArchive,
    replaces: Option<PluginSource>,
) -> Result<(), AppError> {
    let stored = state.blob_store.put(archive.bytes()).await?;
    if stored != archive.hash() {
        return Err(AppError::Internal(format!(
            "blob store keyed the plugin archive {} as {stored}",
            archive.hash()
        )));
    }
    let id = archive.id().clone();
    let source = PluginSource::Upload(archive.hash().to_hex());
    let result = with_applied(state, async |applied| {
        // Unpack from the bytes in hand rather than fetching them back.
        let stored = plugin_distribution::store(state, archive).await;
        let result = match stored {
            Ok(_) => {
                let change = Change::Upload(replaces.as_ref());
                load_then_publish(state, applied, &id, change, &source).await
            }
            Err(e) => Err(ChangeError::Load(e)),
        };
        refresh_cache(state).await;
        result.map_err(|e| match e {
            ChangeError::Load(e) => AppError::PluginLoadFailed(e.to_string()),
            other => other.into(),
        })
    })
    .await;
    if result.is_ok() {
        recheck_config_issues(state, &[id.as_str()]).await;
    }
    result
}

/// [`retain_cache`] against the rows as they are now.
async fn refresh_cache(state: &AppState) {
    match plugin_entity::Entity::find().all(&state.db).await {
        Ok(rows) => retain_cache(state, published_archives(&rows)).await,
        Err(e) => tracing::warn!(error = %e, "Skipped plugin cache cleanup"),
    }
}

/// What removing a plugin's upload does.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum UploadRemoval {
    /// The bundled copy is the plugin's source again.
    BundleRestored,
    /// No bundled copy exists, so the plugin is gone from the fleet.
    Removed,
}

/// Remove a plugin's uploaded version fleet-wide. With a bundled copy on
/// this replica, that copy becomes the source again (loaded here first if
/// the plugin is enabled); without one, the plugin's row is deleted, which
/// unloads it on every replica, and its pending timers go with it.
///
/// `expect` is the outcome the admin confirmed. Which one happens depends on
/// this replica's bundle, so if it differs nothing is changed and the call
/// is refused as a conflict.
pub async fn remove_upload(
    state: &AppState,
    id: &PluginId,
    expect: UploadRemoval,
) -> Result<UploadRemoval, AppError> {
    let plugin_id = id.as_str();
    let result = with_applied(state, async |applied| {
        let row = plugin_entity::Entity::find_by_id(plugin_id)
            .one(&state.db)
            .await?
            .ok_or_else(|| AppError::NotFound(format!("Plugin '{plugin_id}' not found")))?;
        let Some(archive) = row.archive_hash.clone() else {
            return Err(AppError::Conflict(format!(
                "Plugin '{plugin_id}' has no uploaded version"
            )));
        };
        let bundled = plugin_distribution::bundled_root(state, id);
        let would = match bundled {
            Some(_) => UploadRemoval::BundleRestored,
            None => UploadRemoval::Removed,
        };
        if would != expect {
            return Err(AppError::Conflict(match would {
                UploadRemoval::BundleRestored => format!(
                    "This server has a bundled copy of '{plugin_id}', so removing the upload \
                     would restore it rather than remove the plugin; confirm again"
                ),
                UploadRemoval::Removed => format!(
                    "This server has no bundled copy of '{plugin_id}', so removing the upload \
                     would remove the plugin from every server; confirm again"
                ),
            }));
        }
        let change = Change::RemoveUpload(&archive);
        let stale = || ChangeError::Stale(plugin_id.to_string());
        let outcome = match bundled {
            Some(_) if row.is_enabled => {
                load_then_publish(state, applied, id, change, &PluginSource::Bundle).await?;
                UploadRemoval::BundleRestored
            }
            Some(root) => {
                let fingerprint = plugin_utils::fingerprint(&root).await;
                let loaded = Loaded {
                    fingerprint: fingerprint.as_ref(),
                    governs: None,
                    host_locale: None,
                };
                let generation = publish(&state.db, plugin_id, change, loaded, &PluginSource::Bundle)
                .await?
                .ok_or_else(stale)?;
                let desired = Desired {
                    enabled: false,
                    generation,
                    published: fingerprint,
                    source: PluginSource::Bundle,
                };
                apply(state, applied, id, desired, Instant::now()).await?;
                UploadRemoval::BundleRestored
            }
            None => {
                let deleted = state
                    .db
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        "DELETE FROM plugin WHERE id = $1 AND archive_hash = $2",
                        [plugin_id.into(), archive.clone().into()],
                    ))
                    .await?;
                if deleted.rows_affected() == 0 {
                    return Err(stale().into());
                }
                forget(state, applied, plugin_id).await;
                delete_timers(state, plugin_id).await;
                if let Err(e) = crate::plugin_db::schema::retire(&state.db, plugin_id).await {
                    tracing::warn!(plugin_id, error = %e, "Could not retire the plugin's database role");
                }
                UploadRemoval::Removed
            }
        };
        refresh_cache(state).await;
        Ok(outcome)
    })
    .await;
    if result.is_ok() {
        recheck_config_issues(state, &[plugin_id]).await;
    }
    result
}

/// Reload every enabled plugin (including ones that failed to load here)
/// and pick up new plugin directories in the bundle, fleet-wide.
pub async fn reload_all(state: &AppState) -> Result<ReloadAllResponse, AppError> {
    let result = with_applied(state, async |applied| {
        let rows = plugin_entity::Entity::find().all(&state.db).await?;
        let enabled: Vec<(PluginId, PluginSource)> = rows
            .iter()
            // The fleet's desired state, not this replica's cache of it, so a
            // peer's recent enable or disable counts.
            .filter(|r| r.is_enabled)
            .filter_map(|r| Some((row_id(r)?, PluginSource::of(r.archive_hash.as_deref()))))
            .collect();

        let mut reloaded = Vec::new();
        let mut failed = Vec::new();
        for (id, source) in enabled {
            match load_then_publish(state, applied, &id, Change::Reload, &source).await {
                Ok(()) => reloaded.push(id.to_string()),
                Err(e) => {
                    tracing::error!(plugin_id = %id, error = %e, "Failed to reload plugin");
                    failed.push(ReloadFailure {
                        id: id.to_string(),
                        error: e.to_string(),
                    });
                }
            }
        }

        let new_ids = state.plugins.rediscover_plugins().unwrap_or_else(|e| {
            tracing::error!("Failed to rediscover plugins: {}", e);
            Vec::new()
        });
        register_rows(&state.db, &registrations(state, &new_ids)).await?;
        // A new row starts enabled unless an operator already recorded it as
        // disabled; apply whatever the table says.
        let rows = plugin_entity::Entity::find().all(&state.db).await?;
        let now = Instant::now();
        for row in rows.into_iter().filter(|r| new_ids.contains(&r.id)) {
            let Some(id) = row_id(&row) else { continue };
            let desired = Desired::of(&row);
            if let Err(e) = apply(state, applied, &id, desired, now).await {
                tracing::error!(plugin_id = %row.id, error = %e, "Failed to load new plugin");
                failed.push(ReloadFailure {
                    id: row.id.clone(),
                    error: e.to_string(),
                });
            }
            state.plugin_lifecycle.set_desired(&row.id, row.is_enabled);
        }

        Ok(ReloadAllResponse {
            reloaded,
            new: new_ids,
            failed,
        })
    })
    .await;
    if let Ok(response) = &result {
        let ids: Vec<&str> = response
            .reloaded
            .iter()
            .chain(&response.new)
            .map(String::as_str)
            .collect();
        recheck_config_issues(state, &ids).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One enabled pack provides a locale: a new pack for a locale an enabled
    /// plugin provides is registered disabled, and of two new packs for one
    /// locale only one is enabled, whatever order they are listed in.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_language_pack_for_a_taken_locale_is_registered_disabled() {
        let (_container, db) = migrated_database().await;
        let pack = |id: &str, locale: &str| (id.to_string(), Some(locale.to_string()));
        register_rows(
            &db,
            &[
                pack("en-b", "en"),
                ("other".to_string(), None),
                pack("en-a", "en"),
            ],
        )
        .await
        .expect("register");
        register_rows(&db, &[pack("en-c", "en"), pack("zh", "zh-CN")])
            .await
            .expect("register again");
        let mut rows: Vec<(String, bool)> = plugin_entity::Entity::find()
            .all(&db)
            .await
            .expect("rows")
            .into_iter()
            .map(|row| (row.id, row.is_enabled))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            [
                ("en-a".to_string(), true),
                ("en-b".to_string(), false),
                ("en-c".to_string(), false),
                ("other".to_string(), true),
                ("zh".to_string(), true),
            ]
        );
    }

    /// `plugin.governs` has the one shape [`Governs`] reads: a row missing a
    /// field or carrying another is refused when it is written, so the SQL
    /// that reads the column and the struct never read a row differently.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn governs_is_stored_only_in_the_shape_both_readers_share() {
        let (_container, db) = migrated_database().await;
        let write = |governs: serde_json::Value| {
            db.execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO plugin (id, is_enabled, generation, governs) VALUES ('p', TRUE, 0, $1) \
                 ON CONFLICT (id) DO UPDATE SET governs = EXCLUDED.governs",
                [governs.into()],
            ))
        };
        for partial in [
            serde_json::json!({"visibility": false, "contest_types": ["x"]}),
            serde_json::json!({
                "visibility": false, "contest_types": [], "problem_types": [], "checker_formats": [],
                "languages": [], "unchecked_problem_types": [], "hooks": [], "judging_inputs": [], "host_locale": null,
            }),
            serde_json::json!({
                "visibility": "no", "contest_types": [], "problem_types": [], "checker_formats": [],
                "languages": [], "unchecked_problem_types": [], "hooks": [], "judging_inputs": [],
            }),
            serde_json::json!([]),
        ] {
            assert!(
                write(partial.clone()).await.is_err(),
                "{partial} was stored"
            );
        }
        let whole = Governs {
            visibility: true,
            provides: Provides {
                contest_types: vec!["x".into()],
                problem_types: vec!["batch".into(), "communication".into()],
                checker_formats: vec!["lines".into()],
                languages: vec!["cpp".into()],
            },
            unchecked_problem_types: vec!["communication".into()],
            hooks: vec![crate::plugin_lifecycle::HookTopic {
                topic: "after_submission".into(),
                scope: plugin_core::hook::HookScope::Global,
            }],
            judging_inputs: vec!["checker".into()],
        };
        write(serde_json::to_value(&whole).unwrap())
            .await
            .expect("the whole shape is stored");
        let stored = plugin_entity::Entity::find_by_id("p")
            .one(&db)
            .await
            .expect("row")
            .and_then(|row| row.governs)
            .expect("governs");
        assert_eq!(serde_json::from_value::<Governs>(stored).unwrap(), whole);
    }

    /// A migrated database in a container of its own.
    async fn migrated_database() -> (
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        common::db::DatabaseConnection,
    ) {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;

        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        crate::database::BootSession::acquire(&url)
            .await
            .expect("boot session")
            .migrate()
            .await
            .expect("migrate");
        let db = sea_orm::Database::connect(&url)
            .await
            .expect("connect")
            .into();
        (container, db)
    }

    /// A lifecycle where `judge` provides contest type `t`, `eval` the
    /// evaluator of problem type `p` and `check` the checker of format `c`,
    /// enabled as given.
    fn providers(judge_enabled: bool, eval_enabled: bool, check_enabled: bool) -> PluginLifecycle {
        let lifecycle = PluginLifecycle::default();
        let provides = |provides: Provides| Governs {
            provides,
            ..Governs::default()
        };
        lifecycle.set_governs(
            "judge",
            provides(Provides {
                contest_types: vec!["t".into()],
                ..Provides::default()
            }),
        );
        lifecycle.set_governs(
            "eval",
            provides(Provides {
                problem_types: vec!["p".into()],
                ..Provides::default()
            }),
        );
        lifecycle.set_governs(
            "check",
            provides(Provides {
                checker_formats: vec!["c".into()],
                ..Provides::default()
            }),
        );
        lifecycle.set_desired("judge", judge_enabled);
        lifecycle.set_desired("eval", eval_enabled);
        lifecycle.set_desired("check", check_enabled);
        lifecycle
    }

    /// A replica is ready unless a live peer serves every contest type it
    /// serves on its fleet-wide provider and more: then a gateway sends its
    /// browsers to a replica that takes their submissions. Replicas that
    /// each lack a different type, and a type no replica has loaded, leave
    /// every replica ready, so the gateways always keep one.
    #[test]
    fn a_replica_is_unready_only_while_a_peer_serves_strictly_more_contest_types() {
        let lifecycle = PluginLifecycle::default();
        for (plugin, types) in [
            ("judge1", vec!["t1"]),
            ("judge2", vec!["t2"]),
            ("zz", vec!["t2"]),
        ] {
            lifecycle.set_governs(
                plugin,
                Governs {
                    provides: Provides {
                        contest_types: types.into_iter().map(String::from).collect(),
                        ..Provides::default()
                    },
                    ..Governs::default()
                },
            );
            lifecycle.set_desired(plugin, true);
        }
        let loaded = |types: &[(&str, &str)]| {
            let mut loaded = crate::providers::Loaded::default();
            for (key, plugin) in types {
                loaded
                    .of_mut(Provided::ContestType)
                    .entry(key.to_string())
                    .or_default()
                    .push(plugin.to_string());
            }
            loaded
        };
        let here = loaded(&[("t1", "judge1")]);
        assert_eq!(
            lifecycle.missing_here(Provided::ContestType, &here),
            [("t2".to_string(), "judge2".to_string())].into()
        );

        assert!(lifecycle.ready(&here), "no peer known yet");
        lifecycle.set_fleet_peers(vec![loaded(&[("t1", "judge1"), ("t2", "judge2")])]);
        assert!(!lifecycle.ready(&here), "a peer serves t2 as well");
        assert!(lifecycle.ready(&loaded(&[("t1", "judge1"), ("t2", "judge2")])));

        // t2 on a provider that is not its fleet-wide one serves nothing more.
        lifecycle.set_fleet_peers(vec![loaded(&[("t1", "judge1"), ("t2", "zz")])]);
        assert!(lifecycle.ready(&here));
        // Each lacks what the other has: neither serves more.
        lifecycle.set_fleet_peers(vec![loaded(&[("t2", "judge2")])]);
        assert!(lifecycle.ready(&here));
        assert!(lifecycle.ready(&loaded(&[("t2", "judge2")])));
    }

    /// A problem's submit is closed for each language whose providers are
    /// all disabled, and closed outright, by the first of them, once no
    /// recorded language is left open; what it needs besides a language
    /// closes it first.
    #[test]
    fn a_problem_is_closed_per_language_and_outright_once_no_language_is_open() {
        use broccoli_api_types::visibility::ClosedLanguage;
        let lifecycle = providers(true, true, true);
        let languages = |languages: &[&str]| Governs {
            provides: Provides {
                languages: languages.iter().map(|l| l.to_string()).collect(),
                ..Provides::default()
            },
            ..Governs::default()
        };
        lifecycle.set_governs("langs-a", languages(&["cpp", "python3"]));
        lifecycle.set_governs("langs-b", languages(&["java"]));
        lifecycle.set_desired("langs-a", false);
        lifecycle.set_desired("langs-b", true);
        let needs = lifecycle.needs("t", "p", "c", None);
        let closed = |language: &str, plugin_id: &str| ClosedLanguage {
            language: language.into(),
            closed_by: ClosedBy {
                plugin_id: plugin_id.into(),
                provides: Provided::Language,
                reason: broccoli_api_types::visibility::ClosedReason::Disabled,
            },
        };

        let some = lifecycle.problem_closure(needs, None);
        assert_eq!(some.closed_by, None);
        assert_eq!(
            some.closed_languages,
            vec![closed("cpp", "langs-a"), closed("python3", "langs-a")]
        );

        lifecycle.set_desired("langs-b", false);
        let all = lifecycle.problem_closure(needs, None);
        assert_eq!(
            all.closed_by,
            Some(ClosedBy {
                plugin_id: "langs-a".into(),
                provides: Provided::Language,
                reason: broccoli_api_types::visibility::ClosedReason::Disabled,
            })
        );
        assert_eq!(all.closed_languages.len(), 3);

        // A problem that names its languages is closed by those alone.
        lifecycle.set_desired("langs-b", true);
        let java_only = HashSet::from(["java"]);
        let named = lifecycle.problem_closure(needs, Some(&java_only));
        assert_eq!((named.closed_by, named.closed_languages), (None, vec![]));
        let cpp_only = HashSet::from(["cpp"]);
        assert_eq!(
            lifecycle.problem_closure(needs, Some(&cpp_only)).closed_by,
            Some(ClosedBy {
                plugin_id: "langs-a".into(),
                provides: Provided::Language,
                reason: broccoli_api_types::visibility::ClosedReason::Disabled,
            })
        );

        // The contest type closes it before any language does.
        lifecycle.set_desired("judge", false);
        assert_eq!(
            lifecycle.problem_closure(needs, None).closed_by,
            Some(ClosedBy {
                plugin_id: "judge".into(),
                provides: Provided::ContestType,
                reason: broccoli_api_types::visibility::ClosedReason::Disabled,
            })
        );
    }

    /// A key resolves to the enabled plugin providing it whose id sorts
    /// first, whatever is loaded here: a replica with only a later provider
    /// loaded does not run the key on it. With every provider disabled the
    /// key is closed, and a key no plugin recorded resolves locally.
    #[test]
    fn a_key_resolves_to_its_fleet_wide_provider_only() {
        use crate::providers::{FleetProvider, Unresolved, resolve};
        use crate::registry::{ContestTypeHandlers, Providers};
        let lifecycle = PluginLifecycle::default();
        for id in ["a-contest", "b-contest"] {
            lifecycle.set_governs(
                id,
                Governs {
                    provides: Provides {
                        contest_types: vec!["t".into()],
                        ..Provides::default()
                    },
                    ..Governs::default()
                },
            );
            lifecycle.set_desired(id, true);
        }
        let mut here: Providers<ContestTypeHandlers> = Providers::default();
        for key in ["t", "unrecorded"] {
            here.insert(
                key.into(),
                ContestTypeHandlers {
                    plugin_id: "b-contest".into(),
                    submission_fn: "s".into(),
                    code_run_fn: "r".into(),
                    submission_admission: false,
                },
            );
        }
        let kind = Provided::ContestType;
        let resolved = |key: &str| {
            resolve(&here, lifecycle.provider(kind, key), kind, key).map(|h| h.plugin_id)
        };

        assert_eq!(
            lifecycle.provider(kind, "t"),
            FleetProvider::Plugin("a-contest".into())
        );
        assert_eq!(
            resolved("t"),
            Err(Unresolved {
                kind,
                key: "t".into(),
                provider: Some("a-contest".into()),
            }),
            "b-contest is loaded here, but a-contest provides the key"
        );
        lifecycle.set_desired("a-contest", false);
        assert_eq!(resolved("t"), Ok("b-contest".into()));
        lifecycle.set_desired("b-contest", false);
        assert_eq!(lifecycle.provider(kind, "t"), FleetProvider::Closed);
        assert_eq!(
            lifecycle.provider(kind, "unrecorded"),
            FleetProvider::Unknown
        );
        assert_eq!(resolved("unrecorded"), Ok("b-contest".into()));
    }

    /// Work needs exactly the plugins it uses: its problem's checker only
    /// when the problem type's fleet-wide evaluator declared it uses one,
    /// and its language's resolver. A disabled checker closes only work
    /// whose evaluator checks with it; a disabled language plugin closes
    /// work in that language.
    #[test]
    fn work_needs_a_checker_only_when_its_evaluator_uses_one() {
        let lifecycle = providers(true, true, false);
        let checked = lifecycle.needs("t", "p", "c", Some("cpp"));
        assert_eq!(checked.checker_format, Some("c"));
        assert_eq!(
            lifecycle.closed_by(checked).map(|c| c.provides),
            Some(Provided::CheckerFormat)
        );

        lifecycle.set_governs(
            "eval",
            Governs {
                provides: Provides {
                    problem_types: vec!["p".into()],
                    ..Provides::default()
                },
                unchecked_problem_types: vec!["p".into()],
                ..Governs::default()
            },
        );
        let unchecked = lifecycle.needs("t", "p", "c", Some("cpp"));
        assert_eq!(unchecked.checker_format, None);
        assert_eq!(lifecycle.closed_by(unchecked), None);

        lifecycle.set_governs(
            "langs",
            Governs {
                provides: Provides {
                    languages: vec!["cpp".into()],
                    ..Provides::default()
                },
                ..Governs::default()
            },
        );
        lifecycle.set_desired("langs", false);
        assert_eq!(
            lifecycle.closed_by(unchecked),
            Some(ClosedBy {
                plugin_id: "langs".into(),
                provides: Provided::Language,
                reason: broccoli_api_types::visibility::ClosedReason::Disabled,
            })
        );
        assert_eq!(
            lifecycle.closed_by(lifecycle.needs("t", "p", "c", None)),
            None
        );
    }

    /// Work whose key's fleet-wide provider is enabled but loaded on no live
    /// replica waits for it, closed by that provider as not loaded, the way
    /// work of a disabled provider waits: it never takes another provider of
    /// the key, and it is open again once a replica has the provider loaded.
    #[test]
    fn work_whose_provider_is_loaded_nowhere_is_closed_by_it_until_one_loads_it() {
        use broccoli_api_types::visibility::ClosedReason;
        let lifecycle = providers(true, true, true);
        let needs = lifecycle.needs("t", "p", "c", None);
        let mut fleet = crate::providers::Loaded::default();
        for (kind, key, plugin) in [
            (Provided::ContestType, "t", "judge"),
            (Provided::CheckerFormat, "c", "check"),
        ] {
            fleet
                .of_mut(kind)
                .insert(key.to_string(), vec![plugin.to_string()]);
        }
        lifecycle.set_fleet_loaded(fleet.clone());
        assert_eq!(
            lifecycle.closed_by(needs),
            Some(ClosedBy {
                plugin_id: "eval".into(),
                provides: Provided::ProblemType,
                reason: ClosedReason::NotLoaded,
            })
        );
        assert_eq!(
            lifecycle.parked_by(needs, common::SubmissionStatus::Queued),
            lifecycle.closed_by(needs)
        );
        fleet
            .of_mut(Provided::ProblemType)
            .insert("p".into(), vec!["eval".into()]);
        lifecycle.set_fleet_loaded(fleet);
        assert_eq!(lifecycle.closed_by(needs), None);
    }

    /// What is stored, checked and hooked per plugin follows the plugin a
    /// key runs on: a contest type's owner, for its configs, config issues
    /// and hooks, is its fleet-wide provider even on a replica that has only
    /// another provider loaded, and that other provider owns nothing of it.
    #[tokio::test]
    async fn a_types_owner_is_its_fleet_wide_provider_on_every_replica() {
        use crate::registry::{ContestTypeHandlers, Providers, TypeProviders};
        let lifecycle = std::sync::Arc::new(PluginLifecycle::default());
        for id in ["a-contest", "b-contest"] {
            lifecycle.set_governs(
                id,
                Governs {
                    provides: Provides {
                        contest_types: vec!["t".into()],
                        ..Provides::default()
                    },
                    ..Governs::default()
                },
            );
            lifecycle.set_desired(id, true);
        }
        let mut here: Providers<ContestTypeHandlers> = Providers::default();
        here.insert(
            "t".into(),
            ContestTypeHandlers {
                plugin_id: "b-contest".into(),
                submission_fn: "s".into(),
                code_run_fn: "r".into(),
                submission_admission: false,
            },
        );
        let types = TypeProviders::new(
            std::sync::Arc::new(tokio::sync::RwLock::new(here)),
            Default::default(),
            Default::default(),
            lifecycle.clone(),
        );
        assert_eq!(
            types.of_contest_type("t").await.as_deref(),
            Some("a-contest")
        );
        assert_eq!(types.provided_by("a-contest").await.contest_types, ["t"]);
        assert!(
            types
                .provided_by("b-contest")
                .await
                .contest_types
                .is_empty()
        );

        lifecycle.set_desired("a-contest", false);
        assert_eq!(
            types.of_contest_type("t").await.as_deref(),
            Some("b-contest")
        );
    }

    #[test]
    fn closed_work_names_the_plugin_and_what_it_provides_for_the_work() {
        let queued = common::SubmissionStatus::Queued;
        let needs = Needs {
            contest_type: "t",
            problem_type: "p",
            checker_format: Some("c"),
            language: None,
        };
        let closed = |provides, plugin_id: &str| {
            Some(ClosedBy {
                plugin_id: plugin_id.into(),
                provides,
                reason: broccoli_api_types::visibility::ClosedReason::Disabled,
            })
        };
        assert_eq!(providers(true, true, true).closed_by(needs), None);
        assert_eq!(
            providers(true, false, true).parked_by(needs, queued),
            closed(Provided::ProblemType, "eval"),
        );
        assert_eq!(
            providers(true, true, false).parked_by(needs, queued),
            closed(Provided::CheckerFormat, "check"),
        );
        // The contest type is what work of it needs first.
        assert_eq!(
            providers(false, false, false).parked_by(needs, queued),
            closed(Provided::ContestType, "judge"),
        );
        // Finished work is parked by nothing.
        assert_eq!(
            providers(false, false, false).parked_by(needs, common::SubmissionStatus::Judged),
            None,
        );
    }

    /// Two replicas booting together register overlapping plugin rows. One
    /// holds the row of `a` (its insert has not committed) while the other
    /// registers `b` and `a`, listed in the order a hash map gave them: the
    /// second waits on `a` before it touches `b`, so the first registering
    /// `b` next goes through instead of deadlocking, and both finish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replicas_registering_the_same_plugins_together_never_deadlock() {
        use sea_orm::TransactionTrait;
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;

        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        crate::database::BootSession::acquire(&url)
            .await
            .expect("boot session")
            .migrate()
            .await
            .expect("migrate");
        let connect = || async {
            common::db::DatabaseConnection::from(
                sea_orm::Database::connect(&url).await.expect("connect"),
            )
        };
        let (first, second, observer) = (connect().await, connect().await, connect().await);

        let holding = first.begin().await.expect("begin");
        register_rows(&holding, &[("a".to_string(), None)])
            .await
            .expect("insert");
        let registering = tokio::spawn(async move {
            register_rows(&second, &[("b".to_string(), None), ("a".to_string(), None)]).await
        });
        // The second replica waits on the row the first holds.
        loop {
            let waiting = observer
                .query_one_raw(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE wait_event_type = 'Lock' AND query LIKE 'INSERT INTO plugin %'",
                ))
                .await
                .expect("activity")
                .expect("row")
                .try_get_by_index::<i64>(0)
                .expect("count");
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        holding
            .execute_unprepared("SET LOCAL lock_timeout = '10s'")
            .await
            .expect("lock timeout");
        register_rows(&holding, &[("b".to_string(), None)])
            .await
            .expect("insert");
        holding.commit().await.expect("commit");
        registering
            .await
            .expect("join")
            .expect("the second replica registers too");
        assert_eq!(
            plugin_entity::Entity::find()
                .all(&observer)
                .await
                .expect("rows")
                .len(),
            2
        );
    }

    #[test]
    fn retry_delay_doubles_and_is_capped() {
        assert_eq!(retry_delay(1), Duration::from_secs(5));
        assert_eq!(retry_delay(2), Duration::from_secs(10));
        assert_eq!(retry_delay(3), Duration::from_secs(20));
        assert_eq!(retry_delay(7), RETRY_MAX);
        assert_eq!(retry_delay(u32::MAX), RETRY_MAX);
    }
}
