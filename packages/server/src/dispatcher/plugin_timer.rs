//! Delivers due plugin timers.
//!
//! Spawned **unconditionally** by `Dispatcher::spawn` -- independent of
//! `dispatcher_lease_steal_enabled`, which governs submission judging, not
//! plugin timers. A deployment that disables lease/steal and the claim fiber
//! still wants a plugin's `[[server.timers]]` callback to fire.
//!
//! Each tick claims due rows in ONE statement (`claim_due`, private) and then
//! invokes each claimed row's plugin OUTSIDE any transaction. Holding a
//! transaction across a plugin invocation and then acquiring a second pooled
//! connection inside it can exhaust the pool and deadlock.
//!
//! Delivery is **at-least-once with bounded retries, then drop**. This
//! cannot fail closed: a timer's only safe outcomes are "eventually fires"
//! or "dropped loudly after `max_attempts`" -- "never fires" is precisely
//! the failure this capability exists to prevent, so a repeatedly-trapping
//! plugin's timer is dropped with an `error!` log rather than retried
//! forever, and the loop keeps delivering every other plugin's timers in the
//! meantime.
//!
//! `attempts` counts deliveries that started. The claim statement is the
//! single owner of the retry budget: it never claims a row whose budget is
//! spent and, in the same statement, deletes every such row whose last
//! claim's lease has expired, however that last delivery ended (a trap, a
//! hang, or a crashed replica).
//!
//! A delivery never outlives its claim's lease, so no two replicas run the
//! same timer at once. A tick starts a delivery only while the lease still
//! has room for a whole plugin call (`TimerConfig::call_budget`), cancels a
//! guest still running when the lease ends, and hands the rows it did not
//! reach back unclaimed, with their attempt refunded.

use std::time::Duration as StdDuration;

use plugin_core::error::PluginError;
use plugin_core::registry::PluginStatus;
use plugin_core::traits::CallOptions;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    FromQueryResult, QueryFilter, Statement,
};
use serde::Serialize;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::entity::plugin_timer;
use crate::state::AppState;

/// Config for the plugin-timer delivery loop. Defaults: 1s tick, 330s claim
/// lease, 64-row batch, 5 delivery attempts before a timer is dropped, and the
/// default plugin call timeout as the call budget.
#[derive(Debug, Clone, Copy)]
pub struct TimerConfig {
    pub tick_interval_secs: u64,
    pub lease_secs: i64,
    pub batch: u64,
    pub max_attempts: i32,
    /// The longest one delivery may need: the plugin call timeout. A delivery
    /// starts only while the lease has at least this much left, so one that
    /// starts is cut short by its own call timeout, not by the lease.
    pub call_budget: StdDuration,
}

impl Default for TimerConfig {
    fn default() -> Self {
        Self {
            tick_interval_secs: 1,
            lease_secs: crate::config::DEFAULT_PLUGIN_TIMER_LEASE_SECS,
            batch: 64,
            max_attempts: 5,
            call_budget: plugin_core::config::PluginConfig::default().call_timeout(),
        }
    }
}

impl TimerConfig {
    fn lease(&self) -> StdDuration {
        StdDuration::from_secs(self.lease_secs.max(0) as u64)
    }
}

/// Outcome of one [`tick_once`] call, exposed so integration tests can race
/// two concurrent ticks and assert their claimed counts sum to exactly one
/// row (see `tests/integration/plugin_timer.rs`,
/// `two_concurrent_claimers_deliver_a_timer_once`).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TickStats {
    /// Rows claimed for delivery.
    pub claimed: usize,
    /// Rows deleted because their retry budget was spent.
    pub dropped: usize,
}

