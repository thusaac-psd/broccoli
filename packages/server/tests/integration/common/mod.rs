use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

use plugin_core::config::PluginConfig;
use plugin_core::error::PluginError;
use plugin_core::host::HostFunctionRegistry;
use plugin_core::i18n::I18nRegistry;
use plugin_core::manifest::PluginManifest;
use plugin_core::registry::PluginRegistry;
use plugin_core::traits::{PluginInvoker, PluginManager};
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
use tokio::sync::{Mutex, OnceCell, RwLock};

use common::storage::config::create_blob_store;
use server::config::{
    AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
    MqAppConfig, ServerConfig, SubmissionConfig,
};
use server::entity::{user, user_role};
use server::manager::ServerManager;
use server::registry::{
    CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
    LanguageResolverEntry, LanguageResolverRegistry, OperationBatches, OperationWaiters,
};
use server::state::AppState;
use server::utils::plugin::sync_plugins;

static SHARED_PG: OnceCell<(ContainerAsync<Postgres>, u16)> = OnceCell::const_new();

static DB_COUNTER: AtomicU32 = AtomicU32::new(0);

static CREATE_DB_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Dedicated multi-thread runtime that owns the shared admin DB connection.
///
/// `#[tokio::test]` spins up a fresh current-thread runtime per test and tears
/// it down at exit; a sqlx pool stored in a `OnceCell` would be tied to the
/// first test's runtime and become invalid for the rest. By housing the pool
/// inside a long-lived dedicated runtime and dispatching `CREATE DATABASE`
/// onto it via `Handle::spawn`, the pool outlives every individual test.
static ADMIN_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
static SHARED_ADMIN_CONN: OnceLock<DatabaseConnection> = OnceLock::new();

struct TestPluginManager {
    inner: Arc<ServerManager>,
}

#[async_trait::async_trait]
impl PluginInvoker for TestPluginManager {
    fn get_registry(&self) -> &PluginRegistry {
        self.inner.get_registry()
    }

    fn get_config(&self) -> &PluginConfig {
        self.inner.get_config()
    }

    async fn call_raw(
        &self,
        plugin_id: &str,
        func_name: &str,
        input: Vec<u8>,
    ) -> Result<Vec<u8>, PluginError> {
        if plugin_id == "__test__" && func_name == "noop" {
            return Ok(br#"{"success":true,"error_message":null}"#.to_vec());
        }

        self.inner.call_raw(plugin_id, func_name, input).await
    }
}

impl PluginManager for TestPluginManager {
    fn get_host_functions(&self) -> &HostFunctionRegistry {
        self.inner.get_host_functions()
    }

    fn get_i18n_registry(&self) -> &I18nRegistry {
        self.inner.get_i18n_registry()
    }

    fn resolve(&self, manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
        self.inner.resolve(manifest)
    }
}

fn admin_runtime() -> &'static tokio::runtime::Handle {
    ADMIN_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .thread_name("integration-admin")
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
                    .acquire_timeout(std::time::Duration::from_secs(60))
                    .idle_timeout(std::time::Duration::from_secs(600));
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

static CONTAINER_ID: OnceLock<String> = OnceLock::new();

extern "C" fn cleanup_container() {
    if let Some(id) = CONTAINER_ID.get() {
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
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        500 * u64::from(attempt),
                    ))
                    .await;
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

            let _ = CONTAINER_ID.set(container.id().to_string());

            unsafe { libc::atexit(cleanup_container) };

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

pub mod routes {
    pub const REGISTER: &str = "/api/v1/auth/register";
    pub const LOGIN: &str = "/api/v1/auth/login";
    pub const REFRESH: &str = "/api/v1/auth/refresh";
    pub const LOGOUT: &str = "/api/v1/auth/logout";
    pub const ME: &str = "/api/v1/auth/me";

    pub const USERS: &str = "/api/v1/users";

    pub fn user(id: i32) -> String {
        format!("/api/v1/users/{id}")
    }

    pub fn user_roles(id: i32) -> String {
        format!("/api/v1/users/{id}/roles")
    }

    pub fn user_role(id: i32, role_name: &str) -> String {
        format!("/api/v1/users/{id}/roles/{role_name}")
    }

    pub const ROLES: &str = "/api/v1/roles";

