use common::db::DatabaseConnection;
use common::db::{PgPoolSpec, PgSession};
use sea_orm::{ConnectionTrait, DbErr};
use sea_orm_migration::MigratorTrait;

use crate::migration::Migrator;

/// `application_name` of the server's own pools (`pg_stat_activity`).
pub const APP_NAME: &str = "broccoli-server";
/// `application_name` of the plugin host-function pools, so plugin SQL is
/// distinguishable from the server's own in `pg_stat_activity`.
pub const PLUGIN_APP_NAME: &str = "broccoli-server-plugin";

/// The session every pooled server connection runs with.
pub fn app_session(config: &crate::config::DatabaseConfig) -> PgSession {
    PgSession::new(
        APP_NAME,
        config.statement_timeout_secs,
        config.tcp_keepalives,
    )
}

/// The same bounds as [`app_session`], labelled as plugin traffic.
pub fn plugin_session(app: &PgSession) -> PgSession {
    PgSession {
        application_name: PLUGIN_APP_NAME,
        ..app.clone()
    }
}

/// Migrated main pool with default settings (tests and tools).
pub async fn init_db(db_url: &str) -> Result<DatabaseConnection, DbErr> {
    init_db_with_max_connections(
        db_url,
        100,
        &app_session(&crate::config::DatabaseConfig::default()),
    )
    .await
}

/// The host function pool: the pool every database host function other than
/// raw `sql` runs on (`host.submission.*`, `host.code_run.*`, `host.storage.*`,
/// `host.timer.*`, `config:*`), along with the host services those functions
/// start. Every statement on it is built by the host, with the guards those
/// functions carry, so it runs as the app role. It is separate from the main
/// pool so plugin traffic can neither exhaust the handlers' connections nor
/// wait behind them.
pub async fn connect_host_fn_pool(
    db_url: &str,
    spec: &PgPoolSpec,
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    common::db::connect(db_url, spec, &plugin_session(session)).await
}

/// The shape of a plugin pool of `max_connections`: a plugin call waits for a
/// connection as long as `call_timeout`, the longest it may run (see
/// [`PoolSizes`]).
pub fn plugin_pool_spec(max_connections: u32, call_timeout: std::time::Duration) -> PgPoolSpec {
    PgPoolSpec {
        acquire_timeout: call_timeout,
        ..PgPoolSpec::new(max_connections, 2)
    }
}

/// Bring the schema up to date (under the boot lock), then open the main pool.
/// For tests and tools; the server binary drives [`BootSession`] itself so
/// seeding runs under the same lock.
pub async fn init_db_with_max_connections(
    db_url: &str,
    max_connections: u32,
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    let boot = BootSession::acquire(db_url).await?;
    boot.migrate().await?;
    boot.release().await?;
    connect_main_pool(db_url, max_connections, session).await
}

/// How many connections each of a server's three pools may hold: what this
/// host's CPUs and the plugin executor's CPU tokens want (64 at most
/// together), fitted at boot to this server's share of the database's
/// `max_connections`.
///
/// The plugin pools follow admission
/// ([`plugin_core::admission::Capacity`]). Plugin host functions are parked:
/// a call waiting for a connection or for Postgres holds no CPU token, so more
/// calls can be in the database at once than there are tokens, up to the
/// admitted-call caps, and those run to hundreds. No database has a connection
/// for each, so both plugin pools are sized from the tokens, by one rule: one
/// host function connection per token and one raw plugin SQL permit per two.
/// The host function pool carries the judging path (every
/// `host.submission.*` and `host.code_run.*` write, the evaluate-batch reads)
/// and plugin provisioning; raw plugin SQL (scoreboards, submission gates)
/// runs far less per judged submission. Every admitted call is still served:
/// a call that finds the pool busy waits, parked, as long as
/// `plugin.call_timeout_secs` (see [`plugin_pool_spec`]), so the pool never
/// fails a call that its own deadline would have let finish.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSizes {
    /// Handlers, the dispatcher and background loops.
    pub main: u32,
    /// Raw plugin SQL: the permits all plugin pools share (see
    /// [`crate::plugin_db`]).
    pub plugin: u32,
    /// The host functions' own SQL (the host function pool).
    pub host_fn: u32,
}

