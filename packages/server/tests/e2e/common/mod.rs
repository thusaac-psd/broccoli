use common::db::DatabaseConnection;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use common::worker::{Executor, TaskReply, TaskResult};
use plugin_core::config::PluginConfig;
use reqwest::Client;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, EntityTrait, QueryFilter, Set,
    TransactionTrait,
};
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use worker::consumer::Settlement;
use worker::models::operation::OperationTaskExecutor;
use worker::models::operation::executor::{PlatformTools, derived_tools_dir};
use worker::models::operation::sandbox::isolate::IsolateSandboxManager;
use worker::models::operation::sandbox::mock::MockSandboxManager;

use server::config::{
    AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
    MqAppConfig, ServerConfig, SubmissionConfig,
};
use server::consumers::consume_operation_results;
use server::entity::{role, role_permission, user, user_role};
use server::manager::ServerManager;
use server::registry::{
    CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
    LanguageResolverRegistry, OperationBatches, OperationWaiters,
};
use server::state::AppState;
use server::utils::plugin::sync_plugins;

static TEST_COUNTER: AtomicU32 = AtomicU32::new(0);

pub use test_support::TestResponse;
use test_support::{TestDatabase, close_database_pool, create_test_database, shared_redis_port};

/// Whether the in-process worker judges with the mock sandbox:
/// `E2E_SANDBOX_BACKEND=mock`, or no backend named on a non-Linux host. Any
/// other value selects isolate.
fn mock_sandbox() -> bool {
    match std::env::var("E2E_SANDBOX_BACKEND") {
        Ok(backend) => backend.eq_ignore_ascii_case("mock"),
        Err(_) => !cfg!(target_os = "linux"),
    }
}

/// Fails the test unless submissions are judged in a real sandbox: a running
/// stack (`E2E_SERVER_URL`), or the in-process worker on isolate. Every test
/// that compiles and runs real programs calls this first, so a misconfigured
/// run fails instead of passing vacuously. These tests are `#[ignore]`d and run
/// in the `sandbox` CI job; locally, install isolate and run
/// `E2E_SANDBOX_BACKEND=isolate cargo test -p server --test e2e -- --ignored`.
pub fn require_real_sandbox() {
    if std::env::var("E2E_SERVER_URL").is_ok() {
        return;
    }
    assert!(
        !mock_sandbox(),
        "this test judges real programs; set E2E_SANDBOX_BACKEND=isolate"
    );
    assert!(
        std::process::Command::new("isolate")
            .arg("--version")
            .status()
            .is_ok_and(|status| status.success()),
        "E2E_SANDBOX_BACKEND selects isolate, but `isolate --version` fails"
    );
}

fn create_sandbox_manager()
-> Box<dyn worker::models::operation::sandbox::SandboxManager + Send + Sync> {
    if mock_sandbox() {
        Box::new(MockSandboxManager::default())
    } else {
        Box::new(IsolateSandboxManager::new("isolate".into(), true))
    }
}

fn plugins_dir() -> PathBuf {
    test_support::require_built_plugins(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("plugins"),
    )
}

/// The in-process worker's platform tools (`broccoli-compare`, ...). Judging
/// fuses a `check` step that runs `broccoli-compare` as a
/// `MountSource::PlatformTool`, in the mock sandbox as in isolate, so a
/// worker without it fails the job. The harness builds the tools once per
/// run into `target/e2e-tools`, a target directory of their own so the build
/// does not wait on the `cargo test` running it, and finds them there by the
/// worker's own rule ([`derived_tools_dir`]).
fn worker_tools_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let target = root.join("target/e2e-tools");
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| env!("CARGO").to_string());
        let status = std::process::Command::new(cargo)
            .current_dir(&root)
            .args(["build", "--offline", "--locked", "-p", "broccoli-compare"])
            .arg("--target-dir")
            .arg(&target)
            .status()
            .expect("run cargo to build the platform tools");
        assert!(
            status.success(),
            "building broccoli-compare failed: {status}"
        );
        derived_tools_dir(&target.join("debug").join("worker"))
    })
    .clone()
}

