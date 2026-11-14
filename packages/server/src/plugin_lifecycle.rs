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

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use plugin_core::error::PluginError;
use plugin_core::registry::PluginStatus;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, EntityTrait, Set, Statement};

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
    /// This replica's latest report (also read by [`not_ready`]), and when it
    /// was last written to the `plugin_replica` row, so an unchanged report
    /// is only re-touched every [`plugin_fleet::TOUCH_INTERVAL`].
    reported: std::sync::Mutex<Option<Reported>>,
    /// The recorded [`Governs`] per plugin, as of the last reconcile.
    governs: RwLock<HashMap<String, Governs>>,
    /// Set by [`stop`] at shutdown: no further report writes.
    stopped: std::sync::atomic::AtomicBool,
}

struct Reported {
    report: ReplicaReport,
    contest_types: Vec<String>,
    /// `None` until a write of an equal report succeeds.
    written_at: Option<Instant>,
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
/// have decided, instead of for everything or nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Governs {
    /// It declares a `[[server.queries]] topic = "visibility"`.
    pub visibility: bool,
    /// The contest types its `init()` registered, sorted.
    pub contest_types: Vec<String>,
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
    let factor = 1u32 << attempts.saturating_sub(1).min(16);
    RETRY_BASE.saturating_mul(factor).min(RETRY_MAX)
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

    /// Whether `contest_type` is provided only by plugins the fleet has
    /// disabled: some disabled plugin last registered it and no enabled one
    /// did. A type no plugin ever registered is not disabled, just unknown.
    pub fn is_contest_type_disabled(&self, contest_type: &str) -> bool {
        let Ok(governs) = self.governs.read() else {
            return false;
        };
        let mut providers = governs
            .iter()
            .filter(|(_, g)| g.contest_types.iter().any(|t| t == contest_type))
            .map(|(id, _)| self.is_desired_enabled(id))
            .peekable();
        providers.peek().is_some() && providers.all(|enabled| !enabled)
    }

