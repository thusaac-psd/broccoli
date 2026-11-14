//! Plugin SQL privileges, enforced by Postgres. Each test boots a fresh
//! database through the real migration and drives the real per-plugin pools,
//! so what a statement may do is decided by the engine, not by the SQL guard.

use std::time::Duration;

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};
use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;

use super::PluginSqlPools;
use super::schema::PluginDbNames;

pub(super) struct Fixture {
    _container: ContainerAsync<Postgres>,
    pub url: String,
    pub app: DatabaseConnection,
}

impl Fixture {
    pub async fn new() -> Self {
        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let app = crate::database::init_db(&url).await.expect("migrate");
        Self {
            _container: container,
            url,
            app,
        }
    }

    /// One server's plugin pools, `budget` connections together.
    pub fn pools(&self, budget: u32) -> PluginSqlPools {
        PluginSqlPools::new(
            self.app.clone(),
            &self.url,
            budget,
            Duration::from_secs(20),
            &crate::database::app_session(&crate::config::DatabaseConfig::default()),
        )
    }
}

/// Run `sql` as `plugin`.
pub(super) async fn exec(pools: &PluginSqlPools, plugin: &str, sql: &str) -> Result<(), DbErr> {
    let lease = pools.lease(plugin).await?;
    lease.db.execute_unprepared(sql).await.map(drop)
}

/// The single text value `sql` returns, run as `plugin`.
pub(super) async fn text(pools: &PluginSqlPools, plugin: &str, sql: &str) -> String {
    let lease = pools.lease(plugin).await.expect("lease");
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
pub(super) async fn denied(pools: &PluginSqlPools, plugin: &str, sql: &str) {
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
    let names = PluginDbNames::new("postgres", "print");
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
    let other = PluginDbNames::new("postgres", "cooldown").role;
    denied(&pools, "intruder", &format!("SET ROLE \"{other}\"")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_role_escape_lands_on_the_plugins_own_role() {
    let f = Fixture::new().await;
    // One connection: a role change would persist into the next statement,
    // as it would into the plugin's next call.
    let pools = f.pools(1);
    let names = PluginDbNames::new("postgres", "print");
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
    exec(&pools, "print", "SET ROLE broccoli_plugin")
        .await
        .expect("a member may step down to its group role");
    denied(&pools, "print", "CREATE TABLE plugin_print.t (v INT)").await;
    denied(&pools, "print", "SELECT password FROM plugin_login_secret").await;
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
                exec(&pools, plugin, "CREATE TABLE IF NOT EXISTS t (v INT)").await
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

/// Waits until every connection the pools hold is back idle: sqlx returns a
/// released connection to its pool from a task of its own.
async fn settle(pools: &PluginSqlPools) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while pools
            .inner
            .built()
            .iter()
            .any(|(_, db)| super::sqlx_pool(db).size() as usize != super::sqlx_pool(db).num_idle())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("released connections return to their pools");
}

/// Plugins taking turns never hold more connections together than the
/// budget: each first closes another plugin's idle connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_plugin_pools_keep_to_one_budget() {
    let f = Fixture::new().await;
    let pools = f.pools(2);
    for round in 0..3 {
        for plugin in ["a", "b", "c", "d"] {
            exec(&pools, plugin, "SELECT 1")
                .await
                .unwrap_or_else(|e| panic!("{plugin} round {round}: {e}"));
            settle(&pools).await;
            assert!(
                pools.inner.open() <= 2,
                "{} connections open after {plugin}",
                pools.inner.open()
            );
        }
    }
    // Leases beyond the budget wait for one to end rather than open more.
    let first = pools.lease("a").await.expect("first");
    let second = pools.lease("b").await.expect("second");
    let third = pools.lease("c");
    tokio::pin!(third);
    assert!(
        tokio::time::timeout(Duration::from_millis(300), &mut third)
            .await
            .is_err(),
        "a third lease must wait while two are held"
    );
    drop(first);
    let third = tokio::time::timeout(Duration::from_secs(10), third)
        .await
        .expect("the third lease proceeds once one ends")
        .expect("third");
    drop((second, third));
}