#[allow(dead_code)]
pub struct E2eTestApp {
    pub base_url: String,
    pub client: Client,
    pub db: DatabaseConnection,
    server_handle: Option<JoinHandle<()>>,
    claim_cancel: Option<watch::Sender<bool>>,
    claim_handle: Option<JoinHandle<()>>,
    /// The in-process workers' consume loops, which stop once
    /// `worker_shutdown` is set.
    worker_handles: Vec<JoinHandle<()>>,
    _worker_heartbeats: Vec<worker::heartbeat::HeartbeatHandle>,
    worker_shutdown: Option<Arc<AtomicBool>>,
    /// Holds the in-process workers before its next read while `true` (see
    /// [`E2eTestApp::pause_worker`]). `None` against an external server.
    worker_paused: Option<watch::Sender<bool>>,
    result_consumer_handle: Option<JoinHandle<()>>,
    /// The in-process server's liveness key; dropping it releases the key
    /// and the reply stream. `None` against an external server.
    _node_heartbeat: Option<common::node_identity::NodeHeartbeat>,
    /// The in-process server's plugin cache (`plugin.cache_dir`), so a test
    /// never writes into the source tree. `None` against an external server.
    _plugin_cache: Option<tempfile::TempDir>,
    /// This server's own test database, dropped with it. `None` against an
    /// external server.
    _database: Option<TestDatabase>,
    /// The in-process server's detached judging sessions (see
    /// [`E2eTestApp::judging_idle`]). `None` against an external server.
    judging_sessions: Option<server::services::judging_sessions::JudgingSessions>,
    /// The in-process server's state (see [`E2eTestApp::reconcile_plugins`]).
    /// `None` against an external server.
    state: Option<AppState>,
    /// A contest type this server actually registers, for problem creation
    /// (which requires one): `icpc` from the real plugins, or the stub
    /// `standard` a plugin-less server registers instead.
    pub contest_type: &'static str,
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
        if let Some(shutdown) = self.worker_shutdown.take() {
            shutdown.store(true, Ordering::Relaxed);
        }
        if let Some(h) = self.result_consumer_handle.take() {
            h.abort();
        }
        close_database_pool(self.db.clone());
    }
}

/// One in-process worker as production runs it, but without a sandbox
/// preflight or metrics: the worker's own consume loop and heartbeat around
/// an executor that runs each task and replies. While `paused` is `true` a
/// task it reads waits unrun.
#[allow(clippy::too_many_arguments)]
async fn spawn_worker(
    worker_id: &str,
    mq: &mq::Mq,
    redis: &redis::Client,
    namespace: &str,
    db_url: &str,
    db: &DatabaseConnection,
    blob_store: Arc<dyn common::storage::BlobStore>,
    metrics: common::metrics::Metrics,
    tools_dir: Option<PathBuf>,
    paused: watch::Receiver<bool>,
    shutdown: Arc<AtomicBool>,
) -> (JoinHandle<()>, worker::heartbeat::HeartbeatHandle) {
    let tools = PlatformTools::new(tools_dir.clone());
    let executor = Arc::new(
        OperationTaskExecutor::new_with_sandbox_manager_and_blob_store(
            create_sandbox_manager(),
            blob_store,
            db.clone(),
            std::env::temp_dir().join(format!("broccoli-e2e-worker-cache-{worker_id}")),
            metrics,
            tools_dir,
        )
        .await
        .expect("Failed to initialize e2e operation executor"),
    );
    let clock = common::clock::PlatformClock::connect(db_url)
        .await
        .expect("read the database clock");
    let in_flight = worker::heartbeat::InFlightCounter::new();
    let started_at = clock.now();
    let heartbeat = worker::heartbeat::HeartbeatConfig {
        worker_id: worker_id.to_string(),
        sandbox_backend: "e2e".into(),
        max_concurrency: Some(1),
        fairness_mode: "unknown".into(),
        system_info: worker::system_info::SystemInfo::detect(),
        tools: tools.clone(),
        clock,
    };
    let claim = common::node_identity::NodeClaim::claim_at_boot(
        redis.clone(),
        namespace,
        common::liveness::NodeRole::Worker,
        worker_id,
        heartbeat.payload(started_at, &in_flight),
    )
    .await
    .expect("claim the e2e worker id");
    let heartbeat = worker::heartbeat::spawn(claim, heartbeat, started_at, in_flight.clone());

    let tasks = mq.task_consumer(worker_id);
    tasks
        .ensure_groups()
        .await
        .expect("create the worker groups");
    let reply_mq = mq.clone();
    let handle = tokio::spawn(worker::runtime::run_consume_loop(
        mq.clone(),
        tasks,
        1,
        tools,
        in_flight,
        shutdown,
        Duration::from_secs(1),
        move |delivery: mq::Delivery<common::worker::Task>| {
            let (executor, reply_mq, mut paused) =
                (executor.clone(), reply_mq.clone(), paused.clone());
            async move {
                if paused.wait_for(|paused| !*paused).await.is_err() {
                    return Settlement::Abandon;
                }
                let Ok(task) = delivery.payload else {
                    return Settlement::Ack;
                };
                let result = match executor.execute(task.clone()).await {
                    Ok(r) => r,
                    Err(e) => TaskResult {
                        task_id: task.id.clone(),
                        success: false,
                        output: serde_json::Value::String(e.to_string()),
                        error: Some(e.to_string()),
                        task_type: Some(task.task_type.clone()),
                        operation: Some(task.executor_name.clone()),
                        worker_id: None,
                        fault: None,
                    },
                };
                let reply = TaskReply::Completed { result };
                if let Err(e) = reply_mq.publish_reply(&task.reply_to, &reply).await {
                    tracing::warn!(error = %e, "e2e worker: publishing a reply failed");
                }
                Settlement::Ack
            }
        },
    ));
    (handle, heartbeat)
}

