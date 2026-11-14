//! Shared harness for the server's `tests/integration` and `tests/e2e` trees.
//!
//! Cargo builds each test tree as its own binary, which is why the two
//! harnesses were forked; this dev-dependency is the one place their shared
//! infrastructure lives: the per-process Postgres and Redis containers, the
//! migrated and seeded `template_test` database every test database is cloned
//! from, the admin connection that clones it, the database clock tests
//! advance ([`DbClock`]), pool teardown, and the HTTP response wrapper. Each tree keeps its own app fixture (`TestApp`,
//! `E2eTestApp`) and re-exports what it needs from here.

use common::db::DatabaseConnection;
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::Duration;

use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
use serde_json::Value;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::sync::{Mutex, OnceCell};

static SHARED_PG: OnceCell<(ContainerAsync<Postgres>, u16)> = OnceCell::const_new();
static SHARED_REDIS: OnceCell<(ContainerAsync<Redis>, u16)> = OnceCell::const_new();
static CREATE_DB_LOCK: Mutex<()> = Mutex::const_new(());

/// Dedicated multi-thread runtime that owns the shared admin DB connection.
///
/// `#[tokio::test]` spins up a fresh current-thread runtime per test and tears
/// it down at exit; a sqlx pool stored in a `OnceCell` would be tied to the
/// first test's runtime and become invalid for the rest. Housing the pool
/// inside a long-lived dedicated runtime - and dispatching `CREATE DATABASE`
/// onto it - keeps a single admin connection alive across every test, instead
/// of churning a new pool per test (which exhausts PostgreSQL backend slots).
static ADMIN_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static SHARED_ADMIN_CONN: OnceLock<DatabaseConnection> = OnceLock::new();

// ---------------------------------------------------------------------------
// Container lifetime
// ---------------------------------------------------------------------------

/// Ids of the containers this process started, removed at process exit:
/// `testcontainers` drops never run for the process-wide statics above.
static CONTAINER_IDS: StdMutex<Vec<String>> = StdMutex::new(Vec::new());

extern "C" fn cleanup_containers() {
    let ids = CONTAINER_IDS
        .lock()
        .map(|ids| ids.clone())
        .unwrap_or_default();
    for id in ids {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", "-v", &id])
            .output();
    }
}

fn remove_at_exit(container_id: &str) {
    static ATEXIT_REGISTERED: OnceLock<()> = OnceLock::new();
    if let Ok(mut ids) = CONTAINER_IDS.lock() {
        ids.push(container_id.to_string());
    }
    ATEXIT_REGISTERED.get_or_init(|| unsafe {
        libc::atexit(cleanup_containers);
    });
}

// ---------------------------------------------------------------------------
// Postgres
// ---------------------------------------------------------------------------

/// Host port of this process's shared Postgres container, started on first use
/// with a migrated and seeded `template_test` database.
pub async fn shared_pg_port() -> u16 {
    let (_, port) = SHARED_PG
        .get_or_init(|| async {
            let container = Postgres::default()
                .with_tag("17-alpine")
                // pg_stat_statements lets a test count the SQL a flow runs.
                .with_cmd([
                    "postgres",
                    "-c",
                    "max_connections=500",
                    "-c",
                    "shared_preload_libraries=pg_stat_statements",
                ])
                .start()
                .await
                .expect("Failed to start PostgreSQL container");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("Failed to get PostgreSQL port");

            let admin_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
            let mut template_created = false;
            for attempt in 0..15u32 {
                if attempt > 0 {
                    tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
                }
                let Ok(admin_db) = Database::connect(ConnectOptions::new(&admin_url)).await else {
                    continue;
                };
                if admin_db
                    .execute_raw(Statement::from_string(
                        DbBackend::Postgres,
                        "CREATE DATABASE \"template_test\"".to_string(),
                    ))
                    .await
                    .is_ok()
                {
                    drop(admin_db);
                    template_created = true;
                    break;
                }
                drop(admin_db);
            }
            assert!(
                template_created,
                "Failed to create template_test database after 15 attempts"
            );

            remove_at_exit(container.id());

            let template_url =
                format!("postgres://postgres:postgres@127.0.0.1:{port}/template_test");
            let template_db = server::database::init_db(&template_url)
                .await
                .expect("Failed to initialize template database");
            server::seed::seed_role_permissions(&template_db)
                .await
                .expect("Failed to seed template database");
            template_db
                .execute_unprepared(INSTALL_CLOCK)
                .await
                .expect("Failed to install the test clock");
            drop(template_db);

            (container, port)
        })
        .await;
    *port
}

