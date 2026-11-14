//! Plugin SQL privileges, enforced by Postgres. Each test boots an empty
//! database owned by a non-superuser `LOGIN CREATEROLE` app role through the
//! real migration and drives the real per-plugin pools, so what a statement
//! may do is decided by the engine, not by the SQL guard.

use std::time::Duration;

use common::db::DatabaseConnection;
use futures::FutureExt;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};
use server::plugin_db::PluginSqlPools;
use server::plugin_db::schema::PluginDbNames;
use test_support::TestDatabase;

struct Fixture {
    database: TestDatabase,
    app: DatabaseConnection,
}

impl Fixture {
    async fn new() -> Self {
        let name = format!("plugin_db_{}", uuid::Uuid::new_v4().simple());
        let database = test_support::create_app_owned_database(&name).await;
        let app = server::database::init_db(database.url())
            .await
            .expect("the app role migrates its own database");
        Self { database, app }
    }

    /// One server's plugin pools, `budget` connections together.
    fn pools(&self, budget: u32) -> PluginSqlPools {
        PluginSqlPools::new(
            self.app.clone(),
            self.database.url(),
            budget,
            Duration::from_secs(20),
            &server::database::app_session(&server::config::DatabaseConfig::default()),
        )
    }

    fn names(&self, plugin: &str) -> PluginDbNames {
        let group = server::plugin_db::schema::group_role(self.database.name());
        PluginDbNames::new(self.database.name(), &group, plugin)
    }
}

/// Run `sql` as `plugin`.
async fn exec(pools: &PluginSqlPools, plugin: &str, sql: &str) -> Result<(), DbErr> {
    let lease = pools
        .lease(plugin, &[], None)
        .await
        .map_err(|e| DbErr::Custom(e.to_string()))?;
    lease.db.execute_unprepared(sql).await.map(drop)
}

/// The single text value `sql` returns, run as `plugin`.
async fn text(pools: &PluginSqlPools, plugin: &str, sql: &str) -> String {
    let lease = pools.lease(plugin, &[], None).await.expect("lease");
    lease
        .db
        .query_one_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .expect("a row")
        .try_get_by_index::<String>(0)
        .expect("text")
}

