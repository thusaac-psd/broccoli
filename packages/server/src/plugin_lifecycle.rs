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
use crate::state::AppState;
use crate::utils::plugin::{PluginActivationFailure, activate_plugin, purge_plugin_registrations};

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
}

#[derive(Debug, Clone, Copy)]
struct Applied {
    generation: i64,
    enabled: bool,
    /// Set while the last load of this generation failed.
    retry: Option<Retry>,
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
        let reason = match registry.get(&id).map(|e| &e.status) {
            Some(PluginStatus::Loaded) => continue,
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
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    Ok(reconcile_locked(state, &mut applied).await?)
}

/// Converge this replica onto the desired state once.
pub async fn reconcile(state: &AppState) -> Result<Vec<PluginActivationFailure>, DbErr> {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    reconcile_locked(state, &mut applied).await
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
    let retry = result.as_ref().err().map(|_| Retry::after(retry, now));
    if let (Some(r), Err(e)) = (retry, &result) {
        tracing::warn!(
            plugin_id,
            attempts = r.attempts,
            retry_in_secs = retry_delay(r.attempts).as_secs(),
            error = %e,
            "Plugin failed to load on this replica; will retry"
        );
    }
    applied.insert(
        plugin_id.to_string(),
        Applied {
            generation: desired.generation,
            enabled: true,
            retry,
        },
    );
    result
}

/// (Re)load a plugin from this replica's disk: re-read its manifest, rebuild
/// its runtime, register its hooks and run `init()`. On any failure the
/// plugin is left unloaded with status `Failed`, never half-registered.
async fn load_from_disk(state: &AppState, plugin_id: &str) -> Result<(), PluginError> {
    if !discover(state, plugin_id)? {
        return Err(PluginError::NotFound(plugin_id.to_string()));
    }
    purge_plugin_registrations(&state.registries, plugin_id).await;
    if let Err(error) = state.plugins.reload_plugin(plugin_id) {
        crate::utils::plugin::fail_plugin(state, plugin_id, &error).await;
        return Err(error);
    }
    activate_plugin(state, plugin_id).await
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
    if let Err(error) = load_from_disk(state, plugin_id).await {
        if let Some(a) = applied.get_mut(plugin_id) {
            a.retry = Some(Retry::after(a.retry, Instant::now()));
        }
        return Err(ChangeError::Load(error));
    }
    let generation = publish(state, plugin_id, true)
        .await
        .map_err(ChangeError::Db)?;
    applied.insert(
        plugin_id.to_string(),
        Applied {
            generation,
            enabled: true,
            retry: None,
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
pub async fn enable(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    require_known(state, plugin_id)?;
    if state.plugins.is_plugin_loaded(plugin_id)? {
        return Err(AppError::Conflict(format!(
            "Plugin '{plugin_id}' is already enabled"
        )));
    }
    Ok(load_then_publish(state, &mut applied, plugin_id).await?)
}

/// Disable a plugin fleet-wide. Its pending timers are deleted: the plugin is
/// off everywhere, so they can never be delivered.
pub async fn disable(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    if !state.plugins.is_plugin_loaded(plugin_id)? {
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
    apply(state, &mut applied, plugin_id, desired, Instant::now()).await?;
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
}

/// Reload a loaded plugin from disk, fleet-wide.
pub async fn reload(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    if !state.plugins.has_plugin(plugin_id)? {
        return Err(AppError::NotFound(format!(
            "Plugin '{plugin_id}' not found"
        )));
    }
    if !state.plugins.is_plugin_loaded(plugin_id)? {
        return Err(AppError::Conflict(format!(
            "Plugin '{plugin_id}' is not currently loaded. Use enable instead."
        )));
    }
    Ok(load_then_publish(state, &mut applied, plugin_id).await?)
}

/// (Re)load a plugin whose files were just written to this replica's disk
/// (admin upload), and enable it fleet-wide.
pub async fn install(state: &AppState, plugin_id: &str) -> Result<(), AppError> {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    Ok(load_then_publish(state, &mut applied, plugin_id).await?)
}

/// Reload every loaded plugin and pick up new plugin directories, fleet-wide.
pub async fn reload_all(state: &AppState) -> Result<ReloadAllResponse, AppError> {
    let mut applied = state.plugin_lifecycle.applied.lock().await;
    let loaded_ids: Vec<String> = state
        .plugins
        .list_plugins()?
        .into_iter()
        .filter(|p| p.status == PluginStatus::Loaded)
        .map(|p| p.id)
        .collect();

    let mut reloaded = Vec::new();
    let mut failed = Vec::new();
    for id in loaded_ids {
        match load_then_publish(state, &mut applied, &id).await {
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
        if let Err(e) = apply(state, &mut applied, &row.id, desired, now).await {
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
