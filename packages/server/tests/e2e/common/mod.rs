use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use common::worker::{Executor, TaskReply, TaskResult};
use plugin_core::config::PluginConfig;
use reqwest::Client;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait,
    QueryFilter, Set, TransactionTrait,
};
use serde_json::Value;
use tokio::sync::{RwLock, watch};
use tokio::task::JoinHandle;
use worker::models::operation::OperationTaskExecutor;
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
use test_support::{close_database_pool, create_test_database, shared_redis_port};

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

/// Worker platform-tools dir (`broccoli-compare`, ...) for real-sandbox judging.
/// Judging fuses a `check` step that mounts `broccoli-compare` as a
/// `MountSource::PlatformTool`, so a real-isolate run rejects the job without
/// it. Resolution: `BROCCOLI_WORKER_TOOLS_DIR` if set, else the conventional
/// deployment path when present. `None` for mock-sandbox runs, which never
/// resolve a platform tool.
fn worker_tools_dir() -> Option<PathBuf> {
    if mock_sandbox() {
        return None;
    }
    if let Ok(dir) = std::env::var("BROCCOLI_WORKER_TOOLS_DIR") {
        return Some(PathBuf::from(dir));
    }
    let default = PathBuf::from("/opt/broccoli/tools");
    default.is_dir().then_some(default)
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
    /// The in-process server's liveness key; dropping it releases the key
    /// and the reply stream. `None` against an external server.
    _node_heartbeat: Option<common::node_identity::NodeHeartbeat>,
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
        if let Some(h) = self.worker_handle.take() {
            h.abort();
        }
        if let Some(h) = self.result_consumer_handle.take() {
            h.abort();
        }
        close_database_pool(self.db.clone());
    }
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
            _node_heartbeat: None,
            // An external server runs with the real plugins.
            contest_type: "icpc",
        }
    }

    async fn spawn_internal(load_plugins: bool, db_max_connections: u32) -> Self {
        let test_id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);

        let db_name = format!("e2e_{test_id}");
        let (db_url, redis_port) =
            tokio::join!(create_test_database(&db_name), shared_redis_port());
        let mut opts = ConnectOptions::new(&db_url);
        opts.max_connections(db_max_connections)
            .min_connections(0)
            .idle_timeout(Duration::from_secs(3));
        let db = Database::connect(opts)
            .await
            .expect("Failed to connect to test database");

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
                id: server_id.clone(),
                expects_multi_replica: false,
                dispatcher_lease_steal_enabled: false,
                dispatcher_semaphore_enabled: false,
                dispatcher_concurrency: 1,
                dispatcher_admission_queue_max: 0,
                // Disable durable Queued-depth backpressure in e2e
                // by default; tests opt in if they need to exercise it.
                max_queued_submissions: 0,
                lease_ttl_secs: 60,
                lease_refresh_interval_secs: 10,
                steal_scan_interval_secs: 15,
                steal_batch_size: 8,
                max_dispatch_retries: 5,
                max_system_error_retries: 50,
                cancel_primitive_enabled: false,
                max_blocking_threads: None,
                evaluate_batch_fanout_concurrency: 64,
                operation_batch_publish_concurrency: 32,
                claim_fiber_enabled: true,
                claim_poll_interval_ms: 100,
                claim_batch_size: 32,
                max_in_flight_submissions: 256,
                plugin_timer_tick_interval_secs: 1,
                plugin_timer_lease_secs: server::config::DEFAULT_PLUGIN_TIMER_LEASE_SECS,
                plugin_timer_batch: 64,
                plugin_timer_max_attempts: 5,
            },
            database: DatabaseConfig {
                url: db_url.clone(),
                max_connections: db_max_connections,
                plugin_max_connections: 1,
                plugin_privileged_max_connections: 1,
                plugin_url: None,
                ..Default::default()
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
                operation_queue_name: operation_queue.clone(),
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
        let operation_batches = OperationBatches::default();
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches = EvaluateBatches::default();
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
            },
            None,
        )
        .expect("Failed to initialize ServerManager");

        let (e2e_metrics, e2e_prom_registry) =
            common::observability::init_metrics("broccoli-e2e-test");

        let clock = server::clock::PlatformClock::connect(&db)
            .await
            .expect("read the database clock");
        let state = AppState {
            plugins,
            db: db.clone(),
            clock,
            config: app_config,
            mq: Some(mq.clone()),
            redis_client: None,
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
            },
            metrics: e2e_metrics.clone(),
            prometheus_registry: e2e_prom_registry,
            dispatcher_permits: server::dispatcher::permits::DispatcherSemaphore::default(),
            plugin_lifecycle: Default::default(),
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

            result_consumer_handle_opt = Some(tokio::spawn(consume_operation_results(
                mq.reply_inbox(&server_id),
                operation_waiters.clone(),
                evaluate_batches.clone(),
                e2e_metrics.clone(),
            )));

            let (claim_cancel_tx, claim_cancel_rx) = watch::channel(false);
            claim_cancel_opt = Some(claim_cancel_tx);
            claim_handle_opt = Some(tokio::spawn(server::dispatcher::claim::run(
                state.clone(),
                server_id.clone(),
                state.config.server.claim_poll_interval_ms,
                state.config.server.claim_batch_size,
                state.config.server.max_in_flight_submissions,
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
            let tasks = mq.task_consumer(&format!("e2e-worker-{test_id}"));
            tasks
                .ensure_groups()
                .await
                .expect("create the worker groups");
            let worker_mq = mq.clone();
            // A minimal worker: run each task, reply, acknowledge.
            worker_handle_opt = Some(tokio::spawn(async move {
                let mut reader = tasks.reader();
                loop {
                    let Ok(batch) = reader.read(8).await else {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    };
                    for delivery in batch {
                        let Ok(task) = delivery.payload else {
                            let _ = tasks.ack(&delivery.entry).await;
                            continue;
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
                            },
                        };
                        let reply = TaskReply::Completed { result };
                        if let Err(e) = worker_mq.publish_reply(&task.reply_to, &reply).await {
                            tracing::warn!(error = %e, "e2e worker: publishing a reply failed");
                        }
                        let _ = tasks.ack(&delivery.entry).await;
                    }
                }
            }));
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
                    submission_admission: false,
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
            _node_heartbeat: Some(node_heartbeat),
            contest_type: if load_plugins { "icpc" } else { "standard" },
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