/// `sql` must fail as `plugin` with a privilege error.
async fn denied(pools: &PluginSqlPools, plugin: &str, sql: &str) {
    let err = exec(pools, plugin, sql)
        .await
        .expect_err(&format!("{plugin} must be refused: {sql}"));
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("permission denied") || msg.contains("must be owner"),
        "{sql}: {err}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_has_full_ddl_and_dml_in_its_own_schema() {
    let f = Fixture::new().await;
    let pools = f.pools(2);
    for sql in [
        "CREATE TABLE IF NOT EXISTS job (id BIGSERIAL PRIMARY KEY, v TEXT NOT NULL)",
        "CREATE INDEX job_v_idx ON job (v)",
        "INSERT INTO job (v) VALUES ('a'), ('b')",
        "UPDATE job SET v = 'c' WHERE v = 'a'",
        "DELETE FROM job WHERE v = 'b'",
        "ALTER TABLE job ADD COLUMN n INT",
        "CREATE TABLE child (job_id BIGINT REFERENCES job (id))",
        "CREATE VIEW job_view AS SELECT v FROM job",
        "TRUNCATE job, child",
        "DROP VIEW job_view",
        "DROP TABLE child",
    ] {
        exec(&pools, "print", sql)
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
    }
    // Unqualified DDL landed in the plugin's schema, owned by its role.
    let names = f.names("print");
    let owner = text(
        &pools,
        "print",
        "SELECT tableowner::text FROM pg_tables WHERE schemaname = 'plugin_print' \
         AND tablename = 'job'",
    )
    .await;
    assert_eq!(owner, names.role);
    assert_eq!(
        text(&pools, "print", "SELECT current_user::text").await,
        names.role
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_cannot_touch_another_plugins_schema() {
    let f = Fixture::new().await;
    let pools = f.pools(2);
    exec(
        &pools,
        "cooldown",
        "CREATE TABLE claim (k TEXT PRIMARY KEY)",
    )
    .await
    .expect("cooldown creates its table");
    // The intruder's own schema exists, so only the privilege boundary is
    // in the way.
    exec(&pools, "intruder", "SELECT 1")
        .await
        .expect("intruder");
    for sql in [
        "SELECT * FROM plugin_cooldown.claim",
        "INSERT INTO plugin_cooldown.claim VALUES ('x')",
        "UPDATE plugin_cooldown.claim SET k = 'y'",
        "DELETE FROM plugin_cooldown.claim",
        "TRUNCATE plugin_cooldown.claim",
        "DROP TABLE plugin_cooldown.claim",
        "ALTER TABLE plugin_cooldown.claim ADD COLUMN v INT",
        "CREATE TABLE plugin_cooldown.planted (v INT)",
        "DROP SCHEMA plugin_cooldown CASCADE",
        "GRANT USAGE ON SCHEMA plugin_cooldown TO PUBLIC",
    ] {
        denied(&pools, "intruder", sql).await;
    }
    // Neither can it become the other plugin's role.
    let other = f.names("cooldown").role;
    denied(&pools, "intruder", &format!("SET ROLE \"{other}\"")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_role_escape_lands_on_the_plugins_own_role() {
    let f = Fixture::new().await;
    // One connection: a role change would persist into the next statement,
    // as it would into the plugin's next call.
    let pools = f.pools(1);
    let names = f.names("print");
    exec(&pools, "print", "RESET ROLE").await.expect("reset");
    assert_eq!(
        text(
            &pools,
            "print",
            "SELECT current_user::text || ',' || session_user::text"
        )
        .await,
        format!("{0},{0}", names.role)
    );
    denied(&pools, "print", "SET ROLE postgres").await;
    // Its group role holds less than the plugin role, so stepping down
    // gains nothing.
    let step_down = format!("SET ROLE \"{}\"", names.group);
    denied(
        &pools,
        "print",
        &format!("{step_down}; CREATE TABLE plugin_print.t (v INT)"),
    )
    .await;
    denied(
        &pools,
        "print",
        &format!("{step_down}; SELECT password FROM plugin_login_secret"),
    )
    .await;
    // A role change ends with the call: the connection returns to the pool
    // as the plugin's own role.
    exec(&pools, "print", &step_down).await.expect("step down");
    let lease = pools.lease("print", &[], None).await.expect("lease");
    let who = lease
        .db
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_user::text",
        ))
        .await
        .expect("query")
        .expect("row")
        .try_get_by_index::<String>(0)
        .expect("text");
    assert_eq!(who, names.role);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_cannot_read_the_plugin_logins() {
    let f = Fixture::new().await;
    let pools = f.pools(1);
    denied(&pools, "print", "SELECT password FROM plugin_login_secret").await;
    denied(&pools, "print", "SELECT * FROM public.plugin_login_secret").await;
}

/// Replicas booting together provision the same plugins at once: every lease
/// succeeds and every replica connects with the one stored password.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_uses_on_several_replicas_converge() {
    let f = Fixture::new().await;
    let replicas: Vec<PluginSqlPools> = (0..4).map(|_| f.pools(4)).collect();
    let mut calls = Vec::new();
    for pools in &replicas {
        for plugin in ["icpc", "ioi", "print"] {
            let pools = pools.clone();
            calls.push(tokio::spawn(async move {
                exec(&pools, plugin, "SELECT 1").await
            }));
        }
    }
    for call in calls {
        call.await
            .expect("join")
            .expect("every concurrent first use succeeds");
    }
    let secrets: i64 = f
        .app
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT COUNT(*)::bigint FROM plugin_login_secret",
        ))
        .await
        .expect("count")
        .expect("row")
        .try_get_by_index(0)
        .expect("count");
    assert_eq!(secrets, 3);
}

