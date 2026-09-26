use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::http::{HeaderName, HeaderValue, Method};
use common::storage::config::create_blob_store;
use dashmap::DashMap;
use mq::{MqConfig as MqConnConfig, init_mq};
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tracing::{info, warn};

use crate::build_router;
use crate::config::{AppConfig, per_replica_result_queue_name, resolve_server_id};
use crate::consumers::{consume_operation_dlq, consume_operation_results};
use crate::dlq::run_stuck_job_detector;
use crate::healthz_runtime::spawn_healthz_runtime;
use crate::host_funcs::context::HostFunctionSystemDeps;
use crate::manager::ServerManager;
use crate::registry;
use crate::serve::serve_with_shutdown;
use crate::state::AppState;
use crate::utils::plugin::sync_plugins;

pub struct ServerRuntime {
    listener: tokio::net::TcpListener,
    app: axum::Router,
    addr: SocketAddr,
    dispatcher: crate::dispatcher::Dispatcher,
    /// Handle to the dedicated `/healthz` + `/metrics` OS thread spawned in
    /// [`crate::healthz_runtime`] (UP#14e). `None` when `server.healthz_listen`
    /// is unset. The thread runs for the process lifetime and is reaped by
    /// the OS on exit, so we hold the handle for ownership but never `join()`
    /// it - hence the underscore prefix to silence dead-code warnings.
    _healthz_handle: Option<std::thread::JoinHandle<()>>,
    _telemetry_guard: common::observability::TelemetryGuard,
}