    pub fn role_permissions(role_name: &str) -> String {
        format!("/api/v1/roles/{role_name}/permissions")
    }

    pub fn role_permission(role_name: &str, permission_name: &str) -> String {
        format!("/api/v1/roles/{role_name}/permissions/{permission_name}")
    }

    pub const ADMIN_PLUGINS: &str = "/api/v1/admin/plugins";

    pub fn admin_plugin_details(id: &str) -> String {
        format!("/api/v1/admin/plugins/{id}")
    }

    pub fn admin_plugin_enable(id: &str) -> String {
        format!("/api/v1/admin/plugins/{id}/enable")
    }

    pub fn admin_plugin_disable(id: &str) -> String {
        format!("/api/v1/admin/plugins/{id}/disable")
    }

    pub fn plugin_proxy(id: &str, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("/api/v1/p/{id}/{path}")
    }

    pub fn plugin_proxy_with_query(id: &str, path: &str, query: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("/api/v1/p/{id}/{path}/?{query}")
    }

    pub fn plugin_asset(id: &str, path: &str) -> String {
        let path = path.trim_start_matches('/');
        format!("/assets/{id}/{path}")
    }

    pub const PROBLEMS: &str = "/api/v1/problems";

    pub fn problem(id: i32) -> String {
        format!("/api/v1/problems/{id}")
    }

    pub fn test_cases(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/test-cases")
    }

    pub fn test_case(problem_id: i32, tc_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/test-cases/{tc_id}")
    }

    pub fn test_cases_upload(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/test-cases/upload")
    }

    pub const CONTESTS: &str = "/api/v1/contests";

    pub fn contest(id: i32) -> String {
        format!("/api/v1/contests/{id}")
    }

    pub fn contest_problems(id: i32) -> String {
        format!("/api/v1/contests/{id}/problems")
    }

    pub fn contest_problem(id: i32, problem_id: i32) -> String {
        format!("/api/v1/contests/{id}/problems/{problem_id}")
    }

    pub fn contest_participants(id: i32) -> String {
        format!("/api/v1/contests/{id}/participants")
    }

    pub fn contest_participant(id: i32, user_id: i32) -> String {
        format!("/api/v1/contests/{id}/participants/{user_id}")
    }

    pub fn contest_problems_reorder(id: i32) -> String {
        format!("/api/v1/contests/{id}/problems/reorder")
    }

    pub fn contest_register(id: i32) -> String {
        format!("/api/v1/contests/{id}/register")
    }

    pub fn contest_my_info(id: i32) -> String {
        format!("/api/v1/contests/{id}/me")
    }

    pub fn test_cases_reorder(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/test-cases/reorder")
    }

    pub const SUBMISSIONS: &str = "/api/v1/submissions";

    pub fn submission(id: i32) -> String {
        format!("/api/v1/submissions/{id}")
    }

    pub fn submission_rejudge(id: i32) -> String {
        format!("/api/v1/submissions/{id}/rejudge")
    }

    pub fn submission_judgements(id: i32) -> String {
        format!("/api/v1/submissions/{id}/judgements")
    }

    pub fn submission_judgement_apply(id: i32, judgement_id: i32) -> String {
        format!("/api/v1/submissions/{id}/judgements/{judgement_id}/apply")
    }

    pub fn submission_judgement_discard(id: i32, judgement_id: i32) -> String {
        format!("/api/v1/submissions/{id}/judgements/{judgement_id}/discard")
    }

    pub fn problem_submissions(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/submissions")
    }

    pub fn contest_submissions(contest_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/submissions")
    }

    pub fn contest_problem_submissions(contest_id: i32, problem_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/submissions")
    }

    pub fn problem_code_runs(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/code-runs")
    }

    pub fn contest_problem_code_runs(contest_id: i32, problem_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/code-runs")
    }

    pub fn code_run(id: i32) -> String {
        format!("/api/v1/code-runs/{id}")
    }

    pub fn contest_clarifications(contest_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/clarifications")
    }

    pub fn contest_clarification_reply(contest_id: i32, clar_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/clarifications/{clar_id}/reply")
    }

    pub fn contest_clarification_toggle(contest_id: i32, clar_id: i32, reply_id: i32) -> String {
        format!(
            "/api/v1/contests/{contest_id}/clarifications/{clar_id}/replies/{reply_id}/toggle-public"
        )
    }

