use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::worker::{Executor, Task, TaskResult};
use mq::{BrokerMessage, MqConfig, init_mq};
use plugin_core::config::PluginConfig;
use reqwest::Client;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, ConnectionTrait, Database, DatabaseConnection,
    DbBackend, EntityTrait, QueryFilter, Set, Statement, TransactionTrait,
};
use serde_json::Value;
use testcontainers::ContainerAsync;
use testcontainers::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use tokio::sync::{Mutex, OnceCell, RwLock, watch};
use tokio::task::JoinHandle;
use worker::models::operation::OperationTaskExecutor;
use worker::models::operation::sandbox::isolate::IsolateSandboxManager;
use worker::models::operation::sandbox::mock::MockSandboxManager;

use server::config::{
    AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
    MqAppConfig, ServerConfig, SubmissionConfig,
};
use server::consumers::consume_operation_results;
use server::entity::{user, user_role};
use server::manager::ServerManager;
use server::registry::{
    CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
    LanguageResolverRegistry, OperationBatches, OperationWaiters,
};
use server::state::AppState;
use server::utils::plugin::sync_plugins;

static SHARED_PG: OnceCell<(ContainerAsync<Postgres>, u16)> = OnceCell::const_new();
static SHARED_REDIS: OnceCell<(ContainerAsync<Redis>, u16)> = OnceCell::const_new();
static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);
static CREATE_DB_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static PG_CONTAINER_ID: OnceLock<String> = OnceLock::new();
static REDIS_CONTAINER_ID: OnceLock<String> = OnceLock::new();

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

fn admin_runtime() -> &'static tokio::runtime::Handle {
    ADMIN_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name("e2e-admin")
                .build()
                .expect("Failed to build admin runtime")
        })
        .handle()
}

async fn create_test_database(db_name: &str) {
    let port = shared_pg_port().await;
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

extern "C" fn cleanup_containers() {
    for id in [PG_CONTAINER_ID.get(), REDIS_CONTAINER_ID.get()]
        .into_iter()
        .flatten()
    {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", "-v", id])
            .output();
    }
}

async fn shared_pg_port() -> u16 {
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

            let _ = PG_CONTAINER_ID.set(container.id().to_string());

            let template_url =
                format!("postgres://postgres:postgres@127.0.0.1:{port}/template_test");
            let template_db = server::database::init_db(&template_url)
                .await
                .expect("Failed to initialize template database");
            server::seed::seed_role_permissions(&template_db)
                .await
                .expect("Failed to seed template database");
            server::seed::ensure_indexes(&template_db)
                .await
                .expect("Failed to create indexes");
            drop(template_db);

            (container, port)
        })
        .await;
    *port
}

async fn shared_redis_port() -> u16 {
    let (_, port) = SHARED_REDIS
        .get_or_init(|| async {
            let container = Redis::default()
                .start()
                .await
                .expect("Failed to start Redis container");
            let port = container
                .get_host_port_ipv4(6379)
                .await
                .expect("Failed to get Redis port");

            let _ = REDIS_CONTAINER_ID.set(container.id().to_string());

            static ATEXIT_REGISTERED: OnceLock<()> = OnceLock::new();
            ATEXIT_REGISTERED.get_or_init(|| unsafe {
                libc::atexit(cleanup_containers);
            });

            (container, port)
        })
        .await;
    *port
}

fn create_sandbox_manager()
-> Box<dyn worker::models::operation::sandbox::SandboxManager + Send + Sync> {
    let backend = std::env::var("E2E_SANDBOX_BACKEND").unwrap_or_else(|_| {
        if cfg!(target_os = "linux") {
            "isolate".into()
        } else {
            "mock".into()
        }
    });

    match backend.as_str() {
        "mock" => Box::new(MockSandboxManager::default()),
        _ => Box::new(IsolateSandboxManager::new("isolate".into(), true)),
    }
}

fn plugins_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
}

