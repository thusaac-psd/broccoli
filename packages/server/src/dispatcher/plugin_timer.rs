//! Delivers due plugin timers (see
//! `docs/superpowers/specs/2026-09-19-plugin-timer-design.md`).
//!
//! Spawned **unconditionally** by `Dispatcher::spawn` -- independent of
//! `dispatcher_lease_steal_enabled`, which governs submission judging, not
//! plugin timers. A deployment that disables lease/steal and the claim fiber
//! still wants a plugin's `[[server.timers]]` callback to fire.
//!
//! Each tick claims due rows in ONE statement ([`claim_due`]) and then
//! invokes each claimed row's plugin OUTSIDE any transaction. Holding a
//! transaction across a plugin invocation and then acquiring a second pooled
//! connection inside it is the self-deadlock pattern this codebase has
//! already been bitten by twice (`create_submission`, then `run_code` on
//! this branch) -- see the `plugin-db-no-transactions-under-concurrency` and
//! `submit-pool-deadlock` postmortems.
//!
//! Delivery is **at-least-once with bounded retries, then drop**. This
//! cannot fail closed: a timer's only safe outcomes are "eventually fires"
//! or "dropped loudly after `max_attempts`" -- "never fires" is precisely
//! the failure this capability exists to prevent, so a repeatedly-trapping
//! plugin's timer is dropped with an `error!` log rather than retried
//! forever, and the loop keeps delivering every other plugin's timers in the
//! meantime.

use std::time::Duration as StdDuration;

use chrono::Duration;
use plugin_core::registry::PluginStatus;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait, FromQueryResult, QueryFilter,
    Statement,
};
use serde::Serialize;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::entity::plugin_timer;
use crate::state::AppState;

/// Config for the plugin-timer delivery loop. Defaults mirror the design
/// spec exactly: 1s tick, 30s claim lease, 64-row batch, 5 delivery
/// attempts before a timer is dropped.
#[derive(Debug, Clone, Copy)]
pub struct TimerConfig {
    pub tick_interval_secs: u64,
    pub lease_secs: i64,
    pub batch: u64,
    pub max_attempts: i32,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 1,
            lease_secs: 30,
            batch: 64,
            max_attempts: 5,
        }
    }
}

/// Outcome of one [`tick_once`] call, exposed so integration tests can race
/// two concurrent ticks and assert their claimed counts sum to exactly one
/// row (see `tests/integration/plugin_timer.rs`,
/// `two_concurrent_claimers_deliver_a_timer_once`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickStats {
    pub claimed: usize,
}

/// Long-running loop. Polls every `config.tick_interval_secs` until the
/// cancel channel fires. Mirrors the shape of `dispatcher::claim::run` /
/// `dispatcher::operation_reaper::run`.
pub async fn run(state: AppState, config: TimerConfig, mut cancel: watch::Receiver<bool>) {
    let mut interval =
        tokio::time::interval(StdDuration::from_secs(config.tick_interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    info!(
        tick_interval_secs = config.tick_interval_secs,
        lease_secs = config.lease_secs,
        batch = config.batch,
        max_attempts = config.max_attempts,
        "Plugin timer delivery loop started"
    );

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(e) = tick_once(&state, &config).await {
                    error!(error = %e, "Plugin timer tick failed");
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    info!("Plugin timer delivery loop shutting down");
                    return;
                }
            }
        }
    }
}

/// One claim-and-deliver cycle. `pub` (not `pub(crate)`) so integration
/// tests can drive it directly through `server::dispatcher::plugin_timer`,
/// the same way `tests/integration/scaling.rs` calls
/// `server::dispatcher::sweeper::sweep_once` directly.
pub async fn tick_once(state: &AppState, config: &TimerConfig) -> Result<TickStats, DbErr> {
    let claimed = claim_due(
        &state.db,
        Duration::seconds(config.lease_secs),
        config.batch,
    )
    .await?;
    let stats = TickStats {
        claimed: claimed.len(),
    };
    for row in claimed {
        deliver(state, config, row).await;
    }
    Ok(stats)
}