/// Connections a server holds outside its pools: the claim listener and the
/// clock's sampling connection.
pub const SERVER_UNPOOLED_CONNECTIONS: u32 = 2;

/// The smallest pools a server still works with: below these, handlers queue
/// behind the dispatcher.
const POOL_FLOOR: PoolSizes = PoolSizes {
    main: 8,
    plugin: 1,
    host_fn: 2,
};

/// The other nodes sharing the database, as their liveness keys show them
/// when this server boots.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fleet {
    /// Live servers other than this one.
    pub other_servers: u32,
    /// Connections the live workers hold together.
    pub worker_connections: u32,
}

/// Connections the database leaves to this server: what the cluster accepts
/// for ordinary roles, less a reserve for operators and booting nodes and the
/// workers' share, split evenly between this server and the others.
pub fn server_share(usable_connections: u32, fleet: Fleet) -> u32 {
    let reserve = (usable_connections / 10).max(10);
    usable_connections
        .saturating_sub(reserve)
        .saturating_sub(fleet.worker_connections)
        / fleet.other_servers.saturating_add(1)
}

impl PoolSizes {
    /// The pools a host with `cpus` CPUs and `plugin_tokens` plugin CPU
    /// tokens wants.
    pub fn derive(cpus: usize, plugin_tokens: usize) -> Self {
        let clamp = |n: usize, lo: u32, hi: u32| u32::try_from(n).unwrap_or(hi).clamp(lo, hi);
        Self {
            main: clamp(cpus.saturating_mul(4), 16, 40),
            plugin: clamp(plugin_tokens / 2, 2, 8),
            host_fn: clamp(plugin_tokens, 4, 16),
        }
    }

    /// Every connection a server with these pools holds at most.
    pub fn connections(&self) -> u32 {
        self.main + self.plugin + self.host_fn + SERVER_UNPOOLED_CONNECTIONS
    }

    /// These pools, shrunk in proportion where they hold more than `share`
    /// connections, but never below the floor. `None` when even the floor
    /// does not fit.
    pub fn fit(self, share: u32) -> Option<Self> {
        if self.connections() <= share {
            return Some(self);
        }
        if POOL_FLOOR.connections() > share {
            return None;
        }
        // Each pool keeps its floor and gets the room above the floors in
        // proportion to how far above its floor it wanted to be.
        let extra = u64::from(share - POOL_FLOOR.connections());
        let above = u64::from(self.connections() - POOL_FLOOR.connections());
        let grow =
            |wanted: u32, floor: u32| floor + (u64::from(wanted - floor) * extra / above) as u32;
        let fitted = Self {
            main: grow(self.main, POOL_FLOOR.main),
            plugin: grow(self.plugin, POOL_FLOOR.plugin),
            host_fn: grow(self.host_fn, POOL_FLOOR.host_fn),
        };
        Some(fitted)
    }

    /// The pools for this host and `config`, fitted to what `boot` reports
    /// the database accepts and the fleet `mq_url` shows as live.
    pub async fn for_host(
        config: &crate::config::AppConfig,
        boot: &DatabaseConnection,
        server_id: &str,
    ) -> Result<Self, DbErr> {
        let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
        let tokens =
            plugin_core::admission::Capacity::for_config(config.plugin.max_concurrent_calls).tokens;
        let wanted = Self::derive(cpus, tokens);
        let usable = usable_connections(boot).await?;
        let fleet = fleet_at_boot(config, server_id).await;
        let share = server_share(usable, fleet);
        Ok(match wanted.fit(share) {
            Some(fitted) if fitted == wanted => wanted,
            Some(fitted) => {
                tracing::warn!(
                    usable_connections = usable,
                    other_servers = fleet.other_servers,
                    worker_connections = fleet.worker_connections,
                    share,
                    ?wanted,
                    ?fitted,
                    "Database pools shrunk to this server's share of max_connections; \
                     raise max_connections for full-size pools"
                );
                fitted
            }
            None => {
                tracing::warn!(
                    usable_connections = usable,
                    other_servers = fleet.other_servers,
                    worker_connections = fleet.worker_connections,
                    share,
                    needed = POOL_FLOOR.connections(),
                    "The fleet does not fit max_connections: requests will wait for \
                     connections or fail; raise max_connections"
                );
                POOL_FLOOR
            }
        })
    }
}