#[allow(dead_code)]
impl E2eTestApp {
    /// Completes once the in-process server runs no judging session: every
    /// submission it judged has finalized its judgement and run its
    /// after-judging hooks. A submission reads `Judged` before its hooks run,
    /// so a test that counts or inspects what judging did waits for this.
    pub async fn judging_idle(&self) {
        self.judging_sessions
            .as_ref()
            .expect("judging_idle needs the in-process server")
            .idle()
            .await;
    }

    /// Records every status a submission or judgement row takes from now on,
    /// in order (see [`Self::logged_statuses`]), so a test sees a transient
    /// status no poll would catch.
    pub async fn log_statuses(&self) {
        use sea_orm::ConnectionTrait;
        self.db
            .execute_unprepared(
                "CREATE TABLE status_log (seq BIGSERIAL PRIMARY KEY, tbl TEXT NOT NULL, \
                 status TEXT NOT NULL); \
                 CREATE FUNCTION log_status() RETURNS trigger LANGUAGE plpgsql AS $$ \
                 BEGIN INSERT INTO status_log (tbl, status) VALUES (TG_TABLE_NAME, NEW.status::text); \
                 RETURN NEW; END $$; \
                 CREATE TRIGGER log_submission_status AFTER INSERT OR UPDATE OF status ON submission \
                 FOR EACH ROW EXECUTE FUNCTION log_status(); \
                 CREATE TRIGGER log_judgement_status AFTER INSERT OR UPDATE OF status \
                 ON submission_judgement FOR EACH ROW EXECUTE FUNCTION log_status();",
            )
            .await
            .expect("install the status log");
    }

    /// Every `(table, status)` [`Self::log_statuses`] recorded, in order.
    pub async fn logged_statuses(&self) -> Vec<(String, String)> {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        self.db
            .query_all_raw(Statement::from_string(
                DbBackend::Postgres,
                "SELECT tbl, status FROM status_log ORDER BY seq",
            ))
            .await
            .expect("read the status log")
            .iter()
            .map(|r| {
                (
                    r.try_get("", "tbl").unwrap(),
                    r.try_get("", "status").unwrap(),
                )
            })
            .collect()
    }

    /// Converges this server onto the fleet's plugin table now, as its
    /// periodic reconcile does: what a change another replica published does
    /// here.
    pub async fn reconcile_plugins(&self) {
        let state = self
            .state
            .as_ref()
            .expect("reconcile_plugins needs the in-process server");
        let failures = server::plugin_lifecycle::reconcile(state)
            .await
            .expect("reconcile the plugin table");
        assert!(failures.is_empty(), "reconcile failed to apply a plugin");
    }

