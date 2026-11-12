//! Fleet-wide view of plugin load state.
//!
//! Every replica keeps one `plugin_replica` row: a JSON map of its per-plugin
//! [`ReplicaPluginState`], written by [`crate::plugin_lifecycle`] whenever it
//! changes and touched periodically otherwise. Both timestamps are written
//! with the database clock, and liveness is decided in SQL against the same
//! clock, so no two servers' clocks are ever compared. A replica that dies
//! simply stops touching its row and drops out of [`read`]; a replica that
//! shuts down cleanly deletes it ([`retire`]).
//!
//! Admin reads aggregate the rows with the desired state (`plugin` table)
//! into a [`PluginFleetResponse`] per plugin: two small queries per request,
//! no fan-out to the replicas themselves.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, Statement};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::entity::plugin as plugin_entity;
use crate::entity::plugin_replica;
use crate::plugin_distribution::{PluginSource, PluginSourceKind};

/// A replica's row counts as live while it was touched within this window.
/// Replicas touch at least every [`TOUCH_INTERVAL`], so a live one is never
/// missed and a dead one disappears within a minute.
pub const LIVE_WINDOW_SECS: i64 = 60;

/// How often a replica re-touches an unchanged row.
pub const TOUCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// One plugin's state on one replica.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReplicaPluginStatus {
    /// Loaded and serving.
    Loaded,
    /// Failed to load or initialize; the replica retries with backoff.
    Failed,
    /// Enabled, but the plugin's files are not on this replica's disk.
    NotInstalled,
    /// The latest version failed to load here; the previously loaded version
    /// is still serving, and the replica keeps retrying the new one.
    Outdated,
    /// Loaded, but this replica's files are not the version the fleet
    /// published (they were loaded because nothing else was running here).
    Mismatched,
    /// Not loaded (disabled, or not applied yet).
    Unloaded,
}

impl ReplicaPluginStatus {
    /// Whether this state means the replica is not running the plugin's
    /// latest version and will not converge without a retry.
    pub fn is_failure(self) -> bool {
        matches!(
            self,
            ReplicaPluginStatus::Failed
                | ReplicaPluginStatus::NotInstalled
                | ReplicaPluginStatus::Outdated
                | ReplicaPluginStatus::Mismatched
        )
    }
}

/// What one replica reports for one plugin (stored as JSON).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplicaPluginState {
    pub status: ReplicaPluginStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The desired-state generation this replica last applied.
    pub generation: i64,
    /// Time until the next load attempt, relative to the row's `state_at`
    /// (the report is rewritten on every attempt, so this stays accurate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_in_ms: Option<u64>,
    /// Manifest version of what is loaded here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

pub type ReplicaReport = BTreeMap<String, ReplicaPluginState>;

/// Upsert this replica's report and its heartbeat. `state_at` only moves
/// when the report actually changed, so a periodic touch does not shift the
/// retry times derived from it.
///
/// `contest_types` are the contest types this replica can judge (see
/// `crate::dispatcher::prefer_local_handler`).
pub async fn report(
    db: &DatabaseConnection,
    server_id: &str,
    report: &ReplicaReport,
    contest_types: &[String],
) -> Result<(), DbErr> {
    let plugins = serde_json::to_value(report).map_err(|e| DbErr::Custom(e.to_string()))?;
    let contest_types =
        serde_json::to_value(contest_types).map_err(|e| DbErr::Custom(e.to_string()))?;
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"INSERT INTO plugin_replica (server_id, plugins, contest_types, state_at, heartbeat_at)
           VALUES ($1, $2, $3, NOW(), NOW())
           ON CONFLICT (server_id) DO UPDATE
             SET state_at = CASE WHEN plugin_replica.plugins = EXCLUDED.plugins
                                 THEN plugin_replica.state_at ELSE NOW() END,
                 plugins = EXCLUDED.plugins,
                 contest_types = EXCLUDED.contest_types,
                 heartbeat_at = NOW()"#,
        [server_id.into(), plugins.into(), contest_types.into()],
    ))
    .await?;
    Ok(())
}

