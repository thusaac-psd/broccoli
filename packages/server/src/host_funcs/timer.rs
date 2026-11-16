use common::db::DatabaseConnection;
use std::sync::Arc;

use extism::host_fn;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait, QueryFilter, Statement,
};
use serde::Deserialize;
use tracing::error;

use crate::entity::plugin_timer;

/// Timers carry references, not documents. A plugin that needs more state
/// should key into its own storage.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
/// Bounds one plugin's ability to exhaust the table.
pub(crate) const MAX_PENDING_PER_PLUGIN: u64 = 10_000;

pub(crate) fn validate_payload(payload: &str) -> Result<(), String> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(format!(
            "timer payload is {} bytes, over the {MAX_PAYLOAD_BYTES}-byte cap",
            payload.len()
        ));
    }
    Ok(())
}

#[derive(Deserialize)]
struct ScheduleInput {
    fire_at_ms: i64,
    key: String,
    payload: String,
}

#[derive(Deserialize)]
struct CancelInput {
    key: String,
}

/// Schedule (or reschedule) `key` for `plugin_id` to fire at `fire_at_ms`.
///
/// The pending-count cap is enforced with a count-then-insert check, not
/// atomically with the insert below: a concurrent scheduler for the SAME
/// plugin can race past the cap by a handful of rows. That is an accepted
/// soft-quota overshoot (see `MAX_PENDING_PER_PLUGIN`'s doc comment) -
/// overshooting a soft quota is acceptable, a lost timer is not, and making
/// this atomic would mean holding a transaction across the check, which is
/// the self-deadlock shape this codebase has already been bitten by twice.
///
/// The count excludes `key` itself: rescheduling an existing key replaces its
/// row rather than adding one, so it must not be blocked by a cap it does not
/// grow into (this is the boundary case a naive count-then-insert gets wrong
/// at exactly the cap).
pub(crate) async fn schedule(
    db: &DatabaseConnection,
    plugin_id: &str,
    key: &str,
    fire_at_ms: i64,
    payload: &str,
) -> Result<(), extism::Error> {
    validate_payload(payload).map_err(extism::Error::msg)?;

    let pending = plugin_timer::Entity::find()
        .filter(plugin_timer::Column::PluginId.eq(plugin_id))
        .filter(plugin_timer::Column::Key.ne(key))
        .count(db)
        .await
        .map_err(|e| {
            error!("DB timer_schedule count error: {e}");
            extism::Error::msg("Database error")
        })?;

    if pending >= MAX_PENDING_PER_PLUGIN {
        return Err(extism::Error::msg(format!(
            "plugin {plugin_id} already has {pending} pending timers, at the \
             {MAX_PENDING_PER_PLUGIN}-timer cap"
        )));
    }

    let fire_at = chrono::DateTime::from_timestamp_millis(fire_at_ms)
        .ok_or_else(|| extism::Error::msg("fire_at_ms is out of range"))?;

    // Only an enabled plugin may arm a timer. A replica that has not yet
    // unloaded a plugin another replica just disabled would otherwise re-arm
    // timers right after the disable deleted them. `FOR SHARE` on the
    // plugin row serializes this insert against that disable: either the
    // disable's update waits for this insert to commit (and its delete then
    // removes the row), or this insert waits for the disable and then sees
    // the plugin disabled.
    //
    // Replace semantics on conflict: rescheduling an existing key resets the
    // delivery state too, so a previously-failed timer does not inherit its
    // old attempt count.
    let inserted = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO plugin_timer (plugin_id, key, fire_at, payload, claimed_at, attempts, created_at)
               SELECT $1, $2, $3, $4, NULL, 0, NOW()
                WHERE EXISTS (SELECT 1 FROM plugin WHERE id = $1 AND is_enabled FOR SHARE)
               ON CONFLICT (plugin_id, key) DO UPDATE
                 SET fire_at = EXCLUDED.fire_at,
                     payload = EXCLUDED.payload,
                     claimed_at = EXCLUDED.claimed_at,
                     attempts = EXCLUDED.attempts"#,
            [
                plugin_id.into(),
                key.into(),
                fire_at.into(),
                payload.into(),
            ],
        ))
        .await
        .map_err(|e| {
            error!("DB timer_schedule insert error: {e}");
            extism::Error::msg("Database error")
        })?
        .rows_affected();
    if inserted == 0 {
        return Err(extism::Error::msg(format!(
            "plugin {plugin_id} is not enabled, so it cannot schedule timers"
        )));
    }

    Ok(())
}

/// Cancel a pending timer. Deleting zero rows is success, not an error - an
/// already-fired timer and a never-scheduled one are indistinguishable to the
/// caller and both mean "nothing pending."
pub(crate) async fn cancel(
    db: &DatabaseConnection,
    plugin_id: &str,
    key: &str,
) -> Result<(), extism::Error> {
    plugin_timer::Entity::delete_many()
        .filter(plugin_timer::Column::PluginId.eq(plugin_id))
        .filter(plugin_timer::Column::Key.eq(key))
        .exec(db)
        .await
        .map_err(|e| {
            error!("DB timer_cancel error: {e}");
            extism::Error::msg("Database error")
        })?;
    Ok(())
}