    pub fn contest_clarification_resolve(contest_id: i32, clar_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/clarifications/{clar_id}/resolve")
    }

    pub const DLQ: &str = "/api/v1/dlq";
    pub const DLQ_STATS: &str = "/api/v1/dlq/stats";

    pub fn dlq_message(id: i32) -> String {
        format!("/api/v1/dlq/{id}")
    }

    pub fn dlq_retry(id: i32) -> String {
        format!("/api/v1/dlq/{id}/retry")
    }

    pub fn test_cases_bulk(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/test-cases/bulk")
    }

    pub fn contest_problems_bulk(contest_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/problems/bulk")
    }

    pub fn contest_participants_bulk(contest_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/participants/bulk")
    }

    pub const DLQ_BULK_RETRY: &str = "/api/v1/dlq/bulk-retry";
    pub const DLQ_BULK: &str = "/api/v1/dlq/bulk";
    pub const SUBMISSIONS_BULK_REJUDGE: &str = "/api/v1/submissions/bulk-rejudge";

    pub const SYSTEM_WORKERS: &str = "/api/v1/admin/system/workers";
    pub const SYSTEM_QUEUES: &str = "/api/v1/admin/system/queues";
    pub const SYSTEM_OVERVIEW: &str = "/api/v1/admin/system/overview";

    pub fn attachments(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/attachments")
    }

    pub fn attachment(problem_id: i32, ref_id: &str) -> String {
        format!("/api/v1/problems/{problem_id}/attachments/{ref_id}")
    }

    pub fn additional_files(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/additional-files")
    }

    pub fn additional_file(problem_id: i32, ref_id: &str) -> String {
        format!("/api/v1/problems/{problem_id}/additional-files/{ref_id}")
    }

    pub fn problem_config(problem_id: i32) -> String {
        format!("/api/v1/problems/{problem_id}/config")
    }

    pub fn problem_config_ns(problem_id: i32, plugin_id: &str, namespace: &str) -> String {
        format!("/api/v1/problems/{problem_id}/config/{plugin_id}/{namespace}")
    }

    pub fn contest_config(contest_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/config")
    }

    pub fn contest_config_ns(contest_id: i32, plugin_id: &str, namespace: &str) -> String {
        format!("/api/v1/contests/{contest_id}/config/{plugin_id}/{namespace}")
    }

    pub fn plugin_global_config(plugin_id: &str) -> String {
        format!("/api/v1/admin/plugins/{plugin_id}/config")
    }

    pub fn plugin_global_config_ns(plugin_id: &str, namespace: &str) -> String {
        format!("/api/v1/admin/plugins/{plugin_id}/config/{namespace}")
    }

    pub fn contest_problem_config(contest_id: i32, problem_id: i32) -> String {
        format!("/api/v1/contests/{contest_id}/problems/{problem_id}/config")
    }

    pub fn contest_problem_config_ns(
        contest_id: i32,
        problem_id: i32,
        plugin_id: &str,
        namespace: &str,
    ) -> String {
        format!(
            "/api/v1/contests/{contest_id}/problems/{problem_id}/config/{plugin_id}/{namespace}"
        )
    }
}

pub struct TestApp {
    pub addr: SocketAddr,
    pub client: Client,
    pub db: DatabaseConnection,
    server_handle: Option<tokio::task::JoinHandle<()>>,
    dispatcher: Option<server::dispatcher::Dispatcher>,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        if let Some(handle) = self.server_handle.take() {
            handle.abort();
        }
        if let Some(mut dispatcher) = self.dispatcher.take() {
            dispatcher.abort();
        }
        close_database_pool(self.db.clone());
    }
}