/// Remove this replica from the fleet view (graceful shutdown).
pub async fn retire(db: &DatabaseConnection, server_id: &str) -> Result<(), DbErr> {
    plugin_replica::Entity::delete_by_id(server_id.to_string())
        .exec(db)
        .await?;
    Ok(())
}

/// Desired state plus every live replica's report.
pub struct FleetView {
    desired: HashMap<String, DesiredRow>,
    replicas: Vec<LiveReplica>,
}

struct DesiredRow {
    enabled: bool,
    generation: i64,
    source: PluginSource,
}

struct LiveReplica {
    server_id: String,
    state_at: DateTime<Utc>,
    plugins: ReplicaReport,
}

pub async fn read(db: &DatabaseConnection) -> Result<FleetView, DbErr> {
    let desired = plugin_entity::Entity::find()
        .all(db)
        .await?
        .into_iter()
        .map(|p| {
            let row = DesiredRow {
                enabled: p.is_enabled,
                generation: p.generation,
                source: PluginSource::of(p.archive_hash.as_deref()),
            };
            (p.id, row)
        })
        .collect();
    let rows = plugin_replica::Entity::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT server_id, plugins, contest_types, state_at, heartbeat_at FROM plugin_replica
               WHERE heartbeat_at > NOW() - make_interval(secs => $1)
               ORDER BY server_id"#,
            [(LIVE_WINDOW_SECS as f64).into()],
        ))
        .all(db)
        .await?;
    let replicas = rows
        .into_iter()
        .map(|row| LiveReplica {
            // A row this version cannot parse (written by a newer one) still
            // counts as a live replica, reporting nothing.
            plugins: serde_json::from_value(row.plugins).unwrap_or_default(),
            server_id: row.server_id,
            state_at: row.state_at,
        })
        .collect();
    Ok(FleetView { desired, replicas })
}

/// The source the fleet runs a plugin from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PluginSourceResponse {
    pub kind: PluginSourceKind,
    /// Content hash of the uploaded archive, when `kind` is `upload`.
    #[schema(example = "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08")]
    pub archive_hash: Option<String>,
    /// Whether the serving replica's bundle has a copy of the plugin: what
    /// removing the upload falls back to.
    pub bundled: bool,
}

/// Fleet-wide state of one plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FleetPluginState {
    /// Enabled and loaded at the current generation on every live replica.
    Loaded,
    /// Enabled, and failing, not installed or outdated on at least one replica.
    Failed,
    /// Converging: some replica has not applied the latest change yet.
    Pending,
    /// Disabled, and unloaded everywhere.
    Disabled,
}

/// One live replica's view of a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PluginReplicaResponse {
    #[schema(example = "server-1")]
    pub server_id: String,
    pub status: ReplicaPluginStatus,
    /// Load or `init()` error, when `status` is `failed`.
    pub error: Option<String>,
    /// Whether this replica has applied the latest lifecycle change.
    pub up_to_date: bool,
    /// When this replica next retries a failed load.
    pub retry_at: Option<DateTime<Utc>>,
    /// Manifest version of what is loaded on this replica.
    #[schema(example = "1.2.0")]
    pub version: Option<String>,
}

/// A plugin's state across all live server replicas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PluginFleetResponse {
    pub state: FleetPluginState,
    /// Live replicas.
    pub replicas_total: u32,
    /// Live replicas with the plugin loaded at the latest generation.
    pub replicas_loaded: u32,
    /// Live replicas where the plugin failed, is not installed, is still on a
    /// previous version because the latest one failed to load, or runs files
    /// that differ from the published version.
    pub replicas_failed: u32,
    pub replicas: Vec<PluginReplicaResponse>,
}

impl FleetView {
    /// Every plugin the fleet knows about (has a desired-state row for).
    pub fn plugin_ids(&self) -> impl Iterator<Item = &str> {
        self.desired.keys().map(String::as_str)
    }