/// Claims due timers in ONE statement.
///
/// Two independent mechanisms stop two replicas (or two concurrent ticks)
/// from both claiming the same row, and they are deliberately redundant:
///
/// - the `claimed_at` predicate excludes a row another claimer already took,
///   within the lease window;
/// - `FOR UPDATE SKIP LOCKED` makes a claimer skip a row another claimer
///   currently holds, rather than blocking on its lock.
///
/// Either alone is sufficient for correctness — verified by mutation, not by
/// argument: removing just one keeps
/// `two_concurrent_claimers_take_a_timer_exactly_once` green, and removing
/// both makes it fail with a genuine duplicate claim. `SKIP LOCKED`'s
/// distinct contribution is liveness (no claimer waits on another's lock),
/// which the suite does not measure.
///
/// Separately load-bearing: no transaction is held across the plugin
/// invocation that follows. `claim_due` returns before `deliver` ever touches
/// the plugin host, so this statement's connection is back in the pool first.
/// Holding one and then acquiring a second pooled connection inside it is the
/// self-deadlock shape this codebase has already been bitten by twice.
async fn claim_due(
    db: &DatabaseConnection,
    lease: Duration,
    batch: u64,
) -> Result<Vec<plugin_timer::Model>, DbErr> {
    let sql = r#"
        UPDATE plugin_timer
        SET claimed_at = now(), attempts = attempts + 1
        WHERE id IN (
          SELECT id FROM plugin_timer
          WHERE fire_at <= now()
            AND (claimed_at IS NULL OR claimed_at < now() - ($1 || ' seconds')::interval)
          ORDER BY fire_at
          LIMIT $2
          FOR UPDATE SKIP LOCKED
        )
        RETURNING id, plugin_id, key, fire_at, payload, claimed_at, attempts, created_at
    "#;
    plugin_timer::Model::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        [lease.num_seconds().into(), (batch as i64).into()],
    ))
    .all(db)
    .await
}

/// Deletes every pending timer belonging to `plugin_id`. Called when a
/// plugin is unloaded -- on a failed `init()` during activation
/// (`utils::plugin::activate_plugin`) and on an admin-initiated disable
/// (`handlers::admin::disable_plugin`). A plugin's timers are meaningless
/// once it can no longer be invoked: without this, `deliver`'s
/// `timer_function` lookup would keep returning `Ok(None)` for them forever,
/// which already drops rows one at a time on the next tick that claims them
/// -- this just does it immediately, at the moment of unload, rather than
/// leaving them to expire on a best-effort basis. NOT called from
/// `reload_plugin`: a reload swaps the WASM runtime under the same plugin
/// id, and its pending timers remain valid callbacks for the reloaded code.
pub async fn delete_timers_for_plugin(
    db: &DatabaseConnection,
    plugin_id: &str,
) -> Result<(), DbErr> {
    plugin_timer::Entity::delete_many()
        .filter(plugin_timer::Column::PluginId.eq(plugin_id))
        .exec(db)
        .await?;
    Ok(())
}

/// Deletes a `plugin_timer` row after successful delivery, a permanent drop
/// past `max_attempts`, or discovering the owning plugin can never receive
/// it. Errors are logged, not propagated: the row is left claimed either
/// way, so the worst case is a redelivery after the lease expires, not a
/// silent loss.
async fn delete_timer(db: &DatabaseConnection, id: i64) {
    if let Err(e) = plugin_timer::Entity::delete_by_id(id).exec(db).await {
        error!(id, error = %e, "Failed to delete plugin_timer row after delivery");
    }
}

/// Looks up the `[[server.timers]] function` a `Loaded` plugin declared.
/// Returns `Ok(None)` for an unknown plugin, one that isn't `Loaded`, or one
/// with no declaration -- the caller treats all three identically: this row
/// cannot be delivered right now and should be dropped with a warning, since
/// a plugin can only reach this state by dropping the declaration (or being
/// disabled) while timers were still pending.
fn timer_function(state: &AppState, plugin_id: &str) -> Result<Option<String>, ()> {
    let registry = state.plugins.get_registry().read().map_err(|_| ())?;
    let Some(entry) = registry.get(plugin_id) else {
        return Ok(None);
    };
    if entry.status != PluginStatus::Loaded {
        return Ok(None);
    }
    let Some(server) = &entry.manifest.server else {
        return Ok(None);
    };
    Ok(server.timers.first().map(|t| t.function.clone()))
}

#[derive(Serialize)]
struct TimerCallbackInput<'a> {
    key: &'a str,
    payload: &'a str,
    fire_at_ms: i64,
    attempt: i32,
}