/// Connections the cluster accepts for roles that are not superusers.
async fn usable_connections(db: &DatabaseConnection) -> Result<u32, DbErr> {
    let row = db
        .query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT (current_setting('max_connections')::int \
               - current_setting('superuser_reserved_connections')::int \
               - coalesce(nullif(current_setting('reserved_connections', true), '')::int, 0) \
             )::bigint AS usable",
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("SHOW max_connections returned no row".into()))?;
    let usable: i64 = row.try_get("", "usable")?;
    Ok(u32::try_from(usable).unwrap_or(0))
}

/// The live fleet in Redis. Without Redis this server is taken to be alone.
async fn fleet_at_boot(config: &crate::config::AppConfig, server_id: &str) -> Fleet {
    use common::liveness::{NodeRole, live_nodes};
    let namespace = &config.mq.operation_queue_name;
    let read = async {
        let client = redis::Client::open(config.mq.url.as_str())?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        let servers = live_nodes(&mut conn, namespace, NodeRole::Server).await?;
        let workers = live_nodes(&mut conn, namespace, NodeRole::Worker).await?;
        redis::RedisResult::Ok((servers, workers))
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), read).await {
        Ok(Ok((servers, workers))) => Fleet {
            other_servers: servers.iter().filter(|s| s.id != server_id).count() as u32,
            worker_connections: workers
                .iter()
                .map(|w| {
                    let slots =
                        serde_json::from_str::<common::worker::HeartbeatPayload>(&w.payload)
                            .ok()
                            .and_then(|p| p.max_concurrency)
                            .unwrap_or(1);
                    common::worker::database_connections(slots)
                })
                .sum(),
        },
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "Reading the live fleet failed; sizing pools as if alone");
            Fleet::default()
        }
        Err(_) => {
            tracing::warn!("Reading the live fleet timed out; sizing pools as if alone");
            Fleet::default()
        }
    }
}

/// The main pool (handlers, dispatcher, background loops). No schema work.
pub async fn connect_main_pool(
    db_url: &str,
    max_connections: u32,
    session: &PgSession,
) -> Result<DatabaseConnection, DbErr> {
    // A statement future dropped mid-flight (client disconnect, the request
    // `TimeoutLayer`, a `select!`) leaves its connection mid-exchange. sqlx
    // pings the connection on release, which flushes the pending bytes and
    // drains to `ReadyForQuery`, and closes it if that surfaces an error, so
    // the pool never hands out a connection that is out of step.
    common::db::connect(db_url, &PgPoolSpec::new(max_connections, 5), session).await
}

/// Session-level advisory lock key that serializes boot-time schema work
/// (migrations, seeding) across every
/// server booting against the same database. The value is arbitrary but fixed
/// ("broccoli" in ASCII); it only has to be unique among this database's
/// advisory locks.
pub const BOOT_LOCK_KEY: i64 = 0x6272_6f63_636f_6c69;

/// One dedicated database session holding [`BOOT_LOCK_KEY`] for the duration
/// of boot-time schema work.
///
/// Servers booting together therefore run migrations and check-then-insert
/// seeding one at a time, instead of racing
/// each other's DDL and inserting duplicate seed rows.
///
/// The session is a single never-recycled connection (so the session-level
/// lock and the work run on the same backend) with no `statement_timeout`
/// (DDL on a large database legitimately runs long). If this
/// process dies while holding the lock, Postgres releases it when the session
/// ends; the server-side keepalive in [`common::db`] bounds that to about a
/// minute even when the host vanishes without closing the socket.
pub struct BootSession {
    db: DatabaseConnection,
}