fn admin_runtime() -> &'static tokio::runtime::Handle {
    ADMIN_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name("test-support-admin")
                .build()
                .expect("Failed to build admin runtime")
        })
        .handle()
}

/// The process's admin connection to the shared cluster, opened on first use
/// on the admin runtime. Call only from a task on that runtime.
async fn admin(port: u16) -> &'static DatabaseConnection {
    if SHARED_ADMIN_CONN.get().is_none() {
        let admin_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let mut admin_opts = ConnectOptions::new(&admin_url);
        admin_opts
            .max_connections(1)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(60))
            .idle_timeout(Duration::from_secs(600));
        let conn = Database::connect(admin_opts)
            .await
            .expect("Failed to initialize shared admin database pool");
        let _ = SHARED_ADMIN_CONN.set(conn.into());
    }
    SHARED_ADMIN_CONN
        .get()
        .expect("shared admin connection must be initialized")
}

async fn clone_template(port: u16, db_name: &str) {
    let handle = admin_runtime().clone();
    let db_name = db_name.to_string();

    handle
        .spawn(async move {
            let admin = admin(port).await;
            admin
                .execute_raw(Statement::from_string(
                    DbBackend::Postgres,
                    format!("CREATE DATABASE \"{db_name}\" TEMPLATE template_test"),
                ))
                .await
                .expect("Failed to create test database from template");
            // A database-level setting, so every session on the clone (the
            // server's pools and the plugin logins') resolves the clock.
            admin
                .execute_raw(Statement::from_string(
                    DbBackend::Postgres,
                    format!("ALTER DATABASE \"{db_name}\" SET search_path = {CLOCK_SEARCH_PATH}"),
                ))
                .await
                .expect("Failed to point the test database at its clock");
        })
        .await
        .expect("admin runtime task panicked");
}

/// The role an app-owned test database belongs to: `LOGIN CREATEROLE` and
/// nothing more, the rights a production app role has.
pub const APP_ROLE: &str = "broccoli_test_app";
const APP_ROLE_PASSWORD: &str = "broccoli_test_app";

/// An empty database owned by [`APP_ROLE`], whose URL connects as that role.
/// Nothing is migrated: the test boots it as the app role, as a server would.
pub async fn create_app_owned_database(db_name: &str) -> TestDatabase {
    let port = shared_pg_port().await;
    {
        let _lock = CREATE_DB_LOCK.lock().await;
        let name = db_name.to_string();
        admin_runtime()
            .clone()
            .spawn(async move {
                let admin = admin(port).await;
                for sql in [
                    format!(
                        "DO $$ BEGIN \
                           CREATE ROLE {APP_ROLE} LOGIN CREATEROLE PASSWORD '{APP_ROLE_PASSWORD}'; \
                         EXCEPTION WHEN duplicate_object THEN NULL; END $$"
                    ),
                    format!("CREATE DATABASE \"{name}\" OWNER {APP_ROLE}"),
                ] {
                    admin
                        .execute_unprepared(&sql)
                        .await
                        .unwrap_or_else(|e| panic!("{sql}: {e}"));
                }
            })
            .await
            .expect("admin runtime task panicked");
    }
    TestDatabase(Arc::new(TestDatabaseInner {
        name: db_name.to_string(),
        url: format!("postgres://{APP_ROLE}:{APP_ROLE_PASSWORD}@127.0.0.1:{port}/{db_name}"),
    }))
}

/// One test's database, cloned from `template_test` and dropped when the last
/// clone of this handle goes, so a process running the whole suite holds only
/// the databases of the tests still running. A clone is the handle another
/// app instance (a replica) joins the same database with.
#[derive(Clone, Debug)]
pub struct TestDatabase(Arc<TestDatabaseInner>);

#[derive(Debug)]
struct TestDatabaseInner {
    name: String,
    url: String,
}

impl TestDatabase {
    pub fn name(&self) -> &str {
        &self.0.name
    }

    pub fn url(&self) -> &str {
        &self.0.url
    }
}

