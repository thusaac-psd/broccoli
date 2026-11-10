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
use serde::Serialize;
use utoipa::ToSchema;

use crate::entity::plugin as plugin_entity;
use crate::error::AppError;
use crate::models::plugin::{ReloadAllResponse, ReloadFailure};
use crate::plugin_fleet::{self, ReplicaPluginState, ReplicaPluginStatus, ReplicaReport};
use crate::state::AppState;
use crate::utils::plugin::{
    self as plugin_utils, LoadFailure, PluginActivationFailure, PreviousVersion,
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
    reported: std::sync::Mutex<Option<(ReplicaReport, Instant)>>,
    /// Plugins still serving a previous version because the latest failed to
    /// load here, with that error (for readiness).
    outdated: RwLock<HashMap<String, String>>,
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
}

#[derive(Debug, Clone, Copy)]
struct Retry {
    attempts: u32,
    at: Instant,
}

#[derive(Debug, Clone, Copy)]
struct Desired {
    enabled: bool,
    generation: i64,
}

/// A plugin that is enabled fleet-wide but not loaded on this replica.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
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

    fn desired_enabled_ids(&self) -> Vec<String> {
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
    let due = match state.plugin_lifecycle.reported.lock() {
        Ok(last) => !matches!(&*last, Some((r, at))
            if same_report(r, &report) && now.duration_since(*at) < plugin_fleet::TOUCH_INTERVAL),
        Err(_) => true,
    };
    if !due {
        return;
    }
    match plugin_fleet::report(&state.db, &state.config.server.id, &report).await {
        Ok(()) => {
            if let Ok(mut last) = state.plugin_lifecycle.reported.lock() {
                *last = Some((report, now));
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
        let desired = Desired {
            enabled: row.is_enabled,
            generation: row.generation,
        };
        if let Err(error) = apply(state, applied, &row.id, desired, now).await {
            failures.push(PluginActivationFailure {
                plugin_id: row.id.clone(),
                error,
            });
        }
        state.plugin_lifecycle.set_desired(&row.id, row.is_enabled);
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

    let result = load_from_disk(state, plugin_id).await;
    let (retry, kept_previous) = match &result {
        Ok(_) => (None, None),
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
            (
                Some(r),
                failure.kept_previous.then(|| failure.error.to_string()),
            )
        }
    };
    applied.insert(
        plugin_id.to_string(),
        Applied {
            generation: desired.generation,
            enabled: true,
            retry,
            kept_previous,
        },
    );
    result.map(drop).map_err(|f| f.error)
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

/// Write the desired state for one plugin and bump its generation, so every
/// replica re-applies it. Returns the new generation.
async fn publish(state: &AppState, plugin_id: &str, enabled: bool) -> Result<i64, DbErr> {
    let row = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO plugin (id, is_enabled, generation, updated_at)
               VALUES ($1, $2, 1, NOW())
               ON CONFLICT (id) DO UPDATE
                 SET is_enabled = EXCLUDED.is_enabled,
                     generation = plugin.generation + 1,
                     updated_at = NOW()
               RETURNING generation"#,
            [plugin_id.into(), enabled.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("plugin upsert returned no row".into()))?;
    row.try_get("", "generation")
}

/// Why a lifecycle change did not go through.
#[derive(Debug)]
enum ChangeError {
    /// The plugin could not be loaded on this replica, so nothing was
    /// published.
    Load(PluginError),
    /// Loaded here, but writing the desired state failed; the next reconcile
    /// returns this replica to the old desired state.
    Db(DbErr),
}

impl std::fmt::Display for ChangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeError::Load(e) => e.fmt(f),
            ChangeError::Db(e) => e.fmt(f),
        }
    }
}

impl From<ChangeError> for AppError {
    fn from(e: ChangeError) -> Self {
        match e {
            ChangeError::Load(e) => e.into(),
            ChangeError::Db(e) => e.into(),
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
    let generation = match publish(state, plugin_id, true).await {
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
            return Err(ChangeError::Db(e));
        }
    };
    applied.insert(
        plugin_id.to_string(),
        Applied {
            generation,
            enabled: true,
            retry: None,
            kept_previous: None,
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
        Ok(load_then_publish(state, applied, plugin_id).await?)
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
        let generation = publish(state, plugin_id, false).await?;
        state.plugin_lifecycle.set_desired(plugin_id, false);
        let desired = Desired {
            enabled: false,
            generation,
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
        Ok(load_then_publish(state, applied, plugin_id).await?)
    })
    .await
}

/// (Re)load a plugin whose files were just written to this replica's disk
/// (admin upload), and enable it fleet-wide.
pub async fn install(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    with_applied(state, async |applied| {
        Ok(load_then_publish(state, applied, plugin_id).await?)
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
            match load_then_publish(state, applied, &id).await {
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
            let desired = Desired {
                enabled: row.is_enabled,
                generation: row.generation,
            };
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