/// At most `budget` leases hold a connection at once: a lease past the budget
/// waits until one ends. Transactions pin their connections, so the count is
/// exact while they are open.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_plugin_pools_keep_to_one_budget() {
    let f = Fixture::new().await;
    let pools = f.pools(2);
    let a = pools.lease("a", &[], None).await.expect("a");
    let a_txn = sea_orm::TransactionTrait::begin(&a.db)
        .await
        .expect("a begins");
    let b = pools.lease("b", &[], None).await.expect("b");
    let b_txn = sea_orm::TransactionTrait::begin(&b.db)
        .await
        .expect("b begins");
    assert_eq!(pools.open_connections(), 2);

    let third = pools.lease("c", &[], None);
    tokio::pin!(third);
    assert!(
        (&mut third).now_or_never().is_none(),
        "a third lease must wait while two are held"
    );
    a_txn.rollback().await.expect("a ends");
    drop(a);
    let third = third.await.expect("the third lease proceeds once one ends");
    third
        .db
        .execute_unprepared("SELECT 1")
        .await
        .expect("c runs");
    drop((third, b_txn, b));
}

/// What plugins may do with host relations: read the classified contest data,
/// nothing else. Driven over every relation in schema `public`, so a host
/// table added later is covered without a test change.
mod grants {
    use super::*;

