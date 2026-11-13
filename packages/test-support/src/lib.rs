//! Shared harness for the server's `tests/integration` and `tests/e2e` trees.
//!
//! Cargo builds each test tree as its own binary, which is why the two
//! harnesses were forked; this dev-dependency is the one place their shared
//! infrastructure lives: the per-process Postgres and Redis containers, the
//! migrated and seeded `template_test` database every test database is cloned
//! from, the admin connection that clones it, pool teardown, and the HTTP
//! response wrapper. Each tree keeps its own app fixture (`TestApp`,
//! `E2eTestApp`) and re-exports what it needs from here.

use std::sync::{Mutex as StdMutex, OnceLock};
use std::time::Duration;

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
};
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
                .with_cmd(["postgres", "-c", "max_connections=500"])
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
            server::seed::ensure_indexes(&template_db, false)
                .await
                .expect("Failed to create indexes");
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

async fn clone_template(port: u16, db_name: &str) {
    let handle = admin_runtime().clone();
    let db_name = db_name.to_string();

    handle
        .spawn(async move {
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
                let _ = SHARED_ADMIN_CONN.set(conn);
            }

            let admin = SHARED_ADMIN_CONN
                .get()
                .expect("shared admin connection must be initialized");
            admin
                .execute_raw(Statement::from_string(
                    DbBackend::Postgres,
                    format!("CREATE DATABASE \"{db_name}\" TEMPLATE template_test"),
                ))
                .await
                .expect("Failed to create test database from template");
        })
        .await
        .expect("admin runtime task panicked");
}

/// Clone database `db_name` from `template_test` and return its connection
/// URL. Creation is serialized: Postgres refuses concurrent clones of one
/// template.
pub async fn create_test_database(db_name: &str) -> String {
    let port = shared_pg_port().await;
    {
        let _lock = CREATE_DB_LOCK.lock().await;
        clone_template(port, db_name).await;
    }
    test_database_url(db_name).await
}

/// Connection URL of an existing test database on the shared container, for
/// a second app instance that joins a database another one created.
pub async fn test_database_url(db_name: &str) -> String {
    let port = shared_pg_port().await;
    format!("postgres://postgres:postgres@127.0.0.1:{port}/{db_name}")
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
            let _ = runtime.block_on(db.close_by_ref());
        });
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