    /// Takes a sample burst on the in-process server's platform clock, as
    /// its resample timer does.
    pub async fn resample_clock(&self) {
        self.state
            .as_ref()
            .expect("resample_clock needs the in-process server")
            .clock
            .sync()
            .await
            .expect("sample the database clock");
    }

    /// Runs this server's start-up plugin sync again: what its next start
    /// does with the plugins and data it finds.
    pub async fn restart_plugins(&self) {
        let state = self
            .state
            .as_ref()
            .expect("restart_plugins needs the in-process server");
        let failures = sync_plugins(state).await.expect("sync the plugins");
        assert!(failures.is_empty(), "a plugin failed to load");
    }

    /// The problem's `config_issues` flag, as stored: what judging copies
    /// onto the judgements it claims.
    pub async fn config_issues_flag(&self, problem_id: i32) -> bool {
        use sea_orm::EntityTrait;
        server::entity::problem::Entity::find_by_id(problem_id)
            .one(&self.db)
            .await
            .unwrap()
            .expect("the problem")
            .config_issues
    }

    /// Sets the flag directly, as a stale value a recheck must correct.
    pub async fn set_config_issues_flag(&self, problem_id: i32, value: bool) {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        server::entity::problem::Entity::update_many()
            .col_expr(
                server::entity::problem::Column::ConfigIssues,
                sea_orm::sea_query::Expr::value(value),
            )
            .filter(server::entity::problem::Column::Id.eq(problem_id))
            .exec(&self.db)
            .await
            .unwrap();
    }

    /// Default app DB pool size. Small on purpose: e2e runs many isolated test
    /// databases and Postgres backend slots are finite.
    const DEFAULT_DB_MAX_CONNECTIONS: u32 = 3;

    pub async fn spawn() -> Self {
        if let Ok(server_url) = std::env::var("E2E_SERVER_URL") {
            Self::connect(&server_url).await
        } else {
            Self::spawn_internal(
                true,
                Self::DEFAULT_DB_MAX_CONNECTIONS,
                vec![worker_tools_dir()],
            )
            .await
        }
    }

    pub async fn spawn_without_plugins() -> Self {
        if let Ok(server_url) = std::env::var("E2E_SERVER_URL") {
            Self::connect(&server_url).await
        } else {
            Self::spawn_internal(
                false,
                Self::DEFAULT_DB_MAX_CONNECTIONS,
                vec![worker_tools_dir()],
            )
            .await
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
            Self::spawn_internal(true, db_max_connections, vec![worker_tools_dir()]).await
        }
    }

    /// Like [`spawn`], with one in-process worker per entry of `tools`, each
    /// reading its platform tools from that directory. In-process only.
    pub async fn spawn_with_workers(tools: Vec<Option<PathBuf>>) -> Self {
        Self::spawn_internal(true, Self::DEFAULT_DB_MAX_CONNECTIONS, tools).await
    }

    /// The platform tools directory of a healthy worker.
    pub fn platform_tools_dir() -> Option<PathBuf> {
        worker_tools_dir()
    }

    /// A fresh, empty tools directory: a worker given it lacks every
    /// platform tool.
    pub fn empty_tools_dir() -> PathBuf {
        tempfile::tempdir().expect("tools tempdir").keep()
    }