    /// A declared scope grants exactly its columns, to that plugin alone, and
    /// a manifest that stops declaring it loses it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_declared_scope_grants_exactly_its_columns() {
        use plugin_core::manifest::SqlReadScope;
        let f = Fixture::new().await;
        let pools = f.pools(1);
        let files = vec![SqlReadScope::parse("submission.files").unwrap()];
        async fn run(
            pools: &PluginSqlPools,
            read: &[SqlReadScope],
            sql: &str,
        ) -> Result<(), DbErr> {
            let lease = pools.lease("print", read, None).await.expect("lease");
            lease.db.execute_unprepared(sql).await.map(drop)
        }
        run(&pools, &files, "SELECT files FROM submission")
            .await
            .expect("the declared column is readable");
        for sql in [
            "SELECT compile_output FROM submission",
            "SELECT input FROM test_case",
            "SELECT stdout FROM test_case_result",
        ] {
            let err = run(&pools, &files, sql).await.expect_err(sql).to_string();
            assert!(err.contains("permission denied"), "{sql}: {err}");
        }
        denied(&pools, "icpc", "SELECT files FROM submission").await;
        // The next version of the manifest drops the scope.
        let err = run(&pools, &[], "SELECT files FROM submission")
            .await
            .expect_err("a dropped scope is revoked")
            .to_string();
        assert!(err.contains("permission denied"), "{err}");
    }
    use server::migration::{PLUGIN_READABLE, PLUGIN_READABLE_COLUMNS};

    /// Host relations no plugin may read at all: credentials, accounts and
    /// roles, other plugins' host-held state, and host machinery.
    const HIDDEN: &[&str] = &[
        "blob_data",
        "dead_letter_message",
        "device_authorization",
        "idempotency_key",
        "plugin",
        "plugin_config",
        "plugin_group_role",
        "plugin_login_secret",
        "plugin_replica",
        "plugin_storage",
        "plugin_timer",
        "refresh_tokens",
        "role",
        "role_permission",
        "seaql_migrations",
        "seeded_role_permission",
        "submission_feedback",
        "task_cache",
        "user_role",
    ];

    async fn strings(db: &DatabaseConnection, sql: &str) -> Vec<String> {
        db.query_all_raw(Statement::from_string(DbBackend::Postgres, sql))
            .await
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .iter()
            .map(|r| r.try_get_by_index::<String>(0).expect("text"))
            .collect()
    }

    /// The host's relations in `public`, leaving out what an extension owns.
    async fn host_relations(f: &Fixture) -> Vec<String> {
        strings(
            &f.app,
            "SELECT c.relname::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p', 'v', 'm', 'f') \
               AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = c.oid \
                               AND d.deptype = 'e') \
             ORDER BY 1",
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn every_host_relation_is_classified() {
        let f = Fixture::new().await;
        for relation in host_relations(&f).await {
            let classified = PLUGIN_READABLE.contains(&relation.as_str())
                || PLUGIN_READABLE_COLUMNS.iter().any(|(t, _)| *t == relation)
                || HIDDEN.contains(&relation.as_str());
            assert!(
                classified,
                "host relation {relation} is neither plugin-readable nor hidden: classify it \
                 in m0001's PLUGIN_READABLE or in this test's HIDDEN"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_cannot_write_any_host_relation() {
        let f = Fixture::new().await;
        let pools = f.pools(1);
        for relation in host_relations(&f).await {
            let t = format!("public.\"{relation}\"");
            let column = strings(
                &f.app,
                &format!(
                    "SELECT attname::text FROM pg_attribute WHERE attrelid = '{t}'::regclass \
                     AND attnum > 0 AND NOT attisdropped ORDER BY attnum LIMIT 1"
                ),
            )
            .await
            .remove(0);
            let is_view = !strings(
                &f.app,
                &format!("SELECT relname::text FROM pg_class WHERE oid = '{t}'::regclass AND relkind = 'v'"),
            )
            .await
            .is_empty();
            let table_only: Vec<String> = if is_view {
                Vec::new()
            } else {
                vec![
                    format!("TRUNCATE {t}"),
                    format!("DROP TABLE {t}"),
                    format!(
                        "CREATE TRIGGER planted BEFORE INSERT ON {t} FOR EACH ROW EXECUTE FUNCTION suppress_redundant_updates_trigger()"
                    ),
                ]
            };
            for sql in [
                format!("INSERT INTO {t} DEFAULT VALUES"),
                format!("UPDATE {t} SET \"{column}\" = \"{column}\""),
                format!("DELETE FROM {t}"),
                format!("ALTER TABLE {t} ADD COLUMN planted INT"),
            ]
            .into_iter()
            .chain(table_only)
            {
                denied(&pools, "print", &sql).await;
            }
            // No privilege that writes, references or hooks the relation.
            let held = text(
                &pools,
                "print",
                &format!(
                    "SELECT concat_ws(',', \
                       CASE WHEN has_table_privilege('{t}', 'INSERT') THEN 'INSERT' END, \
                       CASE WHEN has_table_privilege('{t}', 'UPDATE') THEN 'UPDATE' END, \
                       CASE WHEN has_table_privilege('{t}', 'DELETE') THEN 'DELETE' END, \
                       CASE WHEN has_table_privilege('{t}', 'TRUNCATE') THEN 'TRUNCATE' END, \
                       CASE WHEN has_table_privilege('{t}', 'REFERENCES') THEN 'REFERENCES' END, \
                       CASE WHEN has_table_privilege('{t}', 'TRIGGER') THEN 'TRIGGER' END, \
                       CASE WHEN has_any_column_privilege('{t}', 'INSERT') THEN 'INSERT(col)' END, \
                       CASE WHEN has_any_column_privilege('{t}', 'UPDATE') THEN 'UPDATE(col)' END, \
                       CASE WHEN has_any_column_privilege('{t}', 'REFERENCES') THEN 'REFERENCES(col)' END)"
                ),
            )
            .await;
            assert_eq!(held, "", "{relation}: the plugin holds {held}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_cannot_use_host_sequences_or_create_in_public() {
        let f = Fixture::new().await;
        let pools = f.pools(1);
        let sequences = strings(
            &f.app,
            "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind = 'S' ORDER BY 1",
        )
        .await;
        assert!(!sequences.is_empty());
        for sequence in sequences {
            for sql in [
                format!("SELECT nextval('public.\"{sequence}\"')"),
                format!("SELECT setval('public.\"{sequence}\"', 1)"),
                format!("SELECT last_value FROM public.\"{sequence}\""),
            ] {
                denied(&pools, "print", &sql).await;
            }
        }
        for sql in [
            "CREATE TABLE public.planted (v INT)",
            "CREATE VIEW public.planted AS SELECT 1 AS v",
            "CREATE FUNCTION public.planted() RETURNS int LANGUAGE sql AS 'SELECT 1'",
            "CREATE SCHEMA planted",
        ] {
            denied(&pools, "print", sql).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_plugin_reads_contest_data_but_no_secret() {
        let f = Fixture::new().await;
        let pools = f.pools(1);
        for relation in PLUGIN_READABLE {
            let content = server::plugin_db::schema::content_columns(relation);
            let columns = strings(
                &f.app,
                &format!(
                    "SELECT attname::text FROM pg_attribute \
                     WHERE attrelid = 'public.\"{relation}\"'::regclass AND attnum > 0 \
                       AND NOT attisdropped ORDER BY attnum"
                ),
            )
            .await;
            let plain = columns
                .iter()
                .filter(|c| !content.contains(&c.as_str()))
                .map(|c| format!("\"{c}\""))
                .collect::<Vec<_>>()
                .join(", ");
            exec(
                &pools,
                "icpc",
                &format!("SELECT {plain} FROM public.\"{relation}\" LIMIT 1"),
            )
            .await
            .unwrap_or_else(|e| panic!("{relation} must be readable: {e}"));
            for column in &content {
                denied(
                    &pools,
                    "icpc",
                    &format!("SELECT \"{column}\" FROM public.\"{relation}\""),
                )
                .await;
            }
            if !content.is_empty() {
                denied(
                    &pools,
                    "icpc",
                    &format!("SELECT * FROM public.\"{relation}\""),
                )
                .await;
            }
        }
        exec(
            &pools,
            "icpc",
            "SELECT u.id, u.username, u.display_name, u.created_at, u.deleted_at \
             FROM \"user\" u LIMIT 1",
        )
        .await
        .expect("an account's public profile is readable");
        for sql in [
            "SELECT password FROM \"user\"",
            "SELECT credentials_changed_at FROM \"user\"",
            "SELECT * FROM \"user\"",
        ] {
            denied(&pools, "icpc", sql).await;
        }
        for relation in host_relations(&f).await {
            if HIDDEN.contains(&relation.as_str()) {
                denied(
                    &pools,
                    "icpc",
                    &format!("SELECT 1 FROM public.\"{relation}\" LIMIT 1"),
                )
                .await;
            }
        }
    }
}

/// A call that holds a permit reuses it: at a full budget, a transaction's call
/// runs further statements, and its nested call opens a transaction of its
/// own, while a different call waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_call_reuses_its_permit_at_a_full_budget() {
    use plugin_core::executor::CallLocals;
    use sea_orm::TransactionTrait;
    let f = Fixture::new().await;
    let pools = f.pools(1);
    let call = std::sync::Arc::new(CallLocals::default());

    let lease = pools
        .lease("icpc", &[], Some(&call))
        .await
        .expect("first lease");
    let txn = lease
        .db
        .begin()
        .await
        .expect("the call opens a transaction");
    let held = lease.permit.clone();
    drop(lease);

    // A lease that needed a second permit would wait out the acquire
    // timeout and fail; this one reuses the call's.
    let statement = pools.lease("icpc", &[], Some(&call)).await.expect("lease");
    assert!(std::sync::Arc::ptr_eq(&statement.permit, &held));
    statement
        .db
        .execute_unprepared("SELECT 1")
        .await
        .expect("statement beside the transaction");
    drop(statement);
    // A nested call shares its parent's call locals.
    let nested = pools.lease("ioi", &[], Some(&call)).await.expect("lease");
    assert!(std::sync::Arc::ptr_eq(&nested.permit, &held));
    let nested_txn = nested.db.begin().await.expect("nested transaction");

    let other = std::sync::Arc::new(CallLocals::default());
    let waiting = pools.lease("icpc", &[], Some(&other));
    tokio::pin!(waiting);
    assert!(
        (&mut waiting).now_or_never().is_none(),
        "another call waits while the budget is held"
    );
    nested_txn.rollback().await.expect("nested ends");
    drop(nested);
    txn.rollback().await.expect("transaction ends");
    drop(held);
    waiting
        .await
        .expect("the other call proceeds once the permit is free");
}

/// A plugin whose role cannot be provisioned is unavailable, and is not
/// retried on every call: the next call fails at once, even after the cause
/// is gone, until the back-off passes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_provisioning_backs_off() {
    use server::plugin_db::Unavailable;
    let f = Fixture::new().await;
    let pools = f.pools(1);
    let group = f.names("print").group;
    f.app
        .execute_unprepared("DELETE FROM plugin_group_role")
        .await
        .expect("hide the group");
    let first = pools
        .lease("print", &[], None)
        .await
        .err()
        .expect("unprovisionable");
    assert!(
        matches!(&first, Unavailable::Unprovisioned(msg) if msg.contains("no plugin group role")),
        "{first}"
    );
    f.app
        .execute_unprepared(&format!("INSERT INTO plugin_group_role VALUES ('{group}')"))
        .await
        .expect("restore the group");
    let again = pools
        .lease("print", &[], None)
        .await
        .err()
        .expect("still backing off");
    assert!(
        matches!(&again, Unavailable::Unprovisioned(msg) if msg.contains("failed recently")),
        "{again}"
    );
    // Another server, with no failure behind it, provisions the plugin.
    exec(&f.pools(1), "print", "SELECT 1")
        .await
        .expect("provisioned once the cause is gone");
}

/// Session state a plugin statement leaves does not outlive it on the pooled
/// connection. The host waits on the plugin's advisory lock, which it gets
/// only once the connection returning to the pool released it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_returned_connection_drops_its_session_state() {
    let f = Fixture::new().await;
    let pools = f.pools(1);
    exec(
        &pools,
        "print",
        "SELECT pg_advisory_lock(42); LISTEN plugin_channel; SET search_path = public",
    )
    .await
    .expect("session state");
    tokio::time::timeout(
        Duration::from_secs(30),
        f.app.execute_unprepared("SELECT pg_advisory_lock(42)"),
    )
    .await
    .expect("the plugin's lock is released when its connection returns")
    .expect("lock");
    f.app
        .execute_unprepared("SELECT pg_advisory_unlock(42)")
        .await
        .expect("unlock");
    // The search_path is the role's again: unqualified DDL lands in the
    // plugin's schema.
    exec(&pools, "print", "CREATE TABLE after_reset (v INT)")
        .await
        .expect("the reset search_path names the plugin's schema first");
}

/// A plugin removed from the fleet can no longer log in. Installing the same
/// id again is the same plugin: its role logs in again and finds its data.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_removed_plugin_cannot_log_in_until_it_is_installed_again() {
    let f = Fixture::new().await;
    let pools = f.pools(1);
    exec(
        &pools,
        "print",
        "CREATE TABLE job (v INT); INSERT INTO job VALUES (7)",
    )
    .await
    .expect("the plugin keeps data");
    let role = f.names("print").role;
    let can_login = || async {
        f.app
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT rolcanlogin FROM pg_roles WHERE rolname = '{role}'"),
            ))
            .await
            .expect("query")
            .expect("the role stays")
            .try_get_by_index::<bool>(0)
            .expect("bool")
    };

    pools.forget("print");
    server::plugin_db::schema::retire(&f.app, "print")
        .await
        .expect("retire");
    assert!(!can_login().await, "a removed plugin's role cannot log in");

    assert_eq!(
        text(&pools, "print", "SELECT v::text FROM job").await,
        "7",
        "the reinstalled plugin logs in and finds its data"
    );
    assert!(can_login().await);
}