/// Worker platform-tools dir (`broccoli-compare`, ...) for real-sandbox judging.
/// Judging fuses a `check` step that mounts `broccoli-compare` as a
/// `MountSource::PlatformTool`, so a real-isolate run rejects the job without
/// it. Resolution: `BROCCOLI_WORKER_TOOLS_DIR` if set, else the conventional
/// deployment path when present. `None` for mock-sandbox runs, which never
/// resolve a platform tool.
fn worker_tools_dir() -> Option<PathBuf> {
    let backend = std::env::var("E2E_SANDBOX_BACKEND").unwrap_or_else(|_| {
        if cfg!(target_os = "linux") {
            "isolate".into()
        } else {
            "mock".into()
        }
    });
    if backend.eq_ignore_ascii_case("mock") {
        return None;
    }
    if let Ok(dir) = std::env::var("BROCCOLI_WORKER_TOOLS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let default = PathBuf::from("/opt/broccoli/tools");
    default.is_dir().then_some(default)
}

#[allow(dead_code)]
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

#[allow(dead_code)]
pub struct E2eTestApp {
    pub base_url: String,
    pub client: Client,
    pub db: DatabaseConnection,
    server_handle: Option<JoinHandle<()>>,
    claim_cancel: Option<watch::Sender<bool>>,
    claim_handle: Option<JoinHandle<()>>,
    worker_handle: Option<JoinHandle<()>>,
    result_consumer_handle: Option<JoinHandle<()>>,
}

impl Drop for E2eTestApp {
    fn drop(&mut self) {
        if let Some(h) = self.server_handle.take() {
            h.abort();
        }
        if let Some(tx) = self.claim_cancel.take() {
            let _ = tx.send(true);
        }
        if let Some(h) = self.claim_handle.take() {
            h.abort();
        }
        if let Some(h) = self.worker_handle.take() {
            h.abort();
        }
        if let Some(h) = self.result_consumer_handle.take() {
            h.abort();
        }
        close_database_pool(self.db.clone());
    }
}

fn close_database_pool(db: DatabaseConnection) {
    let _ = std::thread::Builder::new()
        .name("e2e-db-close".to_string())
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

#[allow(dead_code)]
impl E2eTestApp {
    /// Default app DB pool size. Small on purpose: e2e runs many isolated test
    /// databases and Postgres backend slots are finite.
    const DEFAULT_DB_MAX_CONNECTIONS: u32 = 3;

    pub async fn spawn() -> Self {
        if let Ok(server_url) = std::env::var("E2E_SERVER_URL") {
            Self::connect(&server_url).await
        } else {
            Self::spawn_internal(true, Self::DEFAULT_DB_MAX_CONNECTIONS).await
        }
    }

    pub async fn spawn_without_plugins() -> Self {
        if let Ok(server_url) = std::env::var("E2E_SERVER_URL") {
            Self::connect(&server_url).await
        } else {
            Self::spawn_internal(false, Self::DEFAULT_DB_MAX_CONNECTIONS).await
        }
    }

    /// Like [`spawn`], but with a larger app DB pool. The e2e harness routes
    /// plugin host-function DB calls through the same pool the HTTP handlers use
    /// (production isolates them into a dedicated plugin pool). A submission
    /// handler holds its transaction open across the `before_submission` hook,
    /// so every in-flight submission needs TWO pooled connections at once (its
    /// own txn plus the plugin's claim query). Tests that fire many submissions
    /// concurrently must size the pool for `2 x concurrency` or the pool
    /// deadlocks and requests fail with a spurious "connection pool timed out"
    /// 500 that has nothing to do with the code under test.
    pub async fn spawn_with_db_pool(db_max_connections: u32) -> Self {
        if let Ok(server_url) = std::env::var("E2E_SERVER_URL") {
            Self::connect(&server_url).await
        } else {
            Self::spawn_internal(true, db_max_connections).await
        }
    }

    async fn connect(server_url: &str) -> Self {
        let db_url = std::env::var("E2E_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:password@127.0.0.1:5433/broccoli".into());

        let mut opts = ConnectOptions::new(&db_url);
        opts.max_connections(3)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(5));
        let db = Database::connect(opts)
            .await
            .expect("Failed to connect to Docker database");

        let base_url = server_url.trim_end_matches('/').to_string();

        let client = Client::builder()
            .no_proxy()
            .cookie_store(true)
            .build()
            .expect("Failed to build reqwest client");

        let health_url = format!("{base_url}/api-docs/openapi.json");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(res) = client.get(&health_url).send().await
                && res.status().is_success()
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "Docker server at {base_url} did not become healthy within 60s"
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        Self {
            base_url,
            client,
            db,
            server_handle: None,
            claim_cancel: None,
            claim_handle: None,
            worker_handle: None,
            result_consumer_handle: None,
        }
    }

    async fn spawn_internal(load_plugins: bool, db_max_connections: u32) -> Self {
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);

        let (pg_port, redis_port) = tokio::join!(shared_pg_port(), shared_redis_port());

        let db_name = format!("e2e_{test_id}");

        {
            let _lock = CREATE_DB_LOCK.get_or_init(|| Mutex::new(())).lock().await;
            create_test_database(&db_name).await;
        }

        let db_url = format!("postgres://postgres:postgres@127.0.0.1:{pg_port}/{db_name}");
        let mut opts = ConnectOptions::new(&db_url);
        opts.max_connections(db_max_connections)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(3));
        let db = Database::connect(opts)
            .await
            .expect("Failed to connect to test database");

        let redis_url = format!("redis://127.0.0.1:{redis_port}");

        let operation_queue = format!("e2e_{test_id}_ops");
        let result_queue = format!("e2e_{test_id}_results");
        let dlq_queue = format!("e2e_{test_id}_dlq");

        let mq = Arc::new(
            init_mq(MqConfig {
                url: redis_url.clone(),
                pool_size: 5,
            })
            .await
            .expect("Failed to initialize MQ"),
        );

        let blob_store = common::storage::config::create_blob_store(
            &BlobStoreConfig::default(),
            db.clone(),
            None,
        )
        .await
        .expect("Failed to initialize blob store");

        let app_config = AppConfig {
            server: ServerConfig {
                host: "127.0.0.1".to_string(),
                port: 0,
                cors: CorsConfig {
                    allow_origins: vec![],
                    max_age: 3600,
                },
                public_base_url: None,
                frontend_dist: PathBuf::from("/srv/dist"),
                trusted_proxies: vec![],
                rate_limit_auth: false,
                id: String::new(),
                expects_multi_replica: false,
                dispatcher_lease_steal_enabled: false,
                dispatcher_semaphore_enabled: false,
                dispatcher_concurrency: 1,
                dispatcher_admission_queue_max: 0,
                // UP#39: disable durable Queued-depth backpressure in e2e
                // by default; tests opt in if they need to exercise it.
                max_queued_submissions: 0,
                lease_ttl_secs: 60,
                lease_refresh_interval_secs: 10,
                steal_scan_interval_secs: 15,
                steal_batch_size: 8,
                sweep_interval_secs: 300,
                max_dispatch_retries: 5,
                max_system_error_retries: 50,
                max_stuck_retries: 5,
                sweeper_dry_run: true,
                operation_reaper_enabled: false,
                operation_reaper_interval_secs: 30,
                operation_reaper_grace_secs: 30,
                operation_reaper_max_requeues_per_tick: 1000,
                operation_reaper_dry_run: false,
                cancel_primitive_enabled: false,
                max_blocking_threads: None,
                batch_evaluator_fanout_concurrency: 64,
                operation_batch_publish_concurrency: 32,
                healthz_listen: None,
                healthz_worker_threads: 2,
                claim_fiber_enabled: true,
                claim_poll_interval_ms: 100,
                claim_batch_size: 32,
            },
            database: DatabaseConfig {
                url: db_url.clone(),
                max_connections: db_max_connections,
                plugin_max_connections: 1,
                plugin_privileged_max_connections: 1,
                plugin_url: None,
            },
            auth: AuthConfig {
                jwt_secret: "e2e-test-jwt-secret".to_string(),
                secure_cookies: false,
                login_failure_limit: 0,
                login_failure_window_secs: 60,
            },
            plugin: PluginConfig {
                plugins_dir: plugins_dir(),
                ..Default::default()
            },
            submission: SubmissionConfig::default(),
            storage: BlobStoreConfig::default(),
            mq: MqAppConfig {
                enabled: true,
                url: redis_url.clone(),
                pool_size: 5,
                operation_queue_name: operation_queue.clone(),
                operation_result_queue_name: result_queue.clone(),
                operation_dlq_queue_name: dlq_queue.clone(),
                ..Default::default()
            },
            observability: common::config::ObservabilityConfig::default(),
            batch_max_age_secs: 600,
            bootstrap: BootstrapConfig::default(),
        };

        let contest_type_registry: ContestTypeRegistry = Arc::new(RwLock::new(HashMap::new()));
        let evaluator_registry: EvaluatorRegistry = Arc::new(RwLock::new(HashMap::new()));
        let checker_stage_registry: CheckerStageRegistry = Arc::new(RwLock::new(HashMap::new()));
        let language_resolver_registry: LanguageResolverRegistry =
            Arc::new(RwLock::new(HashMap::new()));
        let operation_batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches: EvaluateBatches = Arc::new(dashmap::DashMap::new());
        let evaluate_ops_registry =
            server::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry::default();
        let hook_registry = server::hooks::new_shared_registry();

        let plugins = ServerManager::new(
            app_config.plugin.clone(),
            server::host_funcs::context::HostFunctionSystemDeps {
                db: db.clone(),
                // Test harness reuses the single app-role pool for both the
                // restricted and privileged plugin pools; the phase-2 role
                // restriction is proven directly against a real DB in
                // `server::database`'s unit tests.
                privileged_db: db.clone(),
                mq: Some(Arc::clone(&mq)),
                operation_batches: operation_batches.clone(),
                operation_waiters: operation_waiters.clone(),
                contest_type_registry: contest_type_registry.clone(),
                evaluator_registry: evaluator_registry.clone(),
                checker_stage_registry: checker_stage_registry.clone(),
                language_resolver_registry: language_resolver_registry.clone(),
                evaluate_batches: evaluate_batches.clone(),
                evaluate_ops_registry: evaluate_ops_registry.clone(),
                blob_store: blob_store.clone(),
                hook_registry: hook_registry.clone(),
                config: app_config.clone(),
                metrics: None,
                redis_client: None,
            },
            None,
        )
        .expect("Failed to initialize ServerManager");

        let (e2e_metrics, e2e_prom_registry) =
            common::observability::init_metrics("broccoli-e2e-test");

        let state = AppState {
            plugins,
            db: db.clone(),
            config: app_config,
            mq: Some(Arc::clone(&mq)),
            redis_client: None,
            blob_store: blob_store.clone(),
            registries: server::state::RegistryState {
                contest_type_registry,
                evaluator_registry,
                checker_stage_registry,
                language_resolver_registry,
                operation_batches,
                operation_waiters: operation_waiters.clone(),
                evaluate_batches,
                hook_registry,
            },
            device_codes: Arc::new(dashmap::DashMap::new()),
            metrics: e2e_metrics.clone(),
            prometheus_registry: e2e_prom_registry,
            dispatcher_permits: server::dispatcher::permits::DispatcherSemaphore::default(),
            login_throttle: Arc::new(server::utils::login_throttle::LoginThrottle::new(
                0,
                std::time::Duration::from_secs(60),
            )),
        };

        let mut result_consumer_handle_opt = None;
        let mut claim_cancel_opt = None;
        let mut claim_handle_opt = None;
        let mut worker_handle_opt = None;

        if load_plugins {
            let failures = sync_plugins(&state).await.expect("Failed to sync plugins");
            assert!(
                failures.is_empty(),
                "Plugin activations failed: {}",
                failures
                    .iter()
                    .map(|f| format!("{}: {}", f.plugin_id, f.error))
                    .collect::<Vec<_>>()
                    .join("; ")
            );

            let consumer_mq = Arc::clone(&mq);
            let consumer_waiters = operation_waiters.clone();
            let consumer_evaluate_ops_registry = evaluate_ops_registry.clone();
            let consumer_queue = result_queue.clone();
            let consumer_metrics = e2e_metrics.clone();
            result_consumer_handle_opt = Some(tokio::spawn(async move {
                consume_operation_results(
                    consumer_mq,
                    consumer_waiters,
                    consumer_evaluate_ops_registry,
                    consumer_queue,
                    consumer_metrics,
                )
                .await;
            }));

            let (claim_cancel_tx, claim_cancel_rx) = watch::channel(false);
            claim_cancel_opt = Some(claim_cancel_tx);
            claim_handle_opt = Some(tokio::spawn(server::dispatcher::claim::run(
                state.clone(),
                format!("e2e-{test_id}"),
                state.config.server.claim_poll_interval_ms,
                state.config.server.claim_batch_size,
                claim_cancel_rx,
            )));

            let sandbox_manager = create_sandbox_manager();
            let worker_cache_dir =
                std::env::temp_dir().join(format!("broccoli-e2e-worker-cache-{test_id}"));
            let executor = Arc::new(
                OperationTaskExecutor::new_with_sandbox_manager_and_blob_store(
                    sandbox_manager,
                    blob_store.clone(),
                    worker_cache_dir,
                    e2e_metrics.clone(),
                    worker_tools_dir(),
                )
                .await
                .expect("Failed to initialize e2e operation executor"),
            );
            let worker_queue = operation_queue.clone();
            let worker_mq = Arc::clone(&mq);
            let worker_mq_for_publish = Arc::clone(&mq);

            worker_handle_opt = Some(tokio::spawn(async move {
                let executor = executor;
                let mq_pub = worker_mq_for_publish;
                let _ = worker_mq
                    .process_messages(
                        &worker_queue,
                        None,
                        None,
                        move |message: BrokerMessage<Task>| {
                            let executor = Arc::clone(&executor);
                            let mq = Arc::clone(&mq_pub);
                            async move {
                                let task = message.payload;
                                let err_str: String;
                                let result = match executor.execute(task.clone()).await {
                                    Ok(r) => r,
                                    Err(e) => {
                                        err_str = e.to_string();
                                        TaskResult {
                                            task_id: task.id.clone(),
                                            success: false,
                                            output: serde_json::Value::String(err_str.clone()),
                                            error: Some(err_str),
                                            task_type: Some(task.task_type.clone()),
                                            operation: Some(task.executor_name.clone()),
                                            worker_id: None,
                                            enqueued_at_unix_ms: task.enqueued_at_unix_ms,
                                        }
                                    }
                                };
                                mq.publish(task.reply_queue_name(), None, &result, None)
                                    .await?;
                                Ok(())
                            }
                        },
                    )
                    .await;
            }));
        } else {
            state.registries.evaluator_registry.write().await.insert(
                "batch".into(),
                server::registry::PluginHandler {
                    plugin_id: "__test__".into(),
                    function_name: "noop".into(),
                },
            );
            {
                let mut stage = state.registries.checker_stage_registry.write().await;
                for fmt in ["exact", "none"] {
                    stage.insert(
                        fmt.into(),
                        server::registry::CheckerStageHandlers {
                            plugin_id: "__test__".into(),
                            resolve_fn: "noop".into(),
                            interpret_fn: "noop".into(),
                        },
                    );
                }
            }
            state.registries.contest_type_registry.write().await.insert(
                "standard".into(),
                server::registry::ContestTypeHandlers {
                    plugin_id: "__test__".into(),
                    submission_fn: "noop".into(),
                    code_run_fn: "noop".into(),
                },
            );
        }

        let app = server::build_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind to random port");
        let addr = listener.local_addr().unwrap();

        let server_handle = tokio::spawn(async move {
            server::serve::serve_with_graceful_shutdown(
                listener,
                app,
                server::serve::pending_shutdown_signal(),
            )
            .await
            .unwrap();
        });

        tokio::task::yield_now().await;

        Self {
            base_url: format!("http://{addr}"),
            client: Client::builder()
                .no_proxy()
                .cookie_store(true)
                .build()
                .expect("Failed to build reqwest client"),
            db,
            server_handle: Some(server_handle),
            claim_cancel: claim_cancel_opt,
            claim_handle: claim_handle_opt,
            worker_handle: worker_handle_opt,
            result_consumer_handle: result_consumer_handle_opt,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub async fn post_with_token(&self, path: &str, body: &Value, token: &str) -> TestResponse {
        let res = self
            .client
            .post(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .json(body)
            .send()
            .await
            .expect("Failed to send POST request");
        TestResponse::from_response(res).await
    }

    pub async fn post_without_token(&self, path: &str, body: &Value) -> TestResponse {
        let res = self
            .client
            .post(self.url(path))
            .json(body)
            .send()
            .await
            .expect("Failed to send POST request");
        TestResponse::from_response(res).await
    }

    pub async fn get_with_token(&self, path: &str, token: &str) -> TestResponse {
        let res = self
            .client
            .get(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .expect("Failed to send GET request");
        TestResponse::from_response(res).await
    }

    pub async fn get_without_token(&self, path: &str) -> TestResponse {
        let res = self
            .client
            .get(self.url(path))
            .send()
            .await
            .expect("Failed to send GET request");
        TestResponse::from_response(res).await
    }

    pub async fn patch_with_token(&self, path: &str, body: &Value, token: &str) -> TestResponse {
        let res = self
            .client
            .patch(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .json(body)
            .send()
            .await
            .expect("Failed to send PATCH request");
        TestResponse::from_response(res).await
    }

    pub async fn put_with_token(&self, path: &str, body: &Value, token: &str) -> TestResponse {
        let res = self
            .client
            .put(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .json(body)
            .send()
            .await
            .expect("Failed to send PUT request");
        TestResponse::from_response(res).await
    }

    pub async fn delete_with_token(&self, path: &str, token: &str) -> TestResponse {
        let res = self
            .client
            .delete(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .expect("Failed to send DELETE request");
        TestResponse::from_response(res).await
    }

    pub async fn delete_with_body_and_token(
        &self,
        path: &str,
        body: &Value,
        token: &str,
    ) -> TestResponse {
        let res = self
            .client
            .delete(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .json(body)
            .send()
            .await
            .expect("Failed to send DELETE request");
        TestResponse::from_response(res).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upload_with_token(
        &self,
        path: &str,
        file_name: &str,
        file_bytes: Vec<u8>,
        input_format: Option<&str>,
        output_format: Option<&str>,
        strategy: Option<&str>,
        token: &str,
    ) -> TestResponse {
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str("application/zip")
            .expect("Failed to set MIME type");
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if let Some(v) = input_format {
            form = form.text("input_format", v.to_string());
        }
        if let Some(v) = output_format {
            form = form.text("output_format", v.to_string());
        }
        if let Some(v) = strategy {
            form = form.text("strategy", v.to_string());
        }
        let res = self
            .client
            .post(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .expect("Failed to send multipart upload request");
        TestResponse::from_response(res).await
    }

    pub async fn upload_attachment(
        &self,
        problem_id: i32,
        file_name: &str,
        file_bytes: Vec<u8>,
        path: Option<&str>,
        token: &str,
    ) -> TestResponse {
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str("application/octet-stream")
            .expect("Failed to set MIME type");
        let mut form = reqwest::multipart::Form::new().part("file", part);
        if let Some(p) = path {
            form = form.text("path", p.to_string());
        }
        let res = self
            .client
            .post(self.url(&format!("/api/v1/problems/{problem_id}/attachments")))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .expect("Failed to send attachment upload request");
        TestResponse::from_response(res).await
    }

    pub async fn download_raw(&self, path: &str, token: &str) -> reqwest::Response {
        self.client
            .get(self.url(path))
            .header("Authorization", format!("Bearer {token}"))
            .send()
            .await
            .expect("Failed to send download request")
    }

    pub async fn create_authenticated_user(&self, username: &str, password: &str) -> String {
        let body = serde_json::json!({ "username": username, "password": password });
        let reg = self
            .post_without_token("/api/v1/auth/register", &body)
            .await;
        assert_eq!(reg.status, 201, "Registration failed: {}", reg.text);
        let res = self.post_without_token("/api/v1/auth/login", &body).await;
        assert_eq!(res.status, 200, "Login failed: {}", res.text);
        res.body["token"]
            .as_str()
            .expect("Login response should contain a token")
            .to_string()
    }

    pub async fn create_user_with_role(
        &self,
        username: &str,
        password: &str,
        role: &str,
    ) -> String {
        let body = serde_json::json!({ "username": username, "password": password });
        let reg = self
            .post_without_token("/api/v1/auth/register", &body)
            .await;
        assert_eq!(reg.status, 201, "Registration failed: {}", reg.text);

        let db_user = user::Entity::find()
            .filter(user::Column::Username.eq(username))
            .one(&self.db)
            .await
            .expect("DB query failed")
            .expect("User not found after registration");

        let txn = self.db.begin().await.expect("Failed to begin transaction");
        user_role::Entity::delete_many()
            .filter(user_role::Column::UserId.eq(db_user.id))
            .exec(&txn)
            .await
            .expect("Failed to clear existing user roles");
        user_role::ActiveModel {
            user_id: Set(db_user.id),
            role: Set(role.to_string()),
        }
        .insert(&txn)
        .await
        .expect("Failed to insert user role");
        txn.commit().await.expect("Failed to commit transaction");

        let res = self.post_without_token("/api/v1/auth/login", &body).await;
        assert_eq!(res.status, 200, "Login failed: {}", res.text);
        res.body["token"]
            .as_str()
            .expect("Login response should contain a token")
            .to_string()
    }

    /// Creates a hidden-draft problem (`is_public = false`, the server
    /// default): unprivileged users can only reach it through a contest.
    pub async fn create_problem(&self, token: &str, title: &str) -> i32 {
        self.create_problem_with(token, title, "exact", false).await
    }

    /// Creates a problem published to the standalone problemset
    /// (`is_public = true`): any authenticated user can read and submit to it
    /// without a contest. Use this when a test's plain user legitimately
    /// submits outside a contest.
    pub async fn create_public_problem(&self, token: &str, title: &str) -> i32 {
        self.create_problem_with(token, title, "exact", true).await
    }

    pub async fn create_problem_with_checker_format(
        &self,
        token: &str,
        title: &str,
        checker_format: &str,
    ) -> i32 {
        self.create_problem_with(token, title, checker_format, false)
            .await
    }

    async fn create_problem_with(
        &self,
        token: &str,
        title: &str,
        checker_format: &str,
        is_public: bool,
    ) -> i32 {
        let res = self
            .post_with_token(
                "/api/v1/problems",
                &serde_json::json!({
                    "title": title,
                    "content": "## Description\nSolve this.",
                    "time_limit": 2000,
                    "memory_limit": 262144,
                    "problem_type": "batch",
                    "checker_format": checker_format,
                    "is_public": is_public,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_problem failed: {}", res.text);
        res.id()
    }

    pub async fn create_test_case(&self, problem_id: i32, token: &str) -> i32 {
        self.create_test_case_with(problem_id, "5\n1 2 3 4 5", "15", 10, true, token)
            .await
    }

    pub async fn create_test_case_with(
        &self,
        problem_id: i32,
        input: &str,
        expected_output: &str,
        score: i32,
        is_sample: bool,
        token: &str,
    ) -> i32 {
        let res = self
            .post_with_token(
                &format!("/api/v1/problems/{problem_id}/test-cases"),
                &serde_json::json!({
                    "input": input,
                    "expected_output": expected_output,
                    "score": score,
                    "is_sample": is_sample,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_test_case failed: {}", res.text);
        res.id()
    }

    pub async fn create_contest(
        &self,
        token: &str,
        title: &str,
        is_public: bool,
        submissions_visible: bool,
    ) -> i32 {
        let res = self
            .post_with_token(
                "/api/v1/contests",
                &serde_json::json!({
                    "title": title,
                    "description": "Contest description",
                    "activate_time": "2020-01-01T00:00:00Z",
                    "start_time": "2020-01-01T00:00:00Z",
                    "end_time": "2099-01-02T00:00:00Z",
                    "is_public": is_public,
                    "submissions_visible": submissions_visible,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_contest failed: {}", res.text);
        res.id()
    }

    pub async fn create_typed_contest(
        &self,
        token: &str,
        title: &str,
        contest_type: &str,
        is_public: bool,
        submissions_visible: bool,
    ) -> i32 {
        let res = self
            .post_with_token(
                "/api/v1/contests",
                &serde_json::json!({
                    "title": title,
                    "description": "Contest description",
                    "activate_time": "2020-01-01T00:00:00Z",
                    "start_time": "2020-01-01T00:00:00Z",
                    "end_time": "2099-01-02T00:00:00Z",
                    "is_public": is_public,
                    "submissions_visible": submissions_visible,
                    "contest_type": contest_type,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_typed_contest failed: {}", res.text);
        res.id()
    }

    pub async fn add_problem_to_contest(&self, contest_id: i32, problem_id: i32, token: &str) {
        self.add_problem_to_contest_with_label(contest_id, problem_id, "A", token)
            .await;
    }

    pub async fn add_problem_to_contest_with_label(
        &self,
        contest_id: i32,
        problem_id: i32,
        label: &str,
        token: &str,
    ) {
        let res = self
            .post_with_token(
                &format!("/api/v1/contests/{contest_id}/problems"),
                &serde_json::json!({ "problem_id": problem_id, "label": label }),
                token,
            )
            .await;
        assert_eq!(
            res.status, 201,
            "add_problem_to_contest failed: {}",
            res.text
        );
    }

    pub async fn register_for_contest(&self, contest_id: i32, token: &str) {
        let res = self
            .post_with_token(
                &format!("/api/v1/contests/{contest_id}/register"),
                &serde_json::json!({}),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "register_for_contest failed: {}", res.text);
    }

    pub async fn create_submission(
        &self,
        problem_id: i32,
        token: &str,
        language: &str,
        code: &str,
    ) -> i32 {
        let filename = match language {
            "cpp" => "main.cpp",
            "c" => "main.c",
            "java" => "Main.java",
            "python3" => "solution.py",
            _ => "main.txt",
        };
        let res = self
            .post_with_token(
                &format!("/api/v1/problems/{problem_id}/submissions"),
                &serde_json::json!({
                    "files": [{"filename": filename, "content": code}],
                    "language": language,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_submission failed: {}", res.text);
        res.id()
    }

    pub async fn create_contest_submission(
        &self,
        contest_id: i32,
        problem_id: i32,
        token: &str,
        language: &str,
        code: &str,
    ) -> i32 {
        let filename = match language {
            "cpp" => "main.cpp",
            "c" => "main.c",
            "java" => "Main.java",
            "python3" => "solution.py",
            _ => "main.txt",
        };
        let res = self
            .post_with_token(
                &format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions"),
                &serde_json::json!({
                    "files": [{"filename": filename, "content": code}],
                    "language": language,
                }),
                token,
            )
            .await;
        assert_eq!(
            res.status, 201,
            "create_contest_submission failed: {}",
            res.text
        );
        res.id()
    }

    pub async fn wait_for_submission_terminal(
        &self,
        submission_id: i32,
        token: &str,
        timeout_secs: u64,
    ) -> TestResponse {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let res = self
                .get_with_token(&format!("/api/v1/submissions/{submission_id}"), token)
                .await;
            assert_eq!(res.status, 200, "Failed to get submission: {}", res.text);

            let status = res.body["status"].as_str().unwrap_or("");
            if matches!(status, "Judged" | "CompilationError" | "SystemError") {
                return res;
            }

            assert!(
                tokio::time::Instant::now() < deadline,
                "Submission {submission_id} did not reach terminal state within {timeout_secs}s. \
                 Last status: {status}, body: {}",
                res.text
            );

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    pub async fn wait_for_code_run_terminal(
        &self,
        code_run_id: i32,
        token: &str,
        timeout_secs: u64,
    ) -> TestResponse {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        loop {
            let res = self
                .get_with_token(&format!("/api/v1/code-runs/{code_run_id}"), token)
                .await;
            assert_eq!(res.status, 200, "Failed to get code run: {}", res.text);

            let status = res.body["status"].as_str().unwrap_or("");
            if matches!(status, "Judged" | "CompilationError" | "SystemError") {
                return res;
            }

            assert!(
                tokio::time::Instant::now() < deadline,
                "Code run {code_run_id} did not reach terminal state within {timeout_secs}s. \
                 Last status: {status}, body: {}",
                res.text
            );

            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}