/// Long-running loop. Polls every `config.tick_interval_secs` until the
/// cancel channel fires. Mirrors the shape of `dispatcher::claim::run`.
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
                // A tick's deliveries end with its lease, so the tick is
                // bounded by the lease plus the usual statement budget.
                let budget = config.lease() + common::deadline::BACKGROUND_STEP;
                if let Some(Err(e)) =
                    common::deadline::bounded("dispatcher.plugin_timer", budget, tick_once(&state, &config)).await
                {
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
/// tests can drive it directly through `server::dispatcher::plugin_timer`.
pub async fn tick_once(state: &AppState, config: &TimerConfig) -> Result<TickStats, DbErr> {
    let Ok(loaded) = loaded_plugin_ids(state) else {
        error!("Plugin registry lock poisoned; skipping plugin timer tick");
        return Ok(TickStats::default());
    };
    if loaded.is_empty() {
        return Ok(TickStats::default());
    }
    // Taken before the claim statement runs, so the local lease below ends no
    // later than the one the database recorded.
    let claim_started = tokio::time::Instant::now();
    let Claim { due, exhausted } = claim_due(&state.db, config, &loaded).await?;
    for row in &exhausted {
        error!(
            plugin_id = %row.plugin_id,
            key = %row.key,
            attempts = row.attempts,
            "Dropping plugin timer after exhausting delivery attempts"
        );
    }
    let stats = TickStats {
        claimed: due.len(),
        dropped: exhausted.len(),
    };

    // Deliver only while the claim's lease holds: past it another replica may
    // re-claim the rows, so a guest still running here would run twice. A
    // delivery starts only with a whole call budget of lease left, and one
    // still running when the lease ends is cancelled and keeps its attempt;
    // the rows not reached are handed back with theirs refunded.
    let lease_ends = claim_started + config.lease();
    let mut rows = due.into_iter().peekable();
    while rows.peek().is_some_and(|_| {
        lease_ends.saturating_duration_since(tokio::time::Instant::now()) >= config.call_budget
    }) {
        let Some(row) = rows.next() else { break };
        if deliver(state, &row, lease_ends.into_std()).await == Delivery::LeaseEnded {
            warn!(
                plugin_id = %row.plugin_id,
                key = %row.key,
                attempts = row.attempts,
                "Plugin timer delivery was cancelled at the end of its claim lease; it will be retried"
            );
            break;
        }
    }
    let unreached: Vec<plugin_timer::Model> = rows.collect();
    if !unreached.is_empty() {
        release_unreached(&state.db, &unreached).await;
    }
    Ok(stats)
}

/// The result of one [`claim_due`] statement.
#[derive(Debug, Default)]
struct Claim {
    /// Rows claimed for delivery, `attempts` already counting this one.
    due: Vec<plugin_timer::Model>,
    /// Rows deleted because their retry budget is spent.
    exhausted: Vec<plugin_timer::Model>,
}

#[derive(FromQueryResult)]
struct ClaimRow {
    id: i64,
    plugin_id: String,
    key: String,
    fire_at: chrono::DateTime<chrono::Utc>,
    payload: String,
    claimed_at: Option<chrono::DateTime<chrono::Utc>>,
    attempts: i32,
    created_at: chrono::DateTime<chrono::Utc>,
    exhausted: bool,
}

/// Claims due timers and drops exhausted ones in ONE statement.
///
/// A row is claimed only while `attempts < max_attempts`. A row whose budget
/// is spent is deleted once its last claim's lease has expired, whichever
/// replica made that claim and whatever became of the delivery; only the
/// lease guards it, so it is dropped even if its plugin is not loaded here.
///
/// Two independent mechanisms stop two replicas (or two concurrent ticks)
/// from both claiming the same row, and they are deliberately redundant:
///
/// - the `claimed_at` predicate excludes a row another claimer already took,
///   within the lease window;
/// - `FOR UPDATE SKIP LOCKED` makes a claimer skip a row another claimer
///   currently holds, rather than blocking on its lock.
///
/// Either alone keeps each timer claimed once, which
/// `two_concurrent_claimers_take_a_timer_exactly_once` checks. `SKIP LOCKED`
/// also keeps claiming live: no claimer waits on another's row lock.
///
/// Only timers of `plugin_ids` (the plugins `Loaded` on this replica) are
/// claimed. Any replica may claim any due row, and replicas do not always
/// agree on which plugins are loaded (one is still converging onto an
/// enable, or failed to load a plugin its peers run fine). A replica that
/// cannot deliver a timer therefore leaves it for one that can, instead of
/// burning a delivery attempt on it or deleting it.
///
/// Separately load-bearing: no transaction is held across the plugin
/// invocation that follows. `claim_due` returns before `deliver` ever touches
/// the plugin host, so this statement's connection is back in the pool first.
/// Holding one and then acquiring a second pooled connection inside it is the
/// pool-exhaustion deadlock the module docs describe.
async fn claim_due(
    db: &DatabaseConnection,
    config: &TimerConfig,
    plugin_ids: &[String],
) -> Result<Claim, DbErr> {
    let sql = r#"
        WITH exhausted AS (
          DELETE FROM plugin_timer
          WHERE attempts >= $4
            AND claimed_at < now() - ($1 || ' seconds')::interval
          RETURNING id, plugin_id, key, fire_at, payload, claimed_at, attempts, created_at
        ), due AS (
          UPDATE plugin_timer
          SET claimed_at = now(), attempts = attempts + 1
          WHERE id IN (
            SELECT id FROM plugin_timer
            WHERE fire_at <= now()
              AND attempts < $4
              AND (claimed_at IS NULL OR claimed_at < now() - ($1 || ' seconds')::interval)
              AND plugin_id = ANY($3)
            ORDER BY fire_at
            LIMIT $2
            FOR UPDATE SKIP LOCKED
          )
          RETURNING id, plugin_id, key, fire_at, payload, claimed_at, attempts, created_at
        )
        SELECT *, FALSE AS exhausted FROM due
        UNION ALL
        SELECT *, TRUE AS exhausted FROM exhausted
    "#;
    let rows = ClaimRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        [
            config.lease_secs.into(),
            (config.batch as i64).into(),
            plugin_ids.to_vec().into(),
            config.max_attempts.into(),
        ],
    ))
    .all(db)
    .await?;
    let mut claim = Claim::default();
    for row in rows {
        let model = plugin_timer::Model {
            id: row.id,
            plugin_id: row.plugin_id,
            key: row.key,
            fire_at: row.fire_at,
            payload: row.payload,
            claimed_at: row.claimed_at,
            attempts: row.attempts,
            created_at: row.created_at,
        };
        if row.exhausted {
            claim.exhausted.push(model);
        } else {
            claim.due.push(model);
        }
    }
    // `due` keeps `fire_at` order for delivery.
    claim.due.sort_by_key(|row| row.fire_at);
    Ok(claim)
}