impl ServerRuntime {
    pub async fn build(mut app_config: AppConfig) -> anyhow::Result<Self> {
        let telemetry_guard = common::observability::init_tracing(&app_config.observability);

        let server_id = resolve_server_id(
            &app_config.server.id,
            app_config.server.expects_multi_replica,
        )?;
        app_config.server.id = server_id.clone();

        let original_pool_max = app_config.plugin.pool_max_instances;
        if app_config.plugin.normalize_for_safe_pool_sizing() {
            let evaluator_parallelism = app_config.plugin.resolve_evaluator_parallelism();
            warn!(
                configured_pool_max_instances = original_pool_max,
                evaluator_parallelism,
                effective_pool_max_instances = app_config.plugin.pool_max_instances,
                "pool_max_instances < evaluator_parallelism; raising pool size to match. \
                 This prevents pool-acquire queueing from inflating the evaluator semaphore \
                 (and outright deadlock for self-checking plugins). Set \
                 BROCCOLI__PLUGIN__POOL_MAX_INSTANCES explicitly to silence this warning."
            );
        }

        let operation_result_queue_base = app_config.mq.operation_result_queue_name.clone();
        let per_replica_op_result_queue =
            per_replica_result_queue_name(&operation_result_queue_base, &server_id);
        app_config.mq.operation_result_queue_name = per_replica_op_result_queue.clone();
        info!(
            server_id = %server_id,
            result_queue = %per_replica_op_result_queue,
            "Resolved server identity"
        );

        let (metrics, prometheus_registry) =
            common::observability::init_metrics(&app_config.observability.otlp.service_name);

        let db = crate::database::init_db_with_max_connections(
            &app_config.database.url,
            app_config.database.max_connections,
        )
        .await?;
        // Dedicated pool for plugin host-function DB calls, isolated from the
        // main pool above (handlers/dispatcher/windowed-eval). Prevents
        // connection-protocol desync from other operations corrupting clean
        // plugin INSERTs as spurious "0x00" errors. See database.rs::init_plugin_db.
        // The RESTRICTED plugin pool must NOT reuse the privileged app login role:
        // a plugin that escapes the after-connect `SET ROLE broccoli_plugin` via
        // `RESET ROLE` would otherwise climb back to the app role (and on a pooled
        // connection that persists into the next call). `resolve_restricted_plugin_pool`
        // points it at a DBA-managed `plugin_url` or an auto-provisioned
        // `broccoli_plugin_login`, degrading to app-role auth + the SQL text guard
        // if neither is available. It returns the fresh-connection fallback target
        // so the 0x00 fallback authenticates the same way the pool did.
        let restricted = crate::database::resolve_restricted_plugin_pool(
            &db,
            &app_config.database.url,
            app_config.database.plugin_url.as_deref(),
            app_config.database.plugin_max_connections,
        )
        .await?;
        let plugin_db = restricted.pool;
        let restricted_fb_url = restricted.fallback_url;
        let restricted_fb_login = restricted.fallback_login;
        // Fail CLOSED on the degrade path: when the restricted pool could NOT be
        // put behind a least-privilege login (no operator `plugin_url`, and the
        // auto-provisioned `broccoli_plugin_login` could not be created/connected),
        // it authenticates as the privileged APP role. On that path the SQL text
        // guard is the SOLE barrier against a role escape — and a text guard on a
        // hand-rolled lexer is best-effort defense-in-depth, not a sound boundary.
        // Rather than run raw plugin `host.db.*` SQL as the app role, DISABLE the
        // raw SQL capability outright (structured `host.submission/storage/config`
        // fns are unaffected — they intentionally use the privileged pool). Operator
        // must grant the app role CREATEROLE or set `database.plugin_url` to restore it.
        if restricted.auth == crate::database::RestrictedPluginAuth::AppRoleDegraded {
            warn!(
                "Restricted plugin DB pool degraded to the privileged application role \
                 (no database.plugin_url, and broccoli_plugin_login could not be \
                 provisioned). DISABLING raw plugin SQL (host.db.*) to avoid executing \
                 plugin SQL as the privileged role; the SQL text guard alone is not a \
                 sound privilege boundary. Grant the application role CREATEROLE or set \
                 database.plugin_url to a dedicated non-privileged role to re-enable it. \
                 Structured host functions (submission/storage/config) are unaffected."
            );
            crate::host_funcs::sql::disable_raw_plugin_sql();
        }
        // Second, PRIVILEGED plugin pool (same URL, no `SET ROLE`), used only by
        // the gated core-WRITE host fns (`host.submission.*`, `host.storage.*`,
        // `config:write`). The `plugin_db` above is restricted to the read-only
        // `broccoli_plugin` role for the raw `sql` capability (phase 2).
        let privileged_plugin_db = crate::database::init_plugin_db_privileged(
            &app_config.database.url,
            app_config.database.plugin_privileged_max_connections,
        )
        .await?;
        info!(
            plugin_max_connections = app_config.database.plugin_max_connections,
            plugin_privileged_max_connections =
                app_config.database.plugin_privileged_max_connections,
            "Initialized isolated plugin host-function DB pools (restricted sql + privileged writes)"
        );
        // URLs for the guaranteed fresh-connection fallback when pool retries are
        // exhausted by poisoned-connection 0x00 errors. The privileged fallback
        // (structured `host.submission.*` writes) uses the app role; the
        // RESTRICTED fallback (raw `sql`) uses whatever auth the restricted pool
        // ended up on, so an escape that survives the SQL text guard lands on a
        // non-privileged role on the fallback path too, not just the pool.
        crate::host_funcs::sql::set_plugin_db_fallback_url(app_config.database.url.clone());
        crate::host_funcs::sql::set_plugin_db_restricted_fallback(
            restricted_fb_url,
            restricted_fb_login,
        );
        let blob_store = create_blob_store(&app_config.storage, db.clone(), Some(metrics.clone()))
            .await
            .context("Failed to initialize blob storage")?;
        info!(
            "Blob storage initialized (backend: {})",
            app_config.storage.backend
        );

        crate::seed::seed_role_permissions(&db).await?;
        crate::seed::ensure_bootstrap_admin(
            &db,
            &app_config.bootstrap.admin_username,
            &app_config.bootstrap.admin_password,
        )
        .await?;
        crate::seed::ensure_indexes(&db).await?;
        crate::seed::backfill_submission_judgements(&db).await?;
        crate::seed::spawn_large_test_case_body_backfill(db.clone(), Arc::clone(&blob_store));

        let mq = if app_config.mq.enabled {
            match init_mq(MqConnConfig {
                url: app_config.mq.url.clone(),
                pool_size: app_config.mq.pool_size,
            })
            .await
            {
                Ok(queue) => {
                    info!("MQ connected to {}", app_config.mq.url);
                    Some(Arc::new(queue))
                }
                Err(e) => {
                    warn!("MQ connection failed, submissions won't be queued: {}", e);
                    None
                }
            }
        } else {
            info!("MQ disabled by configuration");
            None
        };

        let redis_client = if app_config.mq.enabled {
            match redis::Client::open(app_config.mq.url.as_str()) {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    warn!(
                        "Redis admin client init failed, /admin/system endpoints will degrade: {}",
                        e
                    );
                    None
                }
            }
        } else {
            None
        };

        if let Some(ref mq_arc) = mq {
            let op_dlq_consumer_db = db.clone();
            let op_dlq_consumer_mq = Arc::clone(mq_arc);
            let op_dlq_queue = app_config.mq.operation_dlq_queue_name.clone();
            tokio::spawn(async move {
                consume_operation_dlq(op_dlq_consumer_db, op_dlq_consumer_mq, op_dlq_queue).await;
            });
            info!("Operation DLQ consumer started");
        }

        // NOTE: the stuck job detector is spawned later (after AppState is
        // built) so it can re-dispatch stuck rows via the same dispatcher
        // entry as the lease/steal scanner. See UP#5.

