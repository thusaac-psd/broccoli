//! Fleet-wide plugin lifecycle.
//!
//! Several identical server replicas share one database, so a plugin's
//! lifecycle cannot live in any one process. The desired state is the
//! `plugin` table: `is_enabled`, plus a `generation` that every lifecycle
//! change (enable, disable, reload, upload) bumps. Each replica converges its
//! local registry onto that table:
//!
//! - [`run`] polls the table every [`RECONCILE_INTERVAL`] and applies any row
//!   whose generation this replica has not applied yet. The table holds one
//!   row per installed plugin, so a poll is one tiny indexless scan.
//! - The admin endpoints call the `enable` / `disable` / `reload` /
//!   `reload_all` / `install` functions here, which apply the change on the
//!   serving replica first and only then publish it (bump the generation), so
//!   a change that cannot even load here never reaches the rest of the fleet.
//!   Every peer then picks it up on its next poll.
//!
//! A local failure stays local: an `init()` that fails is retried with
//! exponential backoff instead of leaving the plugin `Failed` until restart,
//! and nothing fleet-wide (timers, rows) is ever deleted because of it.
//! [`not_ready`] reports plugins that are desired but not loaded here, for
//! the health endpoints and the visibility kernel's fail-closed check.
//!
//! All of this is plugin-agnostic: the host only knows ids, the enabled flag,
//! and what each manifest declares.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use plugin_core::error::PluginError;
use plugin_core::registry::PluginStatus;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, EntityTrait, Set, Statement};

use crate::entity::plugin as plugin_entity;
use crate::error::AppError;
use crate::models::plugin::{ReloadAllResponse, ReloadFailure};
use crate::plugin_fleet::{self, ReplicaPluginState, ReplicaPluginStatus, ReplicaReport};
use crate::state::AppState;
use crate::utils::plugin::{
    self as plugin_utils, Fingerprint, LoadFailure, PluginActivationFailure, PreviousVersion,
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
    /// The last report written to this replica's `plugin_replica` row, and
    /// when, so an unchanged report is only re-touched every
    /// [`plugin_fleet::TOUCH_INTERVAL`].
    reported: std::sync::Mutex<Option<(ReplicaReport, Vec<String>, Instant)>>,
    /// Plugins still serving a previous version because the latest failed to
    /// load here, with that error (for readiness).
    outdated: RwLock<HashMap<String, String>>,
    /// The recorded [`Governs`] per plugin, as of the last reconcile.
    governs: RwLock<HashMap<String, Governs>>,
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
}