/// Hand claimed rows a tick did not start delivering back to the pool,
/// refunding the attempt their claim counted. Only rows still holding this
/// claim are touched: a row rescheduled meanwhile keeps its new schedule.
/// Errors are logged: the rows then stay claimed and are retried after the
/// lease with one attempt spent.
async fn release_unreached(db: &DatabaseConnection, rows: &[plugin_timer::Model]) {
    let (ids, claimed_at): (Vec<i64>, Vec<chrono::DateTime<chrono::Utc>>) = rows
        .iter()
        .filter_map(|row| Some((row.id, row.claimed_at?)))
        .unzip();
    let sql = r#"
        UPDATE plugin_timer AS t
        SET claimed_at = NULL, attempts = t.attempts - 1
        FROM unnest($1::bigint[], $2::timestamptz[]) AS c(id, claimed_at)
        WHERE t.id = c.id AND t.claimed_at = c.claimed_at
    "#;
    if let Err(e) = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            [ids.into(), claimed_at.into()],
        ))
        .await
    {
        error!(error = %e, rows = rows.len(), "Failed to release unreached plugin timers");
    }
}

/// Deletes every pending timer belonging to `plugin_id`. Called only when an
/// admin disables the plugin (`plugin_lifecycle::disable`): that disables it
/// on every replica, so its timers can never be delivered again. NOT called
/// when a plugin merely fails to load on one replica (a healthy peer is
/// still delivering them; see `claim_due`), nor on reload, which swaps the
/// WASM runtime under the same plugin id and keeps its timers valid.
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