impl Drop for TestDatabaseInner {
    fn drop(&mut self) {
        let Some(admin) = SHARED_ADMIN_CONN.get() else {
            return;
        };
        // On the admin runtime, which outlives every test's own: the test's
        // runtime is already shutting down here. FORCE ends the sessions of
        // pools still closing.
        let sql = format!("DROP DATABASE IF EXISTS \"{}\" WITH (FORCE)", self.name);
        admin_runtime().spawn(async move {
            let _ = admin.execute_unprepared(&sql).await;
        });
    }
}

/// Clone database `db_name` from `template_test`. Creation is serialized:
/// Postgres refuses concurrent clones of one template.
pub async fn create_test_database(db_name: &str) -> TestDatabase {
    let port = shared_pg_port().await;
    {
        let _lock = CREATE_DB_LOCK.lock().await;
        clone_template(port, db_name).await;
    }
    TestDatabase(Arc::new(TestDatabaseInner {
        name: db_name.to_string(),
        url: format!("postgres://postgres:postgres@127.0.0.1:{port}/{db_name}"),
    }))
}

/// Close a test's pool off the current runtime. A test fixture is dropped at
/// the end of a `#[tokio::test]`, whose runtime is already shutting down, so
/// the close runs on a throwaway runtime of its own.
pub fn close_database_pool(db: DatabaseConnection) {
    let _ = std::thread::Builder::new()
        .name("test-db-close".to_string())
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            let _ = runtime.block_on(db.close());
        });
}

// ---------------------------------------------------------------------------
// Plugin builds
// ---------------------------------------------------------------------------

/// Panics unless the plugin in `plugin_dir` has its built WASM module. Plugin
/// and fixture WASM is git-ignored build output, so a test that loads a plugin
/// first checks it was built, instead of running without it.
pub fn require_built_plugin(plugin_dir: &std::path::Path) {
    let manifest = plugin_dir.join("plugin.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    let parsed: toml::Table =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", manifest.display()));
    let Some(entry) = parsed
        .get("server")
        .and_then(|server| server.get("entry"))
        .and_then(|entry| entry.as_str())
    else {
        return;
    };
    let wasm = plugin_dir.join(entry);
    assert!(
        wasm.is_file(),
        "{} is missing. Plugin WASM is build output: run `just build-plugins` \
         (or ./scripts/build-plugins.sh) before the server tests",
        wasm.display()
    );
}