/// Delivers one claimed row: invokes the plugin's declared timer function,
/// then deletes the row on success, drops it (with a loud error) once
/// `max_attempts` is exhausted, or otherwise leaves it claimed for
/// lease-expiry redelivery.
async fn deliver(state: &AppState, config: &TimerConfig, row: plugin_timer::Model) {
    let function = match timer_function(state, &row.plugin_id) {
        Ok(Some(f)) => f,
        Ok(None) => {
            warn!(
                plugin_id = %row.plugin_id,
                key = %row.key,
                "Dropping plugin timer: plugin has no [[server.timers]] handler, or is not \
                 currently loaded. Reachable if a plugin drops the declaration (or is \
                 disabled/uninstalled) while timers are pending."
            );
            delete_timer(&state.db, row.id).await;
            return;
        }
        Err(()) => {
            error!(
                plugin_id = %row.plugin_id,
                key = %row.key,
                "Plugin registry lock poisoned while resolving the timer handler; leaving row \
                 claimed for lease-expiry redelivery"
            );
            return;
        }
    };

    let input = TimerCallbackInput {
        key: &row.key,
        payload: &row.payload,
        fire_at_ms: row.fire_at.timestamp_millis(),
        attempt: row.attempts,
    };
    let input_bytes = match serde_json::to_vec(&input) {
        Ok(b) => b,
        Err(e) => {
            error!(
                plugin_id = %row.plugin_id,
                key = %row.key,
                error = %e,
                "Failed to serialize plugin timer callback input; leaving row claimed for \
                 lease-expiry redelivery"
            );
            return;
        }
    };

    match state
        .plugins
        .call_raw(&row.plugin_id, &function, input_bytes)
        .await
    {
        Ok(_) => {
            delete_timer(&state.db, row.id).await;
        }
        Err(e) => {
            if row.attempts >= config.max_attempts {
                error!(
                    plugin_id = %row.plugin_id,
                    key = %row.key,
                    attempts = row.attempts,
                    error = %e,
                    "Dropping plugin timer after exhausting delivery attempts"
                );
                delete_timer(&state.db, row.id).await;
            } else {
                warn!(
                    plugin_id = %row.plugin_id,
                    key = %row.key,
                    attempts = row.attempts,
                    error = %e,
                    "Plugin timer delivery failed; will retry after lease expiry"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use sea_orm::{ActiveModelTrait, Set};
    use testcontainers::ContainerAsync;
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    /// Owns the Postgres container for the lifetime of a test's `db` binding,
    /// mirroring `host_funcs::timer::tests::TestDb`.
    struct TestDb {
        _container: ContainerAsync<Postgres>,
        conn: DatabaseConnection,
    }

    impl std::ops::Deref for TestDb {
        type Target = DatabaseConnection;
        fn deref(&self) -> &DatabaseConnection {
            &self.conn
        }
    }

    async fn test_db() -> TestDb {
        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container
            .get_host_port_ipv4(5432)
            .await
            .expect("postgres host port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let conn = crate::database::init_db(&url)
            .await
            .expect("init schema on test db");
        TestDb {
            _container: container,
            conn,
        }
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    /// Seeds one `plugin_timer` row directly, bypassing `timer_schedule`
    /// (exercised by `host_funcs::timer`'s own tests). `claimed_at =
    /// Some(_)` simulates a row a prior tick already claimed once, so
    /// `attempts` starts at 1 -- matching the invariant `claim_due` itself
    /// maintains (every claim increments `attempts` by one).
    async fn seed_timer(
        db: &DatabaseConnection,
        plugin_id: &str,
        key: &str,
        fire_at: DateTime<Utc>,
        claimed_at: Option<DateTime<Utc>>,
    ) {
        let attempts = if claimed_at.is_some() { 1 } else { 0 };
        let model = plugin_timer::ActiveModel {
            plugin_id: Set(plugin_id.to_string()),
            key: Set(key.to_string()),
            fire_at: Set(fire_at),
            payload: Set("{}".to_string()),
            claimed_at: Set(claimed_at),
            attempts: Set(attempts),
            created_at: Set(Utc::now()),
            ..Default::default()
        };
        model.insert(db).await.expect("seed plugin_timer row");
    }

    #[tokio::test]
    async fn claim_takes_only_due_unclaimed_rows() {
        let db = test_db().await;
        seed_timer(&db, "p", "due", now() - Duration::seconds(5), None).await;
        seed_timer(&db, "p", "future", now() + Duration::hours(1), None).await;
        seed_timer(
            &db,
            "p",
            "claimed",
            now() - Duration::seconds(5),
            Some(now()),
        )
        .await;

        let claimed = claim_due(&db, Duration::seconds(30), 64).await.unwrap();

        let keys: Vec<&str> = claimed.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, vec!["due"], "only the due, unclaimed timer is taken");
    }

    #[tokio::test]
    async fn claim_reclaims_a_row_whose_lease_expired() {
        let db = test_db().await;
        // A replica claimed this and died 60s ago; the 30s lease has expired.
        seed_timer(
            &db,
            "p",
            "stranded",
            now() - Duration::minutes(5),
            Some(now() - Duration::seconds(60)),
        )
        .await;

        let claimed = claim_due(&db, Duration::seconds(30), 64).await.unwrap();

        assert_eq!(claimed.len(), 1, "an expired lease must be reclaimable");
        assert_eq!(
            claimed[0].attempts, 2,
            "reclaim increments the attempt count"
        );
    }

    #[tokio::test]
    async fn claim_respects_the_batch_limit() {
        let db = test_db().await;
        for i in 0..5 {
            seed_timer(
                &db,
                "p",
                &format!("k{i}"),
                now() - Duration::seconds(5),
                None,
            )
            .await;
        }

        let claimed = claim_due(&db, Duration::seconds(30), 2).await.unwrap();

        assert_eq!(claimed.len(), 2, "batch limit must bound one claim tick");
    }

    #[tokio::test]
    async fn unloading_a_plugin_deletes_its_pending_timers() {
        let db = test_db().await;
        seed_timer(&db, "doomed", "k", now() + Duration::hours(1), None).await;
        seed_timer(&db, "survivor", "k", now() + Duration::hours(1), None).await;

        delete_timers_for_plugin(&db, "doomed").await.unwrap();

        let left = plugin_timer::Entity::find().all(&*db).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(
            left[0].plugin_id, "survivor",
            "only the unloaded plugin's timers go"
        );
    }

    #[tokio::test]
    async fn two_concurrent_claimers_take_a_timer_exactly_once() {
        // Two ticks racing for one due row must not both deliver it: a
        // duplicate claim means a duplicate callback, and while delivery is
        // documented as at-least-once, that is a crash-recovery allowance,
        // not licence to double-fire on the happy path.
        //
        // What this does and does NOT isolate, established by mutation rather
        // than argument:
        //
        // - Remove ONLY `SKIP LOCKED`: this test still passes. Under READ
        //   COMMITTED a blocked `FOR UPDATE` re-evaluates its WHERE clause
        //   against the committed row version, sees `claimed_at` set, and
        //   claims nothing.
        // - Remove ONLY the `claimed_at` predicate: this test still passes.
        //   `SKIP LOCKED` makes the second claimer skip the locked row.
        // - Remove BOTH: this test fails with `left: 2, right: 1` -- a real
        //   duplicate delivery.
        //
        // So the two mechanisms are redundant by design, and this test pins
        // their conjunction, not either one alone. Nothing here isolates
        // `SKIP LOCKED`; its value is that a claimer never waits on another
        // claimer's row lock, which is a liveness property this suite does not
        // measure. Said plainly so a future reader does not mistake a passing
        // suite for proof that the clause is load-bearing on its own.
        let db = test_db().await;
        seed_timer(&db, "p", "contended", now() - Duration::seconds(5), None).await;

        let (a, b) = tokio::join!(
            claim_due(&db, Duration::seconds(30), 64),
            claim_due(&db, Duration::seconds(30), 64),
        );
        let total = a.unwrap().len() + b.unwrap().len();

        assert_eq!(total, 1, "exactly one claimer takes the row");
    }

    #[tokio::test]
    async fn a_contended_batch_is_partitioned_between_claimers_not_duplicated() {
        // The batch-sized version of the sibling above. A single row can pass
        // by a lucky interleaving; 64 cannot. With both mechanisms removed
        // this fails `left: 128, right: 64` -- each claimer took the whole
        // batch.
        //
        // An earlier draft of this test also asserted an elapsed-time ceiling,
        // intending to isolate `SKIP LOCKED` via its liveness effect. Measured
        // against the real mutation, that assertion never fired: on a local
        // container the blocked claimer waits milliseconds, so the timing
        // bound passed with the clause removed. It was decoration implying
        // coverage it did not provide, and is gone. A test that cannot fail is
        // worse than no test, because it converts an open question into
        // documented confidence.
        let db = test_db().await;
        for i in 0..64 {
            seed_timer(
                &db,
                "p",
                &format!("c{i}"),
                now() - Duration::seconds(5),
                None,
            )
            .await;
        }

        let (a, b) = tokio::join!(
            claim_due(&db, Duration::seconds(30), 64),
            claim_due(&db, Duration::seconds(30), 64),
        );
        let total = a.unwrap().len() + b.unwrap().len();

        assert_eq!(
            total, 64,
            "every due row is claimed exactly once across both claimers"
        );
    }
}