/// Deletes a `plugin_timer` row after successful delivery, or on
/// discovering the owning plugin can never receive it. Errors are logged, not propagated: the row is left claimed either
/// way, so the worst case is a redelivery after the lease expires, not a
/// silent loss.
async fn delete_timer(db: &DatabaseConnection, row: &plugin_timer::Model) {
    // A callback may reschedule its own key (or another caller may replace it
    // during delivery). Scheduling clears the claim, so only delete the exact
    // delivery we claimed, never the replacement's future wakeup.
    if let Err(e) = plugin_timer::Entity::delete_by_id(row.id)
        .filter(plugin_timer::Column::ClaimedAt.eq(row.claimed_at))
        .filter(plugin_timer::Column::Attempts.eq(row.attempts))
        .exec(db)
        .await
    {
        error!(id = row.id, error = %e, "Failed to delete plugin_timer row after delivery");
    }
}

/// Ids of the plugins currently `Loaded` on this replica.
fn loaded_plugin_ids(state: &AppState) -> Result<Vec<String>, ()> {
    let registry = state.plugins.get_registry().read().map_err(|_| ())?;
    Ok(registry
        .values()
        .filter(|e| e.status == PluginStatus::Loaded)
        .map(|e| e.id.clone())
        .collect())
}

/// Looks up the `[[server.timers]] function` a `Loaded` plugin declared.
/// Returns `Ok(None)` for an unknown plugin, one that isn't `Loaded`, or one
/// with no declaration. `claim_due` only claims rows of `Loaded` plugins, so
/// in practice `None` means the plugin dropped the declaration while timers
/// were still pending (or was unloaded between claim and delivery), and the
/// caller drops the row with a warning.
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

/// How one delivery ended, as far as the tick is concerned.
#[derive(Debug, PartialEq, Eq)]
enum Delivery {
    /// The delivery finished (delivered, failed, or dropped) before the lease
    /// ended.
    Done,
    /// The guest was still running when the claim's lease ended and was
    /// cancelled.
    LeaseEnded,
}