impl Desired {
    fn of(row: &plugin_entity::Model) -> Self {
        Self {
            enabled: row.is_enabled,
            generation: row.generation,
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

    /// What `plugin_id` governs, as recorded by the replica that last loaded
    /// it; `None` if no replica ever has.
    pub fn governs(&self, plugin_id: &str) -> Option<Governs> {
        self.governs.read().ok()?.get(plugin_id).cloned()
    }

    pub(crate) fn set_desired(&self, plugin_id: &str, enabled: bool) {
        if let Ok(mut d) = self.desired.write() {
            d.insert(plugin_id.to_string(), enabled);
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
    for id in state.plugin_lifecycle.desired_enabled_ids() {
        let outdated = state
            .plugin_lifecycle
            .outdated
            .read()
            .ok()
            .and_then(|o| o.get(&id).cloned());
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
    if let Ok(mut outdated) = state.plugin_lifecycle.outdated.write() {
        *outdated = applied
            .iter()
            .filter_map(|(id, a)| Some((id.clone(), a.kept_previous.clone()?)))
            .collect();
    }
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
                None if a.is_some_and(|a| a.enabled) => (ReplicaPluginStatus::NotInstalled, None),
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

/// Write the report if it changed, or touch it if the last write is getting
/// old. Best-effort: a failed write only makes the admin view stale.
async fn write_report(state: &AppState, applied: &HashMap<String, Applied>) {
    let now = Instant::now();
    let report = build_report(state, applied, now);
    let contest_types = crate::dispatcher::local_contest_types(state).await;
    let due = match state.plugin_lifecycle.reported.lock() {
        Ok(last) => !matches!(&*last, Some((r, types, at))
            if same_report(r, &report)
                && *types == contest_types
                && now.duration_since(*at) < plugin_fleet::TOUCH_INTERVAL),
        Err(_) => true,
    };
    if !due {
        return;
    }
    match plugin_fleet::report(&state.db, &state.config.server.id, &report, &contest_types).await {
        Ok(()) => {
            if let Ok(mut last) = state.plugin_lifecycle.reported.lock() {
                *last = Some((report, contest_types, now));
            }
        }
        Err(e) => tracing::warn!(error = %e, "Failed to write this replica's plugin report"),
    }
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

/// The per-replica reconcile loop. Runs for the process lifetime.
pub async fn run(state: AppState) {
    let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick fires immediately; boot has just reconciled.
    interval.tick().await;
    loop {
        interval.tick().await;
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
        if let Err(error) = apply(state, applied, &row.id, desired, now).await {
            failures.push(PluginActivationFailure {
                plugin_id: row.id.clone(),
                error,
            });
        }
    }
    Ok(failures)
}

/// Bring one plugin to `desired` on this replica, honouring the retry backoff
/// of a previously failed load.
async fn apply(
    state: &AppState,
    applied: &mut HashMap<String, Applied>,
    plugin_id: &str,
    desired: Desired,
    now: Instant,
) -> Result<(), PluginError> {
    let loaded = state.plugins.is_plugin_loaded(plugin_id).unwrap_or(false);

    if !desired.enabled {
        if loaded {
            deactivate(state, plugin_id).await?;
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
    let _ = discover(state, plugin_id);
    let local = plugin_utils::fingerprint(state, plugin_id).await;
    let differs = mismatch(desired.published.as_ref(), local.as_ref());
    let result = match &differs {
        Some(reason) if loaded => Err(LoadFailure {
            error: PluginError::LoadFailed(reason.clone()),
            kept_previous: true,
        }),
        _ => load_from_disk(state, plugin_id).await.map(drop),
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
            }
        }
    };
    applied.insert(plugin_id.to_string(), entry);
    result.map_err(|f| f.error)
}

/// (Re)load a plugin from this replica's disk, keeping a working loaded
/// version if the new one fails (see [`plugin_utils::reload_from_disk`]).
async fn load_from_disk(
    state: &AppState,
    plugin_id: &str,
) -> Result<Option<PreviousVersion>, LoadFailure> {
    match discover(state, plugin_id) {
        Ok(true) => {}
        Ok(false) => {
            return Err(LoadFailure {
                error: PluginError::NotFound(plugin_id.to_string()),
                kept_previous: false,
            });
        }
        Err(error) => {
            return Err(LoadFailure {
                error,
                kept_previous: false,
            });
        }
    }
    plugin_utils::reload_from_disk(state, plugin_id).await
}

async fn deactivate(state: &AppState, plugin_id: &str) -> Result<(), PluginError> {
    purge_plugin_registrations(&state.registries, plugin_id).await;
    state.plugins.unload_plugin(plugin_id)?;
    state.plugins.update_translations()
}

/// Record plugins found on this replica's disk that have no row yet, as
/// enabled (a newly installed plugin is on by default). Idempotent, so
/// replicas booting together cannot fail on the primary key.
async fn register_rows(state: &AppState, ids: &[String]) -> Result<(), DbErr> {
    if ids.is_empty() {
        return Ok(());
    }
    plugin_entity::Entity::insert_many(ids.iter().map(|id| plugin_entity::ActiveModel {
        id: Set(id.clone()),
        is_enabled: Set(true),
        generation: Set(0),
        updated_at: Set(chrono::Utc::now()),
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
enum Change {
    /// Enable (creating the row if needed).
    Enable,
    /// Disable.
    Disable,
    /// Re-apply an enabled plugin. Never enables: if another admin disabled
    /// the plugin meanwhile, the reload is refused instead of undoing that.
    Reload,
}

/// Record `change` for one plugin and bump its generation, so every replica
/// re-applies it. Returns the new generation, or `None` for a `Reload` of a
/// plugin that is not enabled.
///
/// `fingerprint` is what the serving replica loaded; it becomes the version
/// every other replica checks its own files against. `None` (a disable)
/// keeps the recorded one.
async fn publish(
    state: &AppState,
    plugin_id: &str,
    change: Change,
    fingerprint: Option<&Fingerprint>,
) -> Result<Option<i64>, DbErr> {
    let version = fingerprint.map(|f| f.version.clone());
    let hash = fingerprint.map(|f| f.hash.clone());
    let statement = match change {
        Change::Enable | Change::Disable => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO plugin (id, is_enabled, generation, updated_at, version, content_hash)
               VALUES ($1, $2, 1, NOW(), $3, $4)
               ON CONFLICT (id) DO UPDATE
                 SET is_enabled = EXCLUDED.is_enabled,
                     generation = plugin.generation + 1,
                     updated_at = NOW(),
                     version = COALESCE(EXCLUDED.version, plugin.version),
                     content_hash = COALESCE(EXCLUDED.content_hash, plugin.content_hash)
               RETURNING generation"#,
            [
                plugin_id.into(),
                (change == Change::Enable).into(),
                version.into(),
                hash.into(),
            ],
        ),
        Change::Reload => Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"UPDATE plugin
                  SET generation = generation + 1, updated_at = NOW(),
                      version = COALESCE($2, version),
                      content_hash = COALESCE($3, content_hash)
               WHERE id = $1 AND is_enabled
               RETURNING generation"#,
            [plugin_id.into(), version.into(), hash.into()],
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
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeError::Load(e) => e.fmt(f),
            ChangeError::Db(e) => e.fmt(f),
            ChangeError::NotEnabled(id) => write!(f, "Plugin '{id}' is not enabled"),
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
        }
    }
}

/// Load on this replica, then publish: the shared path behind enable, reload
/// and upload. A plugin that cannot load here is not pushed to the fleet;
/// the reconcile loop keeps retrying it locally against the old desired
/// state.
async fn load_then_publish(
    state: &AppState,
    applied: &mut HashMap<String, Applied>,
    plugin_id: &str,
    change: Change,
) -> Result<(), ChangeError> {
    let previous = match load_from_disk(state, plugin_id).await {
        Ok(previous) => previous,
        Err(failure) => {
            if let Some(a) = applied.get_mut(plugin_id) {
                a.retry = Some(Retry::after(a.retry, Instant::now()));
            }
            return Err(ChangeError::Load(failure.error));
        }
    };
    let fingerprint = plugin_utils::fingerprint(state, plugin_id).await;
    record_governs(state, plugin_id).await;
    let published = match publish(state, plugin_id, change, fingerprint.as_ref()).await {
        Ok(Some(generation)) => Ok(generation),
        Ok(None) => Err(ChangeError::NotEnabled(plugin_id.to_string())),
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
        },
    );
    state.plugin_lifecycle.set_desired(plugin_id, true);
    Ok(())
}

/// Whether `plugin_id` is in this replica's registry, looking for newly added
/// plugin directories first if it is not.
fn discover(state: &AppState, plugin_id: &str) -> Result<bool, PluginError> {
    if state.plugins.has_plugin(plugin_id)? {
        return Ok(true);
    }
    state.plugins.rediscover_plugins()?;
    state.plugins.has_plugin(plugin_id)
}

fn require_known(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    if discover(state, plugin_id)? {
        Ok(())
    } else {
        Err(AppError::NotFound(format!(
            "Plugin '{plugin_id}' not found"
        )))
    }
}

/// Enable a plugin fleet-wide.
///
/// Also the retry for a plugin that is enabled but failed on this replica.
pub async fn enable(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    with_applied(state, async |applied| {
        require_known(state, plugin_id)?;
        if state.plugins.is_plugin_loaded(plugin_id)? {
            return Err(AppError::Conflict(format!(
                "Plugin '{plugin_id}' is already enabled"
            )));
        }
        Ok(load_then_publish(state, applied, plugin_id, Change::Enable).await?)
    })
    .await
}

/// Disable a plugin fleet-wide. Its pending timers are deleted: the plugin is
/// off everywhere, so they can never be delivered.
///
/// Keyed on the desired state, not on this replica's load state, so a plugin
/// that failed to load here can still be turned off.
pub async fn disable(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    with_applied(state, async |applied| {
        if !state.plugin_lifecycle.is_desired_enabled(plugin_id) {
            return Err(AppError::Conflict(format!(
                "Plugin '{plugin_id}' is not currently enabled"
            )));
        }
        let generation = publish(state, plugin_id, Change::Disable, None)
            .await?
            .ok_or_else(|| AppError::Internal("plugin upsert returned no row".into()))?;
        state.plugin_lifecycle.set_desired(plugin_id, false);
        let desired = Desired {
            enabled: false,
            generation,
            published: None,
        };
        apply(state, applied, plugin_id, desired, Instant::now()).await?;
        if let Err(e) =
            crate::dispatcher::plugin_timer::delete_timers_for_plugin(&state.db, plugin_id).await
        {
            tracing::error!(
                plugin_id,
                error = %e,
                "Failed to delete pending timers for a disabled plugin"
            );
        }
        Ok(())
    })
    .await
}

/// Reload an enabled plugin from disk, fleet-wide. This is also the retry
/// for a plugin that failed to load: it is accepted whatever this replica's
/// own load state, and a successful load here bumps the generation, so every
/// failing replica retries at once instead of waiting out its backoff.
pub async fn reload(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    with_applied(state, async |applied| {
        require_known(state, plugin_id)?;
        if !state.plugin_lifecycle.is_desired_enabled(plugin_id) {
            return Err(AppError::Conflict(format!(
                "Plugin '{plugin_id}' is not enabled. Use enable instead."
            )));
        }
        Ok(load_then_publish(state, applied, plugin_id, Change::Reload).await?)
    })
    .await
}

/// (Re)load a plugin whose files were just written to this replica's disk
/// (admin upload), and enable it fleet-wide.
pub async fn install(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    with_applied(state, async |applied| {
        Ok(load_then_publish(state, applied, plugin_id, Change::Enable).await?)
    })
    .await
}

/// Reload every enabled plugin (including ones that failed to load here)
/// and pick up new plugin directories, fleet-wide.
pub async fn reload_all(state: &AppState) -> Result<ReloadAllResponse, AppError> {
    with_applied(state, async |applied| {
        let enabled_ids: Vec<String> = state
            .plugins
            .list_plugins()?
            .into_iter()
            .map(|p| p.id)
            .filter(|id| state.plugin_lifecycle.is_desired_enabled(id))
            .collect();

        let mut reloaded = Vec::new();
        let mut failed = Vec::new();
        for id in enabled_ids {
            match load_then_publish(state, applied, &id, Change::Reload).await {
                Ok(()) => reloaded.push(id),
                Err(e) => {
                    tracing::error!(plugin_id = %id, error = %e, "Failed to reload plugin");
                    failed.push(ReloadFailure {
                        id,
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
            let desired = Desired::of(&row);
            if let Err(e) = apply(state, applied, &row.id, desired, now).await {
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