        let contest_type_registry = Arc::new(RwLock::new(HashMap::new()));
        let evaluator_registry = Arc::new(RwLock::new(HashMap::new()));
        let checker_stage_registry = Arc::new(RwLock::new(HashMap::new()));
        let language_resolver_registry = Arc::new(RwLock::new(HashMap::new()));
        let operation_batches = Arc::new(DashMap::new());
        let operation_waiters = Arc::new(DashMap::new());
        let evaluate_batches = Arc::new(DashMap::new());
        let evaluate_ops_registry =
            crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry::default();

        let batch_max_age = Duration::from_secs(app_config.batch_max_age_secs);
        let reaper_mq = mq.clone();
        let reaper_op_dlq_queue = app_config.mq.operation_dlq_queue_name.clone();
        let operation_waiters_for_reaper = operation_waiters.clone();
        registry::spawn_batch_reaper(
            "operation",
            operation_batches.clone(),
            batch_max_age,
            Some(metrics.clone()),
            move |_batch_id, batch| {
                for key in batch.cleanup_keys.iter() {
                    operation_waiters_for_reaper.remove(key);

                    if let Some(ref mq) = reaper_mq {
                        let mq = Arc::clone(mq);
                        let queue = reaper_op_dlq_queue.clone();
                        let key = key.clone();
                        tokio::spawn(async move {
                            let envelope = common::DlqEnvelope {
                                message_id: key.clone(),
                                message_type: common::DlqMessageType::OperationTask,
                                submission_id: None,
                                payload: serde_json::json!({ "task_id": key }),
                                error_code: common::DlqErrorCode::StuckJob,
                                error_message: "Operation batch timed out".into(),
                                retry_history: vec![],
                            };
                            if let Err(e) = mq.publish(&queue, None, &envelope, None).await {
                                tracing::error!(%key, error = %e, "Failed to publish stale op to DLQ");
                            }
                        });
                    }
                }
            },
        );
        let evaluate_ops_registry_for_reaper = evaluate_ops_registry.clone();
        registry::spawn_batch_reaper(
            "evaluate",
            evaluate_batches.clone(),
            batch_max_age,
            Some(metrics.clone()),
            move |batch_id, _batch| {
                evaluate_ops_registry_for_reaper.remove_batch(batch_id);
            },
        );

        if let Some(ref mq_arc) = mq {
            let op_consumer_mq = Arc::clone(mq_arc);
            let op_result_queue = app_config.mq.operation_result_queue_name.clone();
            let op_waiters = operation_waiters.clone();
            let op_evaluate_ops_registry = evaluate_ops_registry.clone();
            let op_result_metrics = metrics.clone();
            let op_result_poll =
                std::time::Duration::from_millis(app_config.mq.result_poll_interval_ms);
            tokio::spawn(async move {
                consume_operation_results(
                    op_consumer_mq,
                    op_waiters,
                    op_evaluate_ops_registry,
                    op_result_queue,
                    op_result_metrics,
                    op_result_poll,
                )
                .await;
            });
            info!(
                queue = %app_config.mq.operation_result_queue_name,
                "Operation result consumer started (per-replica)"
            );
        }

        let hook_registry = crate::hooks::new_shared_registry();

        let manager = ServerManager::new(
            app_config.plugin.clone(),
            HostFunctionSystemDeps {
                db: plugin_db,
                privileged_db: privileged_plugin_db,
                mq: mq.clone(),
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
                metrics: Some(metrics.clone()),
                redis_client: redis_client.clone(),
            },
            Some(metrics.clone()),
        )
        .context("Failed to initialize plugin manager")?;

        let device_codes: crate::state::DeviceCodeStore = Arc::new(DashMap::new());