/// Delivers one claimed row: invokes the plugin's declared timer function,
/// cancelling it if it is still running at `lease_ends`, then deletes the row
/// on success, or otherwise leaves it claimed for lease-expiry redelivery (or,
/// with its budget spent, for [`claim_due`] to drop).
async fn deliver(
    state: &AppState,
    row: &plugin_timer::Model,
    lease_ends: std::time::Instant,
) -> Delivery {
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
            delete_timer(&state.db, row).await;
            return Delivery::Done;
        }
        Err(()) => {
            error!(
                plugin_id = %row.plugin_id,
                key = %row.key,
                "Plugin registry lock poisoned while resolving the timer handler; leaving row \
                 claimed for lease-expiry redelivery"
            );
            return Delivery::Done;
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
            return Delivery::Done;
        }
    };

    match state
        .plugins
        .call_raw_with(
            &row.plugin_id,
            &function,
            input_bytes,
            CallOptions::until(lease_ends),
        )
        .await
    {
        Ok(_) => {
            delete_timer(&state.db, row).await;
            Delivery::Done
        }
        Err(PluginError::DeadlineExceeded { .. }) => Delivery::LeaseEnded,
        Err(e) => {
            warn!(
                plugin_id = %row.plugin_id,
                key = %row.key,
                attempts = row.attempts,
                error = %e,
                "Plugin timer delivery failed; will retry after lease expiry"
            );
            Delivery::Done
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, Utc};
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
        // The plugin these tests schedule for (only an enabled plugin may arm
        // a timer through `host_funcs::timer::schedule`).
        sea_orm::ConnectionTrait::execute_unprepared(
            &conn,
            "INSERT INTO plugin (id, is_enabled, generation, updated_at) VALUES ('p', TRUE, 0, NOW())",
        )
        .await
        .expect("seed plugin row");
        TestDb {
            _container: container,
            conn,
        }
    }

    /// The plugins a test claimer has loaded.
    fn ids() -> Vec<String> {
        vec!["p".to_string()]
    }

    /// A 30s lease, `batch` rows, and the default retry budget.
    fn config(batch: u64) -> TimerConfig {
        TimerConfig {
            lease_secs: 30,
            batch,
            ..Default::default()
        }
    }

    /// One claim by a claimer that has plugin `p` loaded; the rows due.
    async fn claim(db: &DatabaseConnection, batch: u64) -> Vec<plugin_timer::Model> {
        claim_due(db, &config(batch), &ids()).await.unwrap().due
    }

    async fn find(db: &DatabaseConnection, key: &str) -> Option<plugin_timer::Model> {
        plugin_timer::Entity::find()
            .filter(plugin_timer::Column::Key.eq(key))
            .one(db)
            .await
            .unwrap()
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

    async fn set_attempts(db: &DatabaseConnection, key: &str, attempts: i32) {
        plugin_timer::Entity::update_many()
            .col_expr(
                plugin_timer::Column::Attempts,
                sea_orm::sea_query::Expr::value(attempts),
            )
            .filter(plugin_timer::Column::Key.eq(key))
            .exec(db)
            .await
            .unwrap();
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

        let claimed = claim(&db, 64).await;

        let keys: Vec<&str> = claimed.iter().map(|t| t.key.as_str()).collect();
        assert_eq!(keys, vec!["due"], "only the due, unclaimed timer is taken");
    }

    #[tokio::test]
    async fn delivered_timer_cleanup_preserves_a_rescheduled_wakeup() {
        let db = test_db().await;
        seed_timer(&db, "p", "rearm", now() - Duration::seconds(1), None).await;
        let claimed = claim(&db, 64).await;
        let delivery = &claimed[0];
        crate::host_funcs::timer::schedule(
            &db,
            "p",
            "rearm",
            (now() + Duration::seconds(60)).timestamp_millis(),
            "next",
        )
        .await
        .unwrap();
        delete_timer(&db, delivery).await;
        let next = plugin_timer::Entity::find_by_id(delivery.id)
            .one(&*db)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next.payload, "next");
        assert!(next.claimed_at.is_none());

        seed_timer(&db, "p", "finished", now() - Duration::seconds(1), None).await;
        let claimed = claim(&db, 64).await;
        delete_timer(&db, &claimed[0]).await;
        assert!(
            plugin_timer::Entity::find_by_id(claimed[0].id)
                .one(&*db)
                .await
                .unwrap()
                .is_none()
        );
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

        let claimed = claim(&db, 64).await;

        assert_eq!(claimed.len(), 1, "an expired lease must be reclaimable");
        assert_eq!(
            claimed[0].attempts, 2,
            "reclaim increments the attempt count"
        );
    }

    #[tokio::test]
    async fn claim_never_takes_a_row_whose_budget_is_spent() {
        let db = test_db().await;
        let max = TimerConfig::default().max_attempts;
        seed_timer(
            &db,
            "p",
            "spent",
            now() - Duration::minutes(5),
            Some(now() - Duration::seconds(60)),
        )
        .await;
        set_attempts(&db, "spent", max).await;

        let claim = claim_due(&db, &config(64), &ids()).await.unwrap();

        assert!(claim.due.is_empty(), "a spent row is never claimed again");
        assert_eq!(claim.exhausted.len(), 1);
        assert_eq!(claim.exhausted[0].key, "spent");
        assert!(find(&db, "spent").await.is_none(), "and it is deleted");
    }

    #[tokio::test]
    async fn a_spent_row_is_kept_until_its_last_lease_expires() {
        // Its last delivery may still be running on the claimer.
        let db = test_db().await;
        let max = TimerConfig::default().max_attempts;
        seed_timer(&db, "p", "last", now() - Duration::minutes(5), Some(now())).await;
        set_attempts(&db, "last", max).await;

        let claim = claim_due(&db, &config(64), &ids()).await.unwrap();

        assert!(claim.due.is_empty() && claim.exhausted.is_empty());
        assert!(find(&db, "last").await.is_some());
    }

    #[tokio::test]
    async fn a_spent_row_is_dropped_even_if_its_plugin_is_not_loaded_here() {
        let db = test_db().await;
        let max = TimerConfig::default().max_attempts;
        seed_timer(
            &db,
            "elsewhere",
            "spent",
            now() - Duration::minutes(5),
            Some(now() - Duration::seconds(60)),
        )
        .await;
        set_attempts(&db, "spent", max).await;

        let claim = claim_due(&db, &config(64), &ids()).await.unwrap();

        assert_eq!(claim.exhausted.len(), 1);
    }

    #[tokio::test]
    async fn released_rows_are_unclaimed_with_their_attempt_refunded() {
        let db = test_db().await;
        seed_timer(&db, "p", "a", now() - Duration::seconds(5), None).await;
        seed_timer(&db, "p", "b", now() - Duration::seconds(4), None).await;
        let claimed = claim(&db, 64).await;
        assert_eq!(claimed.len(), 2);

        release_unreached(&db, &claimed[1..]).await;

        let a = find(&db, "a").await.unwrap();
        assert_eq!((a.attempts, a.claimed_at.is_some()), (1, true));
        let b = find(&db, "b").await.unwrap();
        assert_eq!((b.attempts, b.claimed_at), (0, None));
        assert_eq!(
            claim(&db, 64)
                .await
                .iter()
                .map(|r| r.key.as_str())
                .collect::<Vec<_>>(),
            vec!["b"],
            "a released row is immediately claimable again"
        );
    }

    #[tokio::test]
    async fn release_leaves_a_row_rescheduled_since_the_claim() {
        let db = test_db().await;
        seed_timer(&db, "p", "rearm", now() - Duration::seconds(5), None).await;
        let claimed = claim(&db, 64).await;
        crate::host_funcs::timer::schedule(
            &db,
            "p",
            "rearm",
            (now() + Duration::seconds(60)).timestamp_millis(),
            "next",
        )
        .await
        .unwrap();

        release_unreached(&db, &claimed).await;

        let row = find(&db, "rearm").await.unwrap();
        assert_eq!((row.attempts, row.payload.as_str()), (0, "next"));
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

        let claimed = claim(&db, 2).await;

        assert_eq!(claimed.len(), 2, "batch limit must bound one claim tick");
    }

    #[tokio::test]
    async fn claim_leaves_timers_of_plugins_not_loaded_here() {
        let db = test_db().await;
        seed_timer(&db, "p", "mine", now() - Duration::seconds(5), None).await;
        seed_timer(&db, "elsewhere", "k", now() - Duration::seconds(5), None).await;

        let claimed = claim(&db, 64).await;
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].plugin_id, "p");

        let left = plugin_timer::Entity::find()
            .filter(plugin_timer::Column::PluginId.eq("elsewhere"))
            .one(&*db)
            .await
            .unwrap()
            .expect("the other plugin's timer must be left in place");
        assert_eq!(left.claimed_at, None, "and left unclaimed");
        assert_eq!(left.attempts, 0, "without burning a delivery attempt");
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

        let ids = ids();
        let config = config(64);
        let (a, b) = tokio::join!(claim_due(&db, &config, &ids), claim_due(&db, &config, &ids),);
        let total = a.unwrap().due.len() + b.unwrap().due.len();

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

        let ids = ids();
        let config = config(64);
        let (a, b) = tokio::join!(claim_due(&db, &config, &ids), claim_due(&db, &config, &ids),);
        let total = a.unwrap().due.len() + b.unwrap().due.len();

        assert_eq!(
            total, 64,
            "every due row is claimed exactly once across both claimers"
        );
    }
}