impl BootSession {
    /// Open the session and take the boot lock, waiting for a peer that is
    /// booting to finish first.
    pub async fn acquire(db_url: &str) -> Result<Self, DbErr> {
        let db = common::db::connect(
            db_url,
            &PgPoolSpec::single_session(),
            &PgSession::unbounded(APP_NAME),
        )
        .await?;
        let boot = Self { db };
        // Override any role- or database-level default too. A plain `SET` (not
        // a startup option) also works through a PgBouncer in session mode.
        boot.db
            .execute_unprepared("SET statement_timeout = 0")
            .await?;
        let held = boot
            .db
            .query_one_raw(boot.lock_stmt("SELECT pg_try_advisory_lock($1) AS held"))
            .await?
            .map(|r| r.try_get::<bool>("", "held"))
            .transpose()?
            .unwrap_or(false);
        if !held {
            tracing::info!(
                lock_key = BOOT_LOCK_KEY,
                "Another server is running boot-time schema work; waiting for it to finish"
            );
            boot.db
                .execute_raw(boot.lock_stmt("SELECT pg_advisory_lock($1)"))
                .await?;
        }
        tracing::info!("Acquired the boot lock");
        Ok(boot)
    }

    fn lock_stmt(&self, sql: &str) -> sea_orm::Statement {
        sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Postgres,
            sql,
            [BOOT_LOCK_KEY.into()],
        )
    }

    /// The locked session, for seeding and provisioning.
    pub fn conn(&self) -> &DatabaseConnection {
        &self.db
    }

    /// Apply this build's pending migrations. Every booting server takes the
    /// boot lock first, so one applies them while the others wait, then find
    /// nothing pending.
    pub async fn migrate(&self) -> Result<(), DbErr> {
        self.migrate_with::<Migrator>().await
    }

    /// [`Self::migrate`] with the migrations `M` defines: a test runs a build
    /// with more (or fewer) migrations than this one.
    pub(crate) async fn migrate_with<M: MigratorTrait>(&self) -> Result<(), DbErr> {
        crate::migration::check_applied_versions::<M>(&self.db).await?;
        M::up(self.db.as_sea_orm(), None).await
    }

    /// Release the lock and close the session. Dropping a `BootSession`
    /// without calling this also releases the lock, once the pool closes.
    pub async fn release(self) -> Result<(), DbErr> {
        self.db
            .execute_raw(self.lock_stmt("SELECT pg_advisory_unlock($1)"))
            .await?;
        self.db.close().await
    }
}

#[cfg(test)]
mod pool_size_tests {
    use super::{Fleet, POOL_FLOOR, PoolSizes, server_share};

    #[test]
    fn pools_grow_with_the_host_and_stay_within_a_fixed_budget() {
        let small = PoolSizes::derive(2, 2);
        assert_eq!((small.main, small.plugin, small.host_fn), (16, 2, 4));
        let large = PoolSizes::derive(128, 256);
        assert_eq!((large.main, large.plugin, large.host_fn), (40, 8, 16));
        assert_eq!(large.connections(), 66);
    }

    #[test]
    fn the_shipped_max_connections_fits_the_documented_fleet() {
        // 500 less the 3 superuser slots; five servers, thirty workers.
        let fleet = Fleet {
            other_servers: 4,
            worker_connections: 30 * common::worker::database_connections(1),
        };
        let share = server_share(497, fleet);
        let large = PoolSizes::derive(128, 256);
        assert_eq!(large.fit(share), Some(large), "share {share}");
    }

    #[test]
    fn pools_shrink_to_the_share_and_keep_the_floor() {
        let wanted = PoolSizes::derive(4, 64);
        assert_eq!(wanted.connections(), 16 + 16 + 8 + 2);
        let fitted = wanted.fit(30).expect("above the floor");
        assert!(fitted.connections() <= 30, "{fitted:?}");
        assert!(
            fitted.main >= POOL_FLOOR.main
                && fitted.plugin >= POOL_FLOOR.plugin
                && fitted.host_fn >= POOL_FLOOR.host_fn
        );
        assert_eq!(wanted.fit(POOL_FLOOR.connections() - 1), None);
        let tight = wanted
            .fit(POOL_FLOOR.connections())
            .expect("exactly the floor");
        assert_eq!(tight.connections(), POOL_FLOOR.connections());
    }