        {
            let codes = device_codes.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(60));
                loop {
                    interval.tick().await;
                    let now = std::time::Instant::now();
                    codes.retain(|_code, entry| entry.expires_at > now);
                }
            });
        }

        {
            let cleanup_db = db.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    interval.tick().await;
                    crate::middleware::idempotency::cleanup_expired_keys(&cleanup_db).await;
                }
            });
        }

        let dispatcher_permits = crate::dispatcher::permits::DispatcherSemaphore::new(
            app_config.server.dispatcher_semaphore_enabled,
            app_config.server.dispatcher_concurrency as usize,
            app_config.server.dispatcher_admission_queue_max as usize,
        );

        let state = AppState {
            plugins: manager,
            db: db.clone(),
            config: app_config.clone(),
            mq: mq.clone(),
            redis_client: redis_client.clone(),
            blob_store,
            registries: crate::state::RegistryState {
                contest_type_registry: contest_type_registry.clone(),
                evaluator_registry: evaluator_registry.clone(),
                checker_stage_registry: checker_stage_registry.clone(),
                language_resolver_registry: language_resolver_registry.clone(),
                operation_batches: operation_batches.clone(),
                operation_waiters: operation_waiters.clone(),
                evaluate_batches: evaluate_batches.clone(),
                hook_registry,
            },
            device_codes,
            metrics,
            prometheus_registry,
            dispatcher_permits,
            login_throttle: Arc::new(crate::utils::login_throttle::LoginThrottle::new(
                app_config.auth.login_failure_limit,
                Duration::from_secs(app_config.auth.login_failure_window_secs),
            )),
        };

        crate::handlers::system::spawn_queue_depth_sampler(state.clone(), Duration::from_secs(5));

        {
            let detector_state = state.clone();
            let detector_config = app_config.mq.dlq.clone();
            tokio::spawn(async move {
                run_stuck_job_detector(detector_state, detector_config).await;
            });
            info!("Stuck job detector started");
        }

        let _failures = sync_plugins(&state).await?;

        // Boot-time orphan recovery: a restart leaves this server's in-flight
        // submissions/judgements/code-runs tagged with our own `server_id` and a
        // fresh lease the (now-dead) driver can no longer honor. The lease fiber
        // would keep refreshing those leases, so the steal sweeper could never
        // reclaim them and the work would hang in `Running` forever. Release them
        // BEFORE spawning the dispatcher so the steal sweeper's first scan picks
        // them up. Best-effort: a failure here must not block startup. Gated on
        // lease/steal being enabled - without the sweeper nothing would reclaim
        // the released rows.
        if app_config.server.dispatcher_lease_steal_enabled {
            match crate::dispatcher::steal::recover_orphaned_leases(&db, &server_id).await {
                Ok((subs, code_runs, judgements)) => {
                    if subs > 0 || code_runs > 0 || judgements > 0 {
                        info!(
                            server_id = %server_id,
                            submissions = subs,
                            code_runs,
                            judgements,
                            "Startup recovery released own orphaned in-flight leases for steal"
                        );
                    } else {
                        info!(server_id = %server_id, "Startup recovery: no orphaned leases to release");
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        server_id = %server_id,
                        error = %e,
                        "Startup orphan-lease recovery failed; in-flight work owned by a prior \
                         instance of this server_id may hang until manually reclaimed"
                    );
                }
            }
        }

        let dispatcher = crate::dispatcher::Dispatcher::spawn(crate::dispatcher::DispatcherDeps {
            state: state.clone(),
            redis_client: redis_client.as_deref().cloned(),
            server_id: server_id.clone(),
            operation_result_queue_base,
            config: app_config.server.clone(),
        });

        let mut allow_origins = Vec::new();
        for origin in &app_config.server.cors.allow_origins {
            allow_origins.push(
                origin
                    .parse::<HeaderValue>()
                    .with_context(|| format!("Invalid CORS origin: {}", origin))?,
            );
        }

        let healthz_state = state.clone();
        let app = build_router(state).layer(
            CorsLayer::new()
                .allow_origin(allow_origins)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::PATCH,
                    Method::DELETE,
                ])
                .allow_headers([
                    HeaderName::from_static("content-type"),
                    HeaderName::from_static("authorization"),
                    HeaderName::from_static("idempotency-key"),
                    HeaderName::from_static("x-request-id"),
                    HeaderName::from_static("x-broccoli-stress-run-id"),
                ])
                .allow_credentials(true)
                .max_age(Duration::from_secs(app_config.server.cors.max_age)),
        );

        let addr_str = format!("{}:{}", app_config.server.host, app_config.server.port);
        let addr: SocketAddr = addr_str
            .parse()
            .with_context(|| format!("Invalid server address: {}", addr_str))?;

        info!("Server running at http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("Failed to bind to {}", addr))?;

        // UP#14e: optional dedicated runtime + listener for /healthz + /metrics.
        // When configured, probe traffic survives main-runtime saturation.
        let healthz_handle = if let Some(ref addr_str) = app_config.server.healthz_listen {
            let healthz_addr: SocketAddr = addr_str
                .parse()
                .with_context(|| format!("Invalid server.healthz_listen address: {}", addr_str))?;
            let worker_threads = app_config.server.healthz_worker_threads.max(1) as usize;
            Some(spawn_healthz_runtime(
                healthz_addr,
                worker_threads,
                healthz_state,
            )?)
        } else {
            None
        };

        Ok(Self {
            listener,
            app,
            addr,
            dispatcher,
            _healthz_handle: healthz_handle,
            _telemetry_guard: telemetry_guard,
        })
    }

    pub async fn serve(mut self) -> anyhow::Result<()> {
        let serve_result = serve_with_shutdown(self.listener, self.app)
            .await
            .with_context(|| format!("Server runtime error at http://{}", self.addr));
        self.dispatcher.shutdown().await;
        serve_result
    }
}