fn close_database_pool(db: DatabaseConnection) {
    let _ = std::thread::Builder::new()
        .name("integration-db-close".to_string())
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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub struct TestResponse {
    pub status: u16,
    pub headers: reqwest::header::HeaderMap,
    pub text: String,
    pub body: Value,
}

/// Per-test config knobs that diverge from the default fixture in
/// `Self::spawn_internal`. Add a field here when a test needs to
/// observe a non-default `ServerConfig` value rather than copy-pasting
/// the entire fixture in the test.
#[derive(Debug, Default, Clone, Copy)]
pub struct SpawnOptions {
    /// UP#39: cap on durable `Queued` rows accepted at POST time.
    /// `Some(0)` (the fixture default) disables the cap; `Some(n)`
    /// trips backpressure once depth reaches `n`. `None` keeps the
    /// fixture default (disabled).
    pub max_queued_submissions: Option<u32>,
    /// Disable the UP#38 claim fiber. Required for backpressure
    /// tests that pre-stage `Queued` rows directly - otherwise the
    /// fiber drains them before the test can observe the cap.
    pub disable_claim_fiber: bool,
    /// Start the UP#38 claim fiber in this integration fixture. Most
    /// handler tests assert API commit state only; claim-fiber tests
    /// opt in so the shared test database is not hammered by hundreds
    /// of background pollers during parallel integration runs.
    pub start_dispatcher: bool,
}

impl TestApp {
    pub async fn spawn() -> Self {
        Self::spawn_internal(false, SpawnOptions::default()).await
    }

    pub async fn spawn_with_plugins() -> Self {
        Self::spawn_internal(true, SpawnOptions::default()).await
    }

    pub async fn spawn_with_options(options: SpawnOptions) -> Self {
        Self::spawn_internal(false, options).await
    }

    async fn spawn_internal(load_plugins: bool, options: SpawnOptions) -> Self {
        let port = shared_pg_port().await;
        let db_name = format!("test_{}", DB_COUNTER.fetch_add(1, Ordering::Relaxed));

        let _lock = CREATE_DB_LOCK.get_or_init(|| Mutex::new(())).lock().await;

        create_test_database(&db_name).await;

        drop(_lock);

        let db_url = format!("postgres://postgres:postgres@127.0.0.1:{port}/{db_name}");
        let mut opts = ConnectOptions::new(&db_url);
        opts.max_connections(2)
            .min_connections(0)
            .idle_timeout(std::time::Duration::from_secs(2));
        let db = Database::connect(opts)
            .await
            .expect("Failed to connect to test database");

        let blob_store = create_blob_store(&BlobStoreConfig::default(), db.clone(), None)
            .await
            .expect("Failed to initialize blob store");

        let mut app_config = AppConfig {
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
                // UP#39: 0 disables the durable Queued-depth cap so the
                // bulk of integration tests don't have to reason about
                // backpressure. Tests that exercise the cap call
                // `spawn_with_options(SpawnOptions { max_queued_submissions: ... })`.
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
                // Enable the UP#38 claim fiber by default so integration
                // tests that rely on the UP#37 `Queued`-on-POST flow see the
                // row transition all the way to dispatch.
                claim_fiber_enabled: true,
                claim_poll_interval_ms: 100,
                claim_batch_size: 32,
            },
            database: DatabaseConfig {
                url: db_url.clone(),
                max_connections: 2,
                plugin_max_connections: 1,
                plugin_privileged_max_connections: 1,
                plugin_url: None,
            },
            auth: AuthConfig {
                jwt_secret: "test-secret-for-integration-tests".to_string(),
                secure_cookies: false,
                login_failure_limit: 0,
                login_failure_window_secs: 60,
            },
            plugin: PluginConfig {
                plugins_dir: fixtures_dir(),
                ..Default::default()
            },
            submission: SubmissionConfig::default(),
            storage: BlobStoreConfig::default(),
            mq: MqAppConfig {
                enabled: false,
                ..Default::default()
            },
            observability: common::config::ObservabilityConfig::default(),
            batch_max_age_secs: 600,
            bootstrap: BootstrapConfig::default(),
        };

        if let Some(cap) = options.max_queued_submissions {
            app_config.server.max_queued_submissions = cap;
        }
        if options.disable_claim_fiber {
            app_config.server.claim_fiber_enabled = false;
        }

        let contest_type_registry: ContestTypeRegistry = Arc::new(RwLock::new(HashMap::new()));
        let evaluator_registry: EvaluatorRegistry = Arc::new(RwLock::new(HashMap::new()));
        let checker_stage_registry: CheckerStageRegistry = Arc::new(RwLock::new(HashMap::new()));
        let language_resolver_registry: LanguageResolverRegistry =
            Arc::new(RwLock::new(HashMap::new()));
        let operation_batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches: EvaluateBatches = Arc::new(dashmap::DashMap::new());
        let hook_registry = server::hooks::new_shared_registry();
        let evaluate_ops_registry =
            server::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry::default();
        let (test_metrics, test_prom_registry) =
            common::observability::init_metrics("broccoli-test");

        let server_plugins = ServerManager::new(
            app_config.plugin.clone(),
            server::host_funcs::context::HostFunctionSystemDeps {
                db: db.clone(),
                // Test harness reuses the single app-role pool for both the
                // restricted and privileged plugin pools; the phase-2 role
                // restriction is proven directly against a real DB in
                // `server::database`'s unit tests.
                privileged_db: db.clone(),
                mq: None,
                operation_batches: operation_batches.clone(),
                operation_waiters: operation_waiters.clone(),
                contest_type_registry: contest_type_registry.clone(),
                evaluator_registry: evaluator_registry.clone(),
                checker_stage_registry: checker_stage_registry.clone(),
                language_resolver_registry: language_resolver_registry.clone(),
                evaluate_batches: evaluate_batches.clone(),
                evaluate_ops_registry,
                blob_store: blob_store.clone(),
                hook_registry: hook_registry.clone(),
                config: app_config.clone(),
                metrics: Some(test_metrics.clone()),
                redis_client: None,
            },
            Some(test_metrics.clone()),
        )
        .expect("Failed to initialize plugin manager");
        let plugins: Arc<dyn PluginManager> = Arc::new(TestPluginManager {
            inner: server_plugins,
        });

        if !load_plugins {
            evaluator_registry.write().await.insert(
                "standard".into(),
                server::registry::PluginHandler {
                    plugin_id: "__test__".into(),
                    function_name: "noop".into(),
                },
            );
            {
                let mut stage = checker_stage_registry.write().await;
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
            contest_type_registry.write().await.insert(
                "standard".into(),
                server::registry::ContestTypeHandlers {
                    plugin_id: "__test__".into(),
                    submission_fn: "noop".into(),
                    code_run_fn: "noop".into(),
                },
            );
            let mut languages = language_resolver_registry.write().await;
            for (id, display_name, default_filename, extensions) in [
                ("c", "C", "main.c", vec!["c".to_string()]),
                (
                    "cpp",
                    "C++",
                    "main.cpp",
                    vec!["cpp".to_string(), "cc".to_string(), "cxx".to_string()],
                ),
                ("java", "Java", "Main.java", vec!["java".to_string()]),
                ("python3", "Python 3", "main.py", vec!["py".to_string()]),
            ] {
                languages.insert(
                    id.to_string(),
                    LanguageResolverEntry {
                        plugin_id: "__test__".into(),
                        function_name: "noop".into(),
                        display_name: display_name.into(),
                        default_filename: default_filename.into(),
                        extensions,
                        template: String::new(),
                    },
                );
            }
        }

        let state = AppState {
            plugins,
            db: db.clone(),
            config: app_config,
            mq: None,
            redis_client: None,
            blob_store,
            registries: server::state::RegistryState {
                contest_type_registry,
                evaluator_registry,
                checker_stage_registry,
                language_resolver_registry,
                operation_batches,
                operation_waiters,
                evaluate_batches,
                hook_registry,
            },
            device_codes: std::sync::Arc::new(dashmap::DashMap::new()),
            metrics: test_metrics.clone(),
            prometheus_registry: test_prom_registry.clone(),
            dispatcher_permits: server::dispatcher::permits::DispatcherSemaphore::default(),
            login_throttle: std::sync::Arc::new(server::utils::login_throttle::LoginThrottle::new(
                0,
                std::time::Duration::from_secs(60),
            )),
        };
        let dispatcher = options.start_dispatcher.then(|| {
            server::dispatcher::Dispatcher::spawn(server::dispatcher::DispatcherDeps {
                state: state.clone(),
                redis_client: None,
                server_id: "integration-test-server".to_string(),
                operation_result_queue_base: "operation_results".to_string(),
                config: state.config.server.clone(),
            })
        });
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

        Self {
            addr,
            client: Client::builder()
                .no_proxy()
                .cookie_store(true)
                .build()
                .expect("Failed to build reqwest client"),
            db,
            server_handle: Some(server_handle),
            dispatcher,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
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
            .expect("Failed to send DELETE request with body");

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
        if let Some(input_format) = input_format {
            form = form.text("input_format", input_format.to_string());
        }
        if let Some(output_format) = output_format {
            form = form.text("output_format", output_format.to_string());
        }
        if let Some(s) = strategy {
            form = form.text("strategy", s.to_string());
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

    pub async fn create_authenticated_user(&self, username: &str, password: &str) -> String {
        let body = serde_json::json!({
            "username": username,
            "password": password,
        });

        let reg = self.post_without_token(routes::REGISTER, &body).await;
        assert_eq!(reg.status, 201, "Registration failed: {}", reg.text);

        let res = self.post_without_token(routes::LOGIN, &body).await;
        assert_eq!(res.status, 200, "Login failed: {}", res.text);

        res.body["token"]
            .as_str()
            .expect("Login response should contain a token")
            .to_string()
    }

    pub async fn create_problem(&self, token: &str, title: &str) -> i32 {
        let res = self
            .post_with_token(
                routes::PROBLEMS,
                &serde_json::json!({
                    "title": title,
                    "content": "## Description\nSolve this.",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "standard",
                    "checker_format": "exact",
                    // Public so a contestant can read/submit to the standalone
                    // problem; `require_problem_read_access` (correctly) hides
                    // non-public problems. Tests that specifically exercise the
                    // hiding behavior use `create_hidden_problem` instead.
                    "is_public": true,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_problem failed: {}", res.text);
        res.id()
    }

    /// Like [`Self::create_problem`] but non-public, for tests that assert a
    /// contestant cannot reach a hidden/unreleased problem.
    pub async fn create_hidden_problem(&self, token: &str, title: &str) -> i32 {
        let res = self
            .post_with_token(
                routes::PROBLEMS,
                &serde_json::json!({
                    "title": title,
                    "content": "## Description\nSolve this.",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "standard",
                    "checker_format": "exact",
                    "is_public": false,
                }),
                token,
            )
            .await;
        assert_eq!(
            res.status, 201,
            "create_hidden_problem failed: {}",
            res.text
        );
        res.id()
    }

    pub async fn create_test_case(&self, problem_id: i32, token: &str) -> i32 {
        let res = self
            .post_with_token(
                &routes::test_cases(problem_id),
                &serde_json::json!({
                    "input": "5\n1 2 3 4 5",
                    "expected_output": "15",
                    "score": 10,
                    "is_sample": true,
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
                routes::CONTESTS,
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
            "javascript" => "solution.js",
            _ => "main.txt",
        };
        let res = self
            .post_with_token(
                &routes::problem_submissions(problem_id),
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

    pub async fn add_problem_to_contest(&self, contest_id: i32, problem_id: i32, token: &str) {
        let res = self
            .post_with_token(
                &routes::contest_problems(contest_id),
                &serde_json::json!({
                    "problem_id": problem_id,
                    "label": "A",
                }),
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
                &routes::contest_register(contest_id),
                &serde_json::json!({}),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "register_for_contest failed: {}", res.text);
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
            .post(self.url(&routes::attachments(problem_id)))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .expect("Failed to send attachment upload request");

        TestResponse::from_response(res).await
    }

    pub async fn upload_additional_file(
        &self,
        problem_id: i32,
        file_name: &str,
        file_bytes: Vec<u8>,
        language: &str,
        token: &str,
    ) -> TestResponse {
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str("application/octet-stream")
            .expect("Failed to set MIME type");
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("language", language.to_string());

        let res = self
            .client
            .post(self.url(&routes::additional_files(problem_id)))
            .header("Authorization", format!("Bearer {token}"))
            .multipart(form)
            .send()
            .await
            .expect("Failed to send additional file upload request");

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

    pub async fn create_user_with_role(
        &self,
        username: &str,
        password: &str,
        role: &str,
    ) -> String {
        let body = serde_json::json!({
            "username": username,
            "password": password,
        });

        let reg = self.post_without_token(routes::REGISTER, &body).await;
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
        .expect("Failed to update user role");

        txn.commit().await.expect("Failed to commit transaction");

        let res = self.post_without_token(routes::LOGIN, &body).await;
        assert_eq!(res.status, 200, "Login failed: {}", res.text);

        res.body["token"]
            .as_str()
            .expect("Login response should contain a token")
            .to_string()
    }
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