    /// Whether `plugin_id` is enabled in the desired state.
    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.desired.get(plugin_id).is_some_and(|row| row.enabled)
    }

    /// Where the fleet takes `plugin_id`'s files from. `bundled` is whether
    /// the serving replica's bundle has a copy.
    pub fn source(&self, plugin_id: &str, bundled: bool) -> PluginSourceResponse {
        let source = self
            .desired
            .get(plugin_id)
            .map_or(PluginSource::Bundle, |row| row.source.clone());
        PluginSourceResponse {
            kind: source.kind(),
            archive_hash: source.archive_hash().map(str::to_string),
            bundled,
        }
    }

    pub fn plugin(&self, plugin_id: &str) -> PluginFleetResponse {
        let (enabled, generation) = self
            .desired
            .get(plugin_id)
            .map_or((false, 0), |row| (row.enabled, row.generation));
        let replicas: Vec<PluginReplicaResponse> = self
            .replicas
            .iter()
            .map(|r| {
                let reported = r.plugins.get(plugin_id);
                PluginReplicaResponse {
                    server_id: r.server_id.clone(),
                    status: reported.map_or(ReplicaPluginStatus::Unloaded, |s| s.status),
                    error: reported.and_then(|s| s.error.clone()),
                    up_to_date: reported.is_some_and(|s| s.generation == generation),
                    version: reported.and_then(|s| s.version.clone()),
                    retry_at: reported.and_then(|s| s.retry_in_ms).map(|ms| {
                        r.state_at + chrono::Duration::milliseconds(ms.min(i64::MAX as u64) as i64)
                    }),
                }
            })
            .collect();
        aggregate(enabled, replicas)
    }
}

fn aggregate(enabled: bool, replicas: Vec<PluginReplicaResponse>) -> PluginFleetResponse {
    let count = |f: &dyn Fn(&PluginReplicaResponse) -> bool| {
        replicas.iter().filter(|r| f(r)).count() as u32
    };
    let total = replicas.len() as u32;
    let loaded = count(&|r| r.status == ReplicaPluginStatus::Loaded && r.up_to_date);
    let failed = count(&|r| r.status.is_failure());
    let any_loaded = count(&|r| r.status == ReplicaPluginStatus::Loaded) > 0;
    let state = match (enabled, failed > 0) {
        (true, true) => FleetPluginState::Failed,
        (true, false) if total > 0 && loaded == total => FleetPluginState::Loaded,
        (true, false) => FleetPluginState::Pending,
        (false, _) if any_loaded => FleetPluginState::Pending,
        (false, _) => FleetPluginState::Disabled,
    };
    PluginFleetResponse {
        state,
        replicas_total: total,
        replicas_loaded: loaded,
        replicas_failed: failed,
        replicas,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replica(status: ReplicaPluginStatus, up_to_date: bool) -> PluginReplicaResponse {
        PluginReplicaResponse {
            server_id: "s".into(),
            status,
            error: None,
            up_to_date,
            retry_at: None,
            version: None,
        }
    }

    #[test]
    fn aggregate_names_the_fleet_state() {
        use ReplicaPluginStatus::*;
        let state = |enabled, replicas| aggregate(enabled, replicas).state;
        assert_eq!(
            state(true, vec![replica(Loaded, true), replica(Loaded, true)]),
            FleetPluginState::Loaded
        );
        let mixed = aggregate(true, vec![replica(Loaded, true), replica(Failed, true)]);
        assert_eq!(mixed.state, FleetPluginState::Failed);
        assert_eq!((mixed.replicas_failed, mixed.replicas_total), (1, 2));
        assert_eq!(
            state(
                true,
                vec![replica(Loaded, true), replica(NotInstalled, true)]
            ),
            FleetPluginState::Failed
        );
        // A replica still on the previous generation, or not reporting yet.
        assert_eq!(
            state(true, vec![replica(Loaded, true), replica(Loaded, false)]),
            FleetPluginState::Pending
        );
        assert_eq!(state(true, vec![]), FleetPluginState::Pending);
        assert_eq!(
            state(false, vec![replica(Unloaded, true)]),
            FleetPluginState::Disabled
        );
        assert_eq!(
            state(false, vec![replica(Loaded, false)]),
            FleetPluginState::Pending
        );
    }
}