host_fn!(pub timer_schedule(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "timer_schedule")?;
    let (plugin_id, db) = &*call;

    let parsed: ScheduleInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid timer_schedule input: {e}")))?;

    tokio::runtime::Handle::current()
        .block_on(schedule(db, plugin_id, &parsed.key, parsed.fire_at_ms, &parsed.payload))?;

    Ok(serde_json::json!({ "ok": true }).to_string())
});

host_fn!(pub timer_cancel(user_data: Arc<(String, DatabaseConnection)>; input: String) -> String {
    let call = super::HostCall::enter(&user_data, "timer_cancel")?;
    let (plugin_id, db) = &*call;

    let parsed: CancelInput = serde_json::from_str(&input)
        .map_err(|e| extism::Error::msg(format!("Invalid timer_cancel input: {e}")))?;

    tokio::runtime::Handle::current()
        .block_on(cancel(db, plugin_id, &parsed.key))?;

    Ok(serde_json::json!({ "ok": true }).to_string())
});

#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Fixtures are stamped with real time.
mod tests {
    use super::*;
    use testcontainers::ContainerAsync;
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    /// Owns the Postgres container for the lifetime of a test's `db` binding
    /// (deref-coerces to `&DatabaseConnection` at call sites), mirroring
    /// `services::submission_dispatch::tests::start_pg`.
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
        // The plugins these tests schedule for, enabled (only an enabled
        // plugin may arm a timer).
        conn.execute_unprepared(
            "INSERT INTO plugin (id, is_enabled, generation, updated_at) VALUES \
             ('p', TRUE, 0, NOW()), ('greedy', TRUE, 0, NOW()), ('atcap', TRUE, 0, NOW()), \
             ('off', FALSE, 0, NOW())",
        )
        .await
        .expect("seed plugin rows");
        TestDb {
            _container: container,
            conn,
        }
    }

    #[tokio::test]
    async fn a_disabled_plugin_cannot_arm_a_timer() {
        let db = test_db().await;
        let err = schedule(&db, "off", "k", future_ms(), "{}")
            .await
            .expect_err("a disabled plugin must not schedule");
        assert!(err.to_string().contains("not enabled"), "{err}");
        let n = plugin_timer::Entity::find().count(&*db).await.unwrap();
        assert_eq!(n, 0);
    }

    /// Reschedule-by-key must keep working after the server RESTARTS, not
    /// just on the fresh database every other test here gets.
    ///
    /// `timer_schedule` relies on `INSERT ... ON CONFLICT (plugin_id, key)`,
    /// which needs the unique index on those columns to survive every boot.
    /// The second `init_db` below is the restart.
    #[tokio::test]
    async fn reschedule_still_upserts_after_a_server_restart() {
        let db = test_db().await;
        let url = {
            let port = db
                ._container
                .get_host_port_ipv4(5432)
                .await
                .expect("postgres host port");
            format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres")
        };
        let restarted = crate::database::init_db(&url)
            .await
            .expect("second boot: sync + migrations on an existing schema");

        schedule(&restarted, "p", "k", future_ms(), "first")
            .await
            .expect("first schedule after restart");
        schedule(&restarted, "p", "k", future_ms(), "second")
            .await
            .expect("rescheduling the same key after a restart must upsert, not fail");

        let rows = plugin_timer::Entity::find()
            .all(&restarted)
            .await
            .expect("read timers");
        assert_eq!(rows.len(), 1, "replace, not duplicate: {rows:?}");
        assert_eq!(rows[0].payload, "second");
    }

    fn future_ms() -> i64 {
        (chrono::Utc::now() + chrono::Duration::hours(1)).timestamp_millis()
    }

    #[test]
    fn rejects_oversized_payload() {
        let payload = "x".repeat(MAX_PAYLOAD_BYTES + 1);
        let err = validate_payload(&payload).unwrap_err();
        assert!(
            err.contains("payload"),
            "error must name the payload, got: {err}"
        );
    }

    #[test]
    fn accepts_payload_at_the_cap() {
        // Boundary, not just over: an off-by-one in the check would let a
        // payload one byte over through, or reject a legal one.
        assert!(validate_payload(&"x".repeat(MAX_PAYLOAD_BYTES)).is_ok());
    }

    #[tokio::test]
    async fn scheduling_past_the_pending_cap_is_rejected() {
        // Without this, one plugin scheduling in a loop can exhaust the table
        // and starve every other plugin's timers. The cap is the only thing
        // bounding a plugin's footprint here.
        let db = test_db().await;
        for i in 0..MAX_PENDING_PER_PLUGIN {
            schedule(&db, "greedy", &format!("k{i}"), future_ms(), "{}")
                .await
                .unwrap();
        }
        let err = schedule(&db, "greedy", "one-too-many", future_ms(), "{}")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("pending"), "got: {err}");
    }

    #[tokio::test]
    async fn rescheduling_an_existing_key_at_the_cap_still_succeeds() {
        // Boundary the naive check gets wrong: at the cap, REPLACING an
        // existing key adds no row, so it must not be refused. A plugin that
        // reschedules its own deadline would otherwise wedge at the limit.
        let db = test_db().await;
        for i in 0..MAX_PENDING_PER_PLUGIN {
            schedule(&db, "atcap", &format!("k{i}"), future_ms(), "{}")
                .await
                .unwrap();
        }
        assert!(
            schedule(&db, "atcap", "k0", future_ms(), "changed")
                .await
                .is_ok(),
            "replacing an existing key does not grow the table"
        );
    }
}