    #[test]
    fn a_four_core_server_alone_on_a_default_postgres_keeps_full_pools() {
        // Stock Postgres: 100 connections, 3 for superusers; four cores give
        // the plugin executor eight tokens.
        let wanted = PoolSizes::derive(4, 8);
        let share = server_share(97, Fleet::default());
        assert_eq!(wanted.fit(share), Some(wanted), "share {share}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_session() -> PgSession {
        app_session(&crate::config::DatabaseConfig::default())
    }

    async fn fresh_postgres() -> (
        testcontainers::ContainerAsync<testcontainers_modules::postgres::Postgres>,
        String,
    ) {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        (container, url)
    }

    #[tokio::test]
    async fn the_usable_connections_leave_out_the_superuser_slots() {
        let (_pg, url) = fresh_postgres().await;
        let boot = BootSession::acquire(&url).await.expect("boot session");
        // Stock Postgres: 100 connections, 3 for superusers.
        assert_eq!(usable_connections(boot.conn()).await.unwrap(), 97);
        boot.release().await.unwrap();
    }

    /// Text holding NUL binds through the server's pool and its transactions
    /// into TEXT and JSONB columns, stored with U+FFFD in place of each NUL,
    /// where Postgres would reject the raw value with 22021 or 22P05.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn nul_text_binds_through_the_pool_and_its_transactions() {
        use sea_orm::{DbBackend, Statement, TransactionTrait};

        let (_c, url) = fresh_postgres().await;
        let db = connect_main_pool(&url, 2, &test_session())
            .await
            .expect("pool");
        db.execute_unprepared("CREATE TABLE nul_probe (t text, j jsonb)")
            .await
            .expect("create");
        let insert = || {
            Statement::from_sql_and_values(
                DbBackend::Postgres,
                "INSERT INTO nul_probe (t, j) VALUES ($1, $2)",
                [
                    "a\u{0}b".into(),
                    serde_json::json!({ "k\u{0}": ["x\u{0}"] }).into(),
                ],
            )
        };
        db.execute_raw(insert()).await.expect("pool insert");
        let txn = db.begin().await.expect("begin");
        txn.execute_raw(insert()).await.expect("transaction insert");
        txn.execute_unprepared("INSERT INTO nul_probe (t) VALUES ('c\u{0}d')")
            .await
            .expect("unprepared insert");
        txn.commit().await.expect("commit");

        let rows = db
            .query_all_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT t, j FROM nul_probe ORDER BY ctid",
            ))
            .await
            .expect("select");
        let stored: Vec<(String, Option<serde_json::Value>)> = rows
            .iter()
            .map(|row| (row.try_get("", "t").unwrap(), row.try_get("", "j").unwrap()))
            .collect();
        let json = serde_json::json!({ "k\u{FFFD}": ["x\u{FFFD}"] });
        assert_eq!(
            stored,
            [
                ("a\u{FFFD}b".to_string(), Some(json.clone())),
                ("a\u{FFFD}b".to_string(), Some(json)),
                ("c\u{FFFD}d".to_string(), None),
            ]
        );
    }

    /// Drive `fut` for at most `polls` polls, yielding between them, then drop
    /// it wherever it stopped.
    async fn abandon_after<F: std::future::Future>(fut: F, polls: u64) {
        let mut fut = std::pin::pin!(fut);
        for _ in 0..polls {
            if futures::poll!(fut.as_mut()).is_ready() {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Statements dropped at arbitrary points (before the write, mid-flush,
    /// mid-response, mid-row-stream, and before a server-side 22021 arrives)
    /// never leave a pooled connection out of step: every statement that runs
    /// to completion afterwards gets exactly its own result, never an earlier
    /// statement's rows or error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cancelled_statements_never_leave_a_pooled_connection_out_of_step() {
        use sea_orm::{DbBackend, Statement};

        let (_c, url) = fresh_postgres().await;
        let db = connect_main_pool(&url, 4, &test_session())
            .await
            .expect("pool");
        let victims = [
            // Fails on the server with 22021 "invalid byte sequence for
            // encoding \"UTF8\": 0x00" after the client has gone.
            "SELECT pg_sleep(0.001), convert_from('\\x610062'::bytea, 'UTF8')",
            "SELECT pg_sleep(0.002), $1::text",
            "SELECT repeat($1, 2000) FROM generate_series(1, 300)",
        ];
        let workers: Vec<_> = (0..16u64)
            .map(|worker| {
                let db = db.clone();
                tokio::spawn(async move {
                    let mut seed = worker.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
                    for i in 0..150u64 {
                        seed ^= seed << 13;
                        seed ^= seed >> 7;
                        seed ^= seed << 17;
                        let sql = victims[(seed % 3) as usize];
                        let values: Vec<sea_orm::Value> = if sql.contains("$1") {
                            vec!["x".into()]
                        } else {
                            vec![]
                        };
                        let victim =
                            Statement::from_sql_and_values(DbBackend::Postgres, sql, values);
                        abandon_after(db.query_all_raw(victim), (seed >> 8) % 16).await;

                        let token = format!("{worker}-{i}");
                        let row = db
                            .query_one_raw(Statement::from_sql_and_values(
                                DbBackend::Postgres,
                                "SELECT $1::text AS v, $2::bigint AS n",
                                [token.clone().into(), (i as i64).into()],
                            ))
                            .await
                            .unwrap_or_else(|e| {
                                panic!("clean statement after a cancelled one failed: {e}")
                            })
                            .expect("one row");
                        assert_eq!(row.try_get::<String>("", "v").unwrap(), token);
                        assert_eq!(row.try_get::<i64>("", "n").unwrap(), i as i64);
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.await.expect("worker");
        }
    }

    /// The server's boot sequence as `ServerRuntime::build` runs it.
    async fn boot_like_the_server(url: &str) -> Result<(), DbErr> {
        let boot = BootSession::acquire(url).await?;
        boot.migrate().await?;
        crate::seed::seed_role_permissions(boot.conn()).await?;
        let bootstrap = crate::config::BootstrapConfig {
            admin_username: "admin".into(),
            admin_password: "pw".into(),
            ..Default::default()
        };
        crate::seed::ensure_bootstrap_admin(boot.conn(), &bootstrap).await?;
        boot.release().await
    }

    /// The boot session is unbounded even when the role carries a default
    /// `statement_timeout` (it sets 0 explicitly instead of relying on startup
    /// options, which a PgBouncer may strip).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_session_overrides_a_role_level_statement_timeout() {
        let (_c, url) = fresh_postgres().await;
        let admin = connect_main_pool(&url, 1, &test_session())
            .await
            .expect("pool");
        admin
            .execute_unprepared("ALTER ROLE postgres SET statement_timeout = '1s'")
            .await
            .expect("role default");
        admin.close().await.expect("close");
        let boot = BootSession::acquire(&url).await.expect("acquire");
        boot.conn()
            .execute_unprepared("SELECT pg_sleep(1.5)")
            .await
            .expect("boot DDL is not cut off by the role default");
        boot.release().await.expect("release");
    }

    /// A second booter blocks on the boot lock until the first releases it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_lock_serializes_booters() {
        let (_c, url) = fresh_postgres().await;
        let first = BootSession::acquire(&url).await.expect("first acquire");
        let second_url = url.clone();
        let mut second = tokio::spawn(async move { BootSession::acquire(&second_url).await });
        assert!(
            tokio::time::timeout(Duration::from_millis(500), &mut second)
                .await
                .is_err(),
            "second booter must wait while the first holds the lock"
        );
        first.release().await.expect("release");
        let second = tokio::time::timeout(Duration::from_secs(10), second)
            .await
            .expect("second booter proceeds once the lock is released")
            .expect("join")
            .expect("second acquire");
        second.release().await.expect("release");
    }

    /// Servers booting together on a fresh database all succeed, and exactly
    /// one bootstrap admin exists, guarded by the unique username index.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn simultaneous_first_boots_converge() {
        let (_c, url) = fresh_postgres().await;
        let boots: Vec<_> = (0..4)
            .map(|_| {
                let url = url.clone();
                tokio::spawn(async move { boot_like_the_server(&url).await })
            })
            .collect();
        for b in boots {
            b.await
                .expect("join")
                .expect("every concurrent boot succeeds");
        }
        let db = connect_main_pool(&url, 1, &test_session())
            .await
            .expect("pool");
        let row = db
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT (SELECT COUNT(*) FROM \"user\" WHERE username = 'admin')::bigint AS admins, \
                 EXISTS (SELECT 1 FROM pg_indexes WHERE indexname = 'idx_user_username_active') AS idx",
            ))
            .await
            .expect("query")
            .expect("row");
        assert_eq!(row.try_get::<i64>("", "admins").unwrap(), 1);
        assert!(row.try_get::<bool>("", "idx").unwrap());
    }

    async fn relation_count(db: &DatabaseConnection) -> i64 {
        db.query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DatabaseBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'public' AND c.relkind IN ('r', 'S')",
        ))
        .await
        .expect("count relations")
        .expect("row")
        .try_get::<i64>("", "n")
        .unwrap()
    }

    /// A build one migration ahead of this one: the baseline plus a migration
    /// that records each time it runs.
    struct NewerBuild;

    struct AddProbe;

    impl sea_orm_migration::MigrationName for AddProbe {
        fn name(&self) -> &str {
            "m0002_upgrade_probe"
        }
    }

    #[async_trait::async_trait]
    impl sea_orm_migration::MigrationTrait for AddProbe {
        async fn up(&self, manager: &sea_orm_migration::SchemaManager) -> Result<(), DbErr> {
            manager
                .get_connection()
                .execute_unprepared(
                    "CREATE TABLE IF NOT EXISTS upgrade_probe (applied_at timestamptz); \
                     INSERT INTO upgrade_probe VALUES (now())",
                )
                .await
                .map(|_| ())
        }
    }

    #[async_trait::async_trait]
    impl MigratorTrait for NewerBuild {
        fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
            let mut migrations = Migrator::migrations();
            migrations.push(Box::new(AddProbe));
            migrations
        }
    }

    /// Servers of a newer build booting together on a database an older build
    /// migrated: exactly one applies the new migration, under the boot lock,
    /// and every boot succeeds. The older build then refuses the database,
    /// naming the version it does not know and its own.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_newer_build_applies_its_pending_migrations_once_across_replicas() {
        let (_c, url) = fresh_postgres().await;
        boot_like_the_server(&url)
            .await
            .expect("the older build boots");

        let boots: Vec<_> = (0..4)
            .map(|_| {
                let url = url.clone();
                tokio::spawn(async move {
                    let boot = BootSession::acquire(&url).await?;
                    boot.migrate_with::<NewerBuild>().await?;
                    boot.release().await
                })
            })
            .collect();
        for b in boots {
            b.await.expect("join").expect("every newer boot succeeds");
        }
        let boot = BootSession::acquire(&url).await.expect("acquire");
        let probe = boot
            .conn()
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                "SELECT (SELECT COUNT(*) FROM upgrade_probe)::bigint AS runs, \
                 (SELECT COUNT(*) FROM seaql_migrations)::bigint AS applied",
            ))
            .await
            .expect("query")
            .expect("row");
        assert_eq!(probe.try_get::<i64>("", "runs").unwrap(), 1);
        assert_eq!(probe.try_get::<i64>("", "applied").unwrap(), 2);

        let relations = relation_count(boot.conn()).await;
        let msg = boot
            .migrate()
            .await
            .expect_err("the older build refuses a database a newer one migrated")
            .to_string();
        assert!(msg.contains("m0002_upgrade_probe"), "{msg}");
        assert!(msg.contains("m0001_baseline"), "{msg}");
        assert!(!msg.contains("drop"), "{msg}");
        assert_eq!(relation_count(boot.conn()).await, relations);
        boot.release().await.expect("release");
    }

    /// A database holding tables but no migration record is not a Broccoli
    /// database (or predates migrations): boot refuses it untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_refuses_a_foreign_database_untouched() {
        let (_c, url) = fresh_postgres().await;
        let boot = BootSession::acquire(&url).await.expect("acquire");
        boot.conn()
            .execute_unprepared("CREATE TABLE someone_elses (id int)")
            .await
            .expect("foreign table");
        let msg = boot
            .migrate()
            .await
            .expect_err("a foreign database must refuse boot")
            .to_string();
        assert!(msg.contains("no Broccoli migration record"), "{msg}");
        assert_eq!(relation_count(boot.conn()).await, 1, "nothing was created");
        boot.release().await.expect("release");
    }

    /// A reboot leaves every index exactly as the first boot built it, the
    /// baseline's partial unique indexes included, so the invariants they
    /// enforce never lapse.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn reboot_keeps_every_index() {
        let (_c, url) = fresh_postgres().await;
        async fn indexes(url: &str) -> Vec<(String, i64, String)> {
            let db = connect_main_pool(url, 1, &test_session())
                .await
                .expect("pool");
            let rows = db
                .query_all_raw(sea_orm::Statement::from_string(
                    sea_orm::DatabaseBackend::Postgres,
                    "SELECT indexname::text AS name, \
                     (quote_ident(schemaname) || '.' || quote_ident(indexname))::regclass::oid::bigint AS oid, \
                     indexdef FROM pg_indexes WHERE schemaname = 'public' ORDER BY indexname",
                ))
                .await
                .expect("list indexes");
            db.close().await.expect("close");
            rows.into_iter()
                .map(|r| {
                    (
                        r.try_get::<String>("", "name").unwrap(),
                        r.try_get::<i64>("", "oid").unwrap(),
                        r.try_get::<String>("", "indexdef").unwrap(),
                    )
                })
                .collect()
        }
        boot_like_the_server(&url).await.expect("first boot");
        let first = indexes(&url).await;
        for partial in [
            "idx_user_username_active",
            "idx_submission_judgement_one_current",
        ] {
            assert!(
                first
                    .iter()
                    .any(|(name, _, def)| name == partial && def.contains(" WHERE ")),
                "{partial} missing after the first boot: {first:?}"
            );
        }
        boot_like_the_server(&url).await.expect("second boot");
        assert_eq!(indexes(&url).await, first);
    }

    /// Every pooled connection carries the shared session settings: a slow
    /// statement is cancelled server-side by `statement_timeout`, and keepalive +
    /// `application_name` are set, on the main AND plugin pools. The migration
    /// session is unbounded, so long DDL is not cut off.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pooled_sessions_are_bounded_and_labelled() {
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::postgres::Postgres;

        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");

        let session = app_session(&crate::config::DatabaseConfig {
            statement_timeout_secs: 1,
            ..Default::default()
        });
        let app = init_db_with_max_connections(&url, 2, &session)
            .await
            .expect("migrate + main pool");
        let host_fn = connect_host_fn_pool(&url, &PgPoolSpec::new(1, 2), &session)
            .await
            .expect("host function pool");
        let plugin_sql = crate::plugin_db::PluginSqlPools::new(
            host_fn.clone(),
            &url,
            1,
            Duration::from_secs(10),
            &session,
        );
        let plugin = plugin_sql
            .lease("labelled", &[], None)
            .await
            .expect("plugin pool")
            .db;

        async fn show(db: &DatabaseConnection, setting: &str) -> String {
            db.query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Postgres,
                format!("SELECT current_setting('{setting}') AS v"),
            ))
            .await
            .expect("show")
            .expect("row")
            .try_get::<String>("", "v")
            .expect("value")
        }

        for (db, name) in [
            (&app, APP_NAME),
            (&host_fn, PLUGIN_APP_NAME),
            (&plugin, PLUGIN_APP_NAME),
        ] {
            assert_eq!(show(db, "statement_timeout").await, "1s");
            assert_eq!(show(db, "application_name").await, name);
            // Keepalives only apply to TCP sessions, which these are.
            assert_eq!(show(db, "tcp_keepalives_idle").await, "30");
            let err = db
                .execute_unprepared("SELECT pg_sleep(3)")
                .await
                .expect_err("statement_timeout must cancel the sleep");
            assert!(
                err.to_string().contains("statement timeout"),
                "unexpected error: {err}"
            );
        }
    }
}