/// [`require_built_plugin`] for every plugin directly under `plugins_dir`,
/// which is returned for use as the server's plugins directory.
pub fn require_built_plugins(plugins_dir: std::path::PathBuf) -> std::path::PathBuf {
    let entries = std::fs::read_dir(&plugins_dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", plugins_dir.display()));
    for entry in entries {
        let dir = entry.expect("plugins dir entry").path();
        if dir.join("plugin.toml").is_file() {
            require_built_plugin(&dir);
        }
    }
    plugins_dir
}

// ---------------------------------------------------------------------------
// Database clock
// ---------------------------------------------------------------------------

/// `search_path` of every test database. `public` stays first, so unqualified
/// DDL still creates tables there. `broccoli_test_clock` comes before an
/// explicitly placed `pg_catalog`, so an unqualified `now()` (or one of its
/// siblings) resolves to the shadowing function in [`INSTALL_CLOCK`].
const CLOCK_SEARCH_PATH: &str = "public, broccoli_test_clock, pg_catalog";

/// Installs the clock into the template: real time plus an offset the test
/// owns. An offset rather than a pinned instant keeps time moving, so rows
/// written one after another still order by their timestamps.
const INSTALL_CLOCK: &str = r#"
CREATE SCHEMA broccoli_test_clock;
CREATE TABLE broccoli_test_clock.state (offset_us bigint NOT NULL);
INSERT INTO broccoli_test_clock.state VALUES (0);
CREATE FUNCTION broccoli_test_clock.shift() RETURNS interval LANGUAGE sql STABLE
  AS $$ SELECT make_interval(secs => offset_us / 1000000.0) FROM broccoli_test_clock.state $$;
CREATE FUNCTION broccoli_test_clock.now() RETURNS timestamptz LANGUAGE sql STABLE
  AS $$ SELECT pg_catalog.now() + broccoli_test_clock.shift() $$;
CREATE FUNCTION broccoli_test_clock.transaction_timestamp() RETURNS timestamptz LANGUAGE sql STABLE
  AS $$ SELECT pg_catalog.transaction_timestamp() + broccoli_test_clock.shift() $$;
CREATE FUNCTION broccoli_test_clock.statement_timestamp() RETURNS timestamptz LANGUAGE sql STABLE
  AS $$ SELECT pg_catalog.statement_timestamp() + broccoli_test_clock.shift() $$;
CREATE FUNCTION broccoli_test_clock.clock_timestamp() RETURNS timestamptz LANGUAGE sql VOLATILE
  AS $$ SELECT pg_catalog.clock_timestamp() + broccoli_test_clock.shift() $$;
GRANT USAGE ON SCHEMA broccoli_test_clock TO PUBLIC;
GRANT SELECT ON broccoli_test_clock.state TO PUBLIC;
"#;

/// The clock of one test database, which a test moves forward instead of
/// sleeping.
///
/// In a test database every unqualified `now()`, `transaction_timestamp()`,
/// `statement_timestamp()` and `clock_timestamp()` reads real time plus this
/// clock's offset. Servers keep wall time by the database clock
/// (`server::clock::PlatformClock`), so [`DbClock::advance`] moves SQL time
/// and, through [`DbClock::following`], the handlers' time with it: a
/// contest starts or ends for SQL and for every replica at the same instant.
///
/// What the clock does not move: column defaults written as
/// `CURRENT_TIMESTAMP`.
#[derive(Clone)]
pub struct DbClock {
    db: DatabaseConnection,
    servers: Vec<server::clock::PlatformClock>,
}

impl DbClock {
    pub fn new(db: &DatabaseConnection) -> Self {
        Self {
            db: db.clone(),
            servers: Vec::new(),
        }
    }

    /// Also resync these servers' platform clocks on every advance.
    pub fn following(mut self, servers: Vec<server::clock::PlatformClock>) -> Self {
        self.servers = servers;
        self
    }

    /// Moves the database clock, and the servers following it, forward by
    /// `by`.
    pub async fn advance(&self, by: Duration) {
        self.shift(Self::micros(by)).await;
        for server in &self.servers {
            server
                .sync()
                .await
                .expect("resync a server's platform clock");
        }
    }

    fn micros(by: Duration) -> i64 {
        i64::try_from(by.as_micros()).expect("advance fits in i64 microseconds")
    }

    async fn shift(&self, micros: i64) {
        self.db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "UPDATE broccoli_test_clock.state SET offset_us = offset_us + $1",
                [micros.into()],
            ))
            .await
            .expect("advance the test clock");
    }

    /// The database's current time, in epoch milliseconds.
    pub async fn now_ms(&self) -> i64 {
        let row = self
            .db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT (EXTRACT(EPOCH FROM now()) * 1000)::bigint AS ms".to_string(),
            ))
            .await
            .expect("read the test clock")
            .expect("one row");
        row.try_get("", "ms").expect("ms column")
    }

    /// Moves the clock to `at_ms` epoch milliseconds, or leaves it if the
    /// database is already there.
    pub async fn advance_to_ms(&self, at_ms: i64) {
        let behind = at_ms - self.now_ms().await;
        if behind > 0 {
            self.advance(Duration::from_millis(behind as u64)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Redis
// ---------------------------------------------------------------------------

/// Host port of this process's shared Redis container, started on first use.
pub async fn shared_redis_port() -> u16 {
    let (_, port) = SHARED_REDIS
        .get_or_init(|| async {
            let container = common::test_util::redis_image()
                .start()
                .await
                .expect("Failed to start Redis container");
            let port = container
                .get_host_port_ipv4(6379)
                .await
                .expect("Failed to get Redis port");
            remove_at_exit(container.id());
            (container, port)
        })
        .await;
    *port
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// A buffered HTTP response: status, headers, raw text and the JSON body
/// (`Value::Null` when the body is not JSON).
pub struct TestResponse {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub text: String,
    pub body: Value,
}

impl TestResponse {
    pub async fn from_response(res: reqwest::Response) -> Self {
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        let text = res.text().await.unwrap_or_default();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Self {
            status,
            headers,
            text,
            body,
        }
    }

    pub fn id(&self) -> i32 {
        self.body["id"]
            .as_i64()
            .expect("response body should contain 'id'") as i32
    }
}