    pub(crate) fn set_desired(&self, plugin_id: &str, enabled: bool) {
        if let Ok(mut d) = self.desired.write() {
            d.insert(plugin_id.to_string(), enabled);
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
/// (they are retried by [`run`]).
pub async fn boot(state: &AppState) -> anyhow::Result<Vec<PluginActivationFailure>> {
    state.plugins.discover_plugins()?;
    let ids: Vec<String> = state
        .plugins
        .list_plugins()?
        .into_iter()
        .map(|p| p.id)
        .collect();
    register_rows(state, &ids).await?;
    Ok(with_applied(state, async |applied| {
        reconcile_locked(state, applied).await
    })
    .await?)
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

/// Record the report in memory, and write it if it changed or touch it if
/// the last write is getting old. Best-effort: a failed write only makes the
/// admin view stale.
async fn write_report(state: &AppState, applied: &HashMap<String, Applied>) {
    let now = Instant::now();
    let report = build_report(state, applied, now);
    let contest_types = crate::dispatcher::local_contest_types(state).await;
    if !record_report(state, &report, &contest_types, now) {
        return;
    }
    match plugin_fleet::report(&state.db, &state.config.server.id, &report, &contest_types).await {
        Ok(()) => {
            if let Ok(mut reported) = state.plugin_lifecycle.reported.lock()
                && let Some(last) = reported.as_mut()
                && same_report(&last.report, &report)
            {
                last.written_at = Some(now);
            }
        }
        Err(e) => tracing::warn!(error = %e, "Failed to write this replica's plugin report"),
    }
}

/// Keep `report` as this replica's latest, and say whether it is due to be
/// written: it changed, its last write is getting old, or no write of it has
/// succeeded yet. Never due once [`stop`] ran.
fn record_report(
    state: &AppState,
    report: &ReplicaReport,
    contest_types: &[String],
    now: Instant,
) -> bool {
    let Ok(mut reported) = state.plugin_lifecycle.reported.lock() else {
        return false;
    };
    let unchanged_at = reported
        .as_ref()
        .filter(|last| same_report(&last.report, report) && last.contest_types == contest_types)
        .and_then(|last| last.written_at);
    *reported = Some(Reported {
        report: report.clone(),
        contest_types: contest_types.to_vec(),
        written_at: unchanged_at,
    });
    unchanged_at.is_none_or(|at| now.duration_since(at) >= plugin_fleet::TOUCH_INTERVAL)
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
        if let Some(governs) = row
            .governs
            .clone()
            .and_then(|g| serde_json::from_value::<Governs>(g).ok())
            && let Ok(mut map) = state.plugin_lifecycle.governs.write()
        {
            map.insert(row.id.clone(), governs);
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

/// Unload a plugin whose row was deleted and forget it on this replica.
async fn forget(state: &AppState, applied: &mut HashMap<String, Applied>, plugin_id: &str) {
    purge_plugin_registrations(&state.registries, plugin_id).await;
    if let Ok(mut registry) = state.plugins.get_registry().write() {
        registry.remove(plugin_id);
    }
    if let Err(e) = state.plugins.update_translations() {
        tracing::warn!(plugin_id, error = %e, "Failed to refresh translations after removing a plugin");
    }
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
    if result.is_ok() {
        record_governs(state, plugin_id).await;
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

async fn deactivate(state: &AppState, plugin_id: &str) -> Result<(), PluginError> {
    purge_plugin_registrations(&state.registries, plugin_id).await;
    state.plugins.unload_plugin(plugin_id)?;
    state.plugin_sql.forget(plugin_id);
    state.plugins.update_translations()
}

/// Record plugins found on this replica's disk that have no row yet, as
/// enabled (a newly installed plugin is on by default). Idempotent, so
/// replicas booting together cannot fail on the primary key.
async fn register_rows(state: &AppState, ids: &[String]) -> Result<(), DbErr> {
    // A bundle directory whose name is not a valid id is never a plugin.
    let ids: Vec<&String> = ids
        .iter()
        .filter(|id| {
            PluginId::parse(id.as_str())
                .inspect_err(|e| tracing::warn!(error = %e, "Ignoring a bundled plugin directory"))
                .is_ok()
        })
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    plugin_entity::Entity::insert_many(ids.into_iter().map(|id| plugin_entity::ActiveModel {
        id: Set(id.clone()),
        is_enabled: Set(true),
        generation: Set(0),
        ..Default::default()
    }))
    .on_conflict_do_nothing_on([plugin_entity::Column::Id])
    .exec(&state.db)
    .await?;
    Ok(())
}

/// Pick up plugin directories added to this replica's disk. Returns whether
/// any were found.
async fn register_new_dirs(state: &AppState) -> Result<bool, DbErr> {
    let new_ids = state.plugins.rediscover_plugins().unwrap_or_else(|e| {
        tracing::error!(error = %e, "Failed to rediscover plugins");
        Vec::new()
    });
    register_rows(state, &new_ids).await?;
    Ok(!new_ids.is_empty())
}

/// Record what a plugin that just loaded here governs, if that changed.
/// Best-effort: a stale record only makes a peer that cannot load the plugin
/// fail closed for a slightly different scope.
async fn record_governs(state: &AppState, plugin_id: &str) {
    let visibility = state
        .plugins
        .get_registry()
        .read()
        .ok()
        .and_then(|r| r.get(plugin_id).map(|e| declares_visibility(&e.manifest)))
        .unwrap_or(false);
    let mut contest_types: Vec<String> = state
        .registries
        .contest_type_registry
        .read()
        .await
        .iter()
        .filter(|(_, h)| h.plugin_id == plugin_id)
        .map(|(t, _)| t.clone())
        .collect();
    contest_types.sort();
    let governs = Governs {
        visibility,
        contest_types,
    };
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
            "UPDATE plugin SET governs = $2 WHERE id = $1 AND governs IS DISTINCT FROM $2",
            [plugin_id.into(), value.into()],
        ))
        .await;
    match result {
        Ok(_) => {
            if let Ok(mut map) = state.plugin_lifecycle.governs.write() {
                map.insert(plugin_id.to_string(), governs);
            }
        }
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
/// `fingerprint` is what the serving replica loaded from `source`; it
/// becomes the version every other replica checks its own copy against.
/// `None` (a disable) keeps the recorded one.
async fn publish(
    state: &AppState,
    plugin_id: &str,
    change: Change<'_>,
    fingerprint: Option<&Fingerprint>,
    source: &PluginSource,
) -> Result<Option<i64>, DbErr> {
    let version = fingerprint.map(|f| f.version.clone());
    let hash = fingerprint.map(|f| f.hash.clone());
    let archive = source.archive_hash().map(str::to_string);
    let statement = match change {
        Change::Enable => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO plugin (id, is_enabled, generation, updated_at, version, content_hash, archive_hash)
               VALUES ($1, TRUE, 1, NOW(), $2, $3, $4)
               ON CONFLICT (id) DO UPDATE
                 SET is_enabled = TRUE,
                     generation = plugin.generation + 1,
                     updated_at = NOW(),
                     version = COALESCE(EXCLUDED.version, plugin.version),
                     content_hash = COALESCE(EXCLUDED.content_hash, plugin.content_hash)
                 WHERE plugin.archive_hash IS NOT DISTINCT FROM EXCLUDED.archive_hash
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                archive.into(),
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
            r#"INSERT INTO plugin (id, is_enabled, generation, updated_at, version, content_hash, archive_hash)
               SELECT $1, TRUE, 1, NOW(), $2, $3, $4
                WHERE $5 OR EXISTS (SELECT 1 FROM plugin WHERE id = $1)
               ON CONFLICT (id) DO UPDATE
                 SET is_enabled = TRUE,
                     generation = plugin.generation + 1,
                     updated_at = NOW(),
                     version = COALESCE(EXCLUDED.version, plugin.version),
                     content_hash = COALESCE(EXCLUDED.content_hash, plugin.content_hash),
                     archive_hash = EXCLUDED.archive_hash
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
            ],
        ),
        Change::Reload => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE plugin
                  SET generation = generation + 1, updated_at = NOW(),
                      version = COALESCE($2, version),
                      content_hash = COALESCE($3, content_hash)
               WHERE id = $1 AND is_enabled AND archive_hash IS NOT DISTINCT FROM $4
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                archive.into(),
            ],
        ),
        Change::RemoveUpload(removed) => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE plugin
                  SET generation = generation + 1, updated_at = NOW(), archive_hash = NULL,
                      version = COALESCE($2, version),
                      content_hash = COALESCE($3, content_hash)
               WHERE id = $1 AND archive_hash = $4
               RETURNING generation"#,
            [
                plugin_id.into(),
                version.into(),
                hash.into(),
                removed.into(),
            ],
        ),
    };
    match state.db.query_one_raw(statement).await? {
        Some(row) => Ok(Some(row.try_get("", "generation")?)),
        None => Ok(None),
    }
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
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeError::Load(e) => e.fmt(f),
            ChangeError::Db(e) => e.fmt(f),
            ChangeError::NotEnabled(id) => write!(f, "Plugin '{id}' is not enabled"),
            ChangeError::Stale(id) => write!(f, "Plugin '{id}' changed on another server"),
            ChangeError::AlreadyExists(id) => write!(f, "Plugin '{id}' already exists"),
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
    record_governs(state, plugin_id).await;
    let published = match publish(state, plugin_id, change, fingerprint.as_ref(), source).await {
        Ok(Some(generation)) => Ok(generation),
        Ok(None) => Err(refused(state, plugin_id, change).await),
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

/// Enable a plugin fleet-wide.
///
/// Also the retry for a plugin that is enabled but failed on this replica.
pub async fn enable(state: &AppState, id: &PluginId) -> Result<(), AppError> {
    let plugin_id = id.as_str();
    with_applied(state, async |applied| {
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
    .await
}

/// Disable a plugin fleet-wide. Its pending timers are deleted: the plugin is
/// off everywhere, so they can never be delivered.
///
/// Keyed on the desired state, not on this replica's load state, so a plugin
/// that failed to load here can still be turned off.
pub async fn disable(state: &AppState, id: &PluginId) -> Result<(), AppError> {
    let plugin_id = id.as_str();
    with_applied(state, async |applied| {
        let source = published_source(state, id).await?;
        let Some(generation) = publish(state, plugin_id, Change::Disable, None, &source).await?
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
    .await
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
    with_applied(state, async |applied| {
        let source = published_source(state, id).await?;
        // Checked against the database so a peer's recent enable or disable
        // counts; `publish` re-checks atomically for a change in between.
        if !enabled_in_db(state, plugin_id).await? {
            return Err(ChangeError::NotEnabled(plugin_id.to_string()).into());
        }
        Ok(load_then_publish(state, applied, id, Change::Reload, &source).await?)
    })
    .await
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
    with_applied(state, async |applied| {
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
    .await
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
    with_applied(state, async |applied| {
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
                let generation = publish(
                    state,
                    plugin_id,
                    change,
                    fingerprint.as_ref(),
                    &PluginSource::Bundle,
                )
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
                state.plugin_sql.forget(plugin_id);
                if let Err(e) = crate::plugin_db::schema::retire(&state.db, plugin_id).await {
                    tracing::warn!(plugin_id, error = %e, "Could not retire the plugin's database role");
                }
                UploadRemoval::Removed
            }
        };
        refresh_cache(state).await;
        Ok(outcome)
    })
    .await
}

/// Reload every enabled plugin (including ones that failed to load here)
/// and pick up new plugin directories in the bundle, fleet-wide.
pub async fn reload_all(state: &AppState) -> Result<ReloadAllResponse, AppError> {
    with_applied(state, async |applied| {
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
        register_rows(state, &new_ids).await?;
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
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_doubles_and_is_capped() {
        assert_eq!(retry_delay(1), Duration::from_secs(5));
        assert_eq!(retry_delay(2), Duration::from_secs(10));
        assert_eq!(retry_delay(3), Duration::from_secs(20));
        assert_eq!(retry_delay(7), RETRY_MAX);
        assert_eq!(retry_delay(u32::MAX), RETRY_MAX);
    }
}