    async fn connect(server_url: &str) -> Self {
        let db_url = std::env::var("E2E_DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:password@127.0.0.1:5433/broccoli".into());

        let mut opts = ConnectOptions::new(&db_url);
        opts.max_connections(3)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(5));
        let db = DatabaseConnection::from(
            Database::connect(opts)
                .await
                .expect("Failed to connect to Docker database"),
        );

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
            worker_handles: Vec::new(),
            _worker_heartbeats: Vec::new(),
            worker_shutdown: None,
            worker_paused: None,
            result_consumer_handle: None,
            _node_heartbeat: None,
            _plugin_cache: None,
            _database: None,
            judging_sessions: None,
            state: None,
            // An external server runs with the real plugins.
            contest_type: "icpc",
        }
    }

    async fn spawn_internal(
        load_plugins: bool,
        db_max_connections: u32,
        worker_tools: Vec<Option<PathBuf>>,
    ) -> Self {
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let plugin_cache = tempfile::tempdir().expect("plugin cache tempdir");

        let db_name = format!("e2e_{test_id}");
        let (database, redis_port) =
            tokio::join!(create_test_database(&db_name), shared_redis_port());
        let db_url = database.url().to_string();
        let mut opts = ConnectOptions::new(&db_url);
        opts.max_connections(db_max_connections)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(3));
        let db = DatabaseConnection::from(
            Database::connect(opts)
                .await
                .expect("Failed to connect to test database"),
        );

        let redis_url = format!("redis://127.0.0.1:{redis_port}");

        let operation_queue = format!("e2e_{test_id}_ops");
        let server_id = format!("e2e-{test_id}");

        let mq = mq::Mq::connect(&redis_url, &operation_queue)
            .await
            .expect("Failed to initialize MQ");
        // The server's liveness key, owning its reply stream, as at boot.
        let node_heartbeat = common::node_identity::NodeClaim::claim_at_boot(
            redis::Client::open(redis_url.as_str()).expect("redis client"),
            &operation_queue,
            common::liveness::NodeRole::Server,
            &server_id,
            serde_json::json!({}),
        )
        .await
        .expect("claim the e2e server id")
        .own_stream(mq.topology().reply(&server_id), mq::REPLY_STREAM_TTL, || {})
        .spawn_heartbeat(|| serde_json::json!({}));

        // Tests create their users by registering.
        server::services::registration::set_open(&db, true)
            .await
            .expect("open self-registration");

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
                id: server_id.clone(),
                dispatcher_lease_steal_enabled: false,
                dispatcher_semaphore_enabled: false,
                dispatcher_concurrency: 1,
                dispatcher_admission_queue_max: 0,
                // Disable durable Queued-depth backpressure in e2e
                // by default; tests opt in if they need to exercise it.
                max_queued_submissions: Some(0),
                lease_ttl_secs: 60,
                lease_refresh_interval_secs: 10,
                steal_scan_interval_secs: 15,
                steal_batch_size: 8,
                max_dispatch_retries: 5,
                // No system-error retries: a SystemError stands as soon as
                // the reaper below confirms it.
                max_system_error_retries: 0,
                cancel_primitive_enabled: false,
                claim_fiber_enabled: true,
                plugin_timer_tick_interval_secs: 1,
                plugin_timer_batch: 64,
                plugin_timer_max_attempts: 5,
            },
            database: DatabaseConfig {
                url: db_url.clone(),
                ..Default::default()
            },
            auth: AuthConfig {
                jwt_secret: "e2e-test-jwt-secret".to_string(),
                login_failure_limit: 0,
                login_failure_window_secs: 60,
            },
            plugin: PluginConfig {
                plugins_dir: plugins_dir(),
                cache_dir: plugin_cache.path().to_path_buf(),
                ..Default::default()
            },
            submission: SubmissionConfig::default(),
            storage: BlobStoreConfig::default(),
            mq: MqAppConfig {
                url: redis_url.clone(),
                operation_queue_name: operation_queue.clone(),
                ..Default::default()
            },
            observability: common::config::ObservabilityConfig::default(),
            batch_max_age_secs: 600,
            bootstrap: BootstrapConfig::default(),
            unknown_keys: Vec::new(),
        };

        let contest_type_registry: ContestTypeRegistry = Default::default();
        let evaluator_registry: EvaluatorRegistry = Default::default();
        let checker_stage_registry: CheckerStageRegistry = Default::default();
        let language_resolver_registry: LanguageResolverRegistry = Default::default();
        let operation_batches = OperationBatches::default();
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches = EvaluateBatches::default();
        let hook_registry = server::hooks::new_shared_registry();

        let judging_sessions = server::services::judging_sessions::JudgingSessions::default();
        let plugin_config_cache =
            server::services::plugin_config_cache::PluginConfigCache::default();
        let worker_slots = server::dispatcher::fanout::WorkerSlots::default();
        let plugin_sql = server::plugin_db::PluginSqlPools::new(
            db.clone(),
            &db_url,
            8,
            std::time::Duration::from_secs(30),
            &server::database::app_session(&app_config.database),
        );
        let plugins = ServerManager::new(
            app_config.plugin.clone(),
            server::host_funcs::context::HostFunctionSystemDeps {
                // Plugin SQL runs as each plugin's own role, as in production;
                // the host function pool shares the app pool.
                plugin_sql: plugin_sql.clone(),
                host_db: db.clone(),
                mq: Some(mq.clone()),
                operation_batches: operation_batches.clone(),
                operation_waiters: operation_waiters.clone(),
                contest_type_registry: contest_type_registry.clone(),
                evaluator_registry: evaluator_registry.clone(),
                checker_stage_registry: checker_stage_registry.clone(),
                language_resolver_registry: language_resolver_registry.clone(),
                evaluate_batches: evaluate_batches.clone(),
                blob_store: blob_store.clone(),
                hook_registry: hook_registry.clone(),
                config: app_config.clone(),
                metrics: None,
                redis_client: None,
                worker_slots: worker_slots.clone(),
                plugin_config_cache: plugin_config_cache.clone(),
                judging_sessions: judging_sessions.clone(),
            },
            None,
        )
        .expect("Failed to initialize ServerManager");

        let (e2e_metrics, e2e_prom_registry) =
            common::observability::init_metrics("broccoli-e2e-test");

        let clock = server::clock::PlatformClock::connect(&db_url)
            .await
            .expect("read the database clock");
        let state = AppState {
            plugins,
            db: db.clone(),
            clock,
            plugin_config_cache,
            config: app_config,
            mq: mq.clone(),
            redis_client: Some(Arc::new(
                redis::Client::open(redis_url.as_str()).expect("redis client"),
            )),
            blob_store: blob_store.clone(),
            registries: server::state::RegistryState {
                contest_type_registry,
                evaluator_registry,
                checker_stage_registry,
                language_resolver_registry,
                operation_batches,
                operation_waiters: operation_waiters.clone(),
                evaluate_batches: evaluate_batches.clone(),
                hook_registry,
                judging_sessions: judging_sessions.clone(),
                live_dispatches: Default::default(),
            },
            metrics: e2e_metrics.clone(),
            prometheus_registry: e2e_prom_registry,
            dispatcher_permits: server::dispatcher::permits::DispatcherSemaphore::default(),
            worker_slots: worker_slots.clone(),
            plugin_lifecycle: Default::default(),
            plugin_sql,
            last_workers: Default::default(),
        };

        let mut result_consumer_handle_opt = None;
        let mut claim_cancel_opt = None;
        let mut claim_handle_opt = None;
        let mut worker_handles = Vec::new();
        let mut worker_heartbeats = Vec::new();
        let worker_shutdown = Arc::new(AtomicBool::new(false));
        let (worker_paused, worker_paused_rx) = watch::channel(false);

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

            result_consumer_handle_opt = Some(tokio::spawn(consume_operation_results(
                mq.reply_inbox(&server_id),
                server::consumers::operation_result::OutstandingCheck {
                    mq: mq.clone(),
                    server_id: server_id.clone(),
                    interval: server::consumers::operation_result::OUTSTANDING_CHECK_INTERVAL,
                },
                operation_waiters.clone(),
                evaluate_batches.clone(),
                e2e_metrics.clone(),
            )));

            let (claim_cancel_tx, claim_cancel_rx) = watch::channel(false);
            claim_cancel_opt = Some(claim_cancel_tx);
            claim_handle_opt = Some(tokio::spawn(server::dispatcher::claim::run(
                state.clone(),
                server_id.clone(),
                claim_cancel_rx.clone(),
            )));
            // As in production: the reaper decides every SystemError, which
            // the submission shows only once it stands.
            tokio::spawn(server::dispatcher::system_error_retry::run(
                state.clone(),
                server_id.clone(),
                0,
                claim_cancel_rx,
            ));

            let redis = redis::Client::open(redis_url.as_str()).expect("redis client");
            for (i, tools_dir) in worker_tools.into_iter().enumerate() {
                let (handle, heartbeat) = spawn_worker(
                    &format!("e2e-worker-{test_id}-{i}"),
                    &mq,
                    &redis,
                    &operation_queue,
                    &db_url,
                    &db,
                    blob_store.clone(),
                    e2e_metrics.clone(),
                    tools_dir,
                    worker_paused_rx.clone(),
                    worker_shutdown.clone(),
                )
                .await;
                worker_handles.push(handle);
                worker_heartbeats.push(heartbeat);
            }
            // The fleet as the claim fiber sees it, read once before any test
            // submits and then kept current as in production.
            worker_slots
                .refresh(
                    &redis,
                    &operation_queue,
                    server::dispatcher::fanout::SLOTS_REFRESH,
                )
                .await;
            worker_slots.spawn_refresh(Arc::new(redis.clone()), operation_queue.clone());
            tokio::spawn(
                server::consumers::operation_result::hand_back_while_none_accepting(
                    worker_slots.clone(),
                    operation_waiters.clone(),
                    Some(Arc::new(redis)),
                ),
            );
        } else {
            state.registries.evaluator_registry.write().await.insert(
                "batch".into(),
                server::registry::EvaluatorHandler {
                    plugin_id: "__test__".into(),
                    functions: server::registry::EvaluatorFunctions::Direct {
                        function: "noop".into(),
                    },
                },
            );
            {
                let mut stage = state.registries.checker_stage_registry.write().await;
                for fmt in ["exact", "lines", "none"] {
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
                    submission_admission: false,
                },
            );
        }

        let app = server::build_router(state.clone()).expect("router");
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
            worker_handles,
            _worker_heartbeats: worker_heartbeats,
            worker_shutdown: Some(worker_shutdown),
            worker_paused: Some(worker_paused),
            result_consumer_handle: result_consumer_handle_opt,
            _node_heartbeat: Some(node_heartbeat),
            _plugin_cache: Some(plugin_cache),
            _database: Some(database),
            judging_sessions: Some(judging_sessions),
            state: Some(state),
            contest_type: if load_plugins { "icpc" } else { "standard" },
        }
    }

    /// Stop the in-process worker from running tasks until
    /// [`Self::resume_worker`]: every task it reads meanwhile waits unrun.
    pub fn pause_worker(&self) {
        self.worker_paused
            .as_ref()
            .expect("pausing needs the in-process worker")
            .send_replace(true);
    }

    pub fn resume_worker(&self) {
        self.worker_paused
            .as_ref()
            .expect("resuming needs the in-process worker")
            .send_replace(false);
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

    /// Creates a user and a brand-new role carrying EXACTLY the given
    /// permissions - not one of the seeded `DEFAULT_MAPPINGS` roles ("admin",
    /// "problem_setter", "contestant"), which each bundle several permissions
    /// together. Use this when a test needs to isolate ONE permission's
    /// effect from another that a stock role would always grant alongside it
    /// (e.g. `admin` holds both `contest:manage` and `submission:view_all`,
    /// so it cannot tell apart which one a given bypass actually checks).
    pub async fn create_user_with_permissions(
        &self,
        username: &str,
        password: &str,
        permissions: &[&str],
    ) -> String {
        let role_name = format!("{username}_role");
        role::ActiveModel {
            name: Set(role_name.clone()),
        }
        .insert(&self.db)
        .await
        .expect("Failed to insert custom role");

        for permission in permissions {
            role_permission::ActiveModel {
                role: Set(role_name.clone()),
                permission: Set(permission.to_string()),
            }
            .insert(&self.db)
            .await
            .expect("Failed to insert custom role permission");
        }

        self.create_user_with_role(username, password, &role_name)
            .await
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
                    "default_contest_type": self.contest_type,
                    "checker_format": checker_format,
                    "is_public": is_public,
                }),
                token,
            )
            .await;
        assert_eq!(res.status, 201, "create_problem failed: {}", res.text);
        res.id()
    }

    /// A sample test case for a sum problem: input `5\n1 2 3 4 5`, answer
    /// `15\n`. The answer is what a correct solution prints, final newline
    /// included, because [`Self::create_problem`] judges with the byte-exact
    /// `exact` checker.
    pub async fn create_test_case(&self, problem_id: i32, token: &str) -> i32 {
        self.create_test_case_with(problem_id, "5\n1 2 3 4 5", "15\n", 10, true, token)
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
                    "contest_type": self.contest_type,
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
