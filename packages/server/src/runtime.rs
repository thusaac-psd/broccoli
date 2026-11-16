use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::http::{HeaderName, HeaderValue, Method, header};
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
use crate::host_funcs::context::HostFunctionSystemDeps;
use crate::manager::ServerManager;
use crate::registry;
use crate::serve::serve_with_shutdown;
use crate::state::AppState;
use crate::utils::plugin::sync_plugins;
use common::node_identity::{NodeClaim, NodeHeartbeat, NodeRole};

pub struct ServerRuntime {
    listener: tokio::net::TcpListener,
    app: axum::Router,
    addr: SocketAddr,
    dispatcher: crate::dispatcher::Dispatcher,
    /// To release this server's in-flight work after the dispatcher stops.
    db: sea_orm::DatabaseConnection,
    server_id: String,
    /// For shutdown: refusing and draining plugin calls, stopping the plugin
    /// lifecycle before its replica row is retired.
    state: AppState,
    /// Keeps this replica's ID claim / heartbeat alive; released in [`Self::serve`].
    node_heartbeat: Option<NodeHeartbeat>,
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

        let db_session = crate::database::app_session(&app_config.database);
        // Boot-time schema work runs on one session holding a cluster-wide
        // advisory lock, so replicas booting together serialize instead of
        // racing each other's DDL and check-then-insert seeding.
        let boot = crate::database::BootSession::acquire(&app_config.database.url).await?;
        boot.migrate().await?;
        crate::seed::seed_role_permissions(boot.conn()).await?;
        crate::seed::ensure_bootstrap_admin(boot.conn(), &app_config.bootstrap).await?;

        let db = crate::database::connect_main_pool(
            &app_config.database.url,
            app_config.database.max_connections,
            &db_session,
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
        // Provisioning the dedicated plugin login (CREATE ROLE, the shared
        // secret row) is boot-time DDL too, so it runs on the locked session.
        let restricted = crate::database::resolve_restricted_plugin_pool(
            boot.conn(),
            &app_config.database.url,
            app_config.database.plugin_url.as_deref(),
            app_config.database.plugin_max_connections,
            &db_session,
        )
        .await?;
        boot.release().await?;
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
            &db_session,
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
        crate::host_funcs::sql::set_plugin_db_fallback_url(
            app_config.database.url.clone(),
            db_session.clone(),
        );
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

        let mq = if app_config.mq.enabled {
            match init_mq(MqConnConfig {
                url: app_config.mq.url.clone(),
                pool_size: app_config.mq.pool_size,
            })
            .await
            {
                Ok(queue) => {
                    info!(
                        "MQ connected to {}",
                        common::redact::redact_url(&app_config.mq.url)
                    );
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

        common::clock::check_clock_skew(&db, redis_client.as_deref(), &app_config.clock).await?;

        // Claim the server ID before consuming this replica's result queue or
        // recovering leases under it: a second live replica with the same ID
        // would eat its results and release its in-flight rows, so it refuses
        // to start instead. The claim is the `broccoli:server:heartbeat:<id>`
        // key the reply-queue sweeper reads; released on graceful shutdown.
        let node_heartbeat = match redis_client.as_deref() {
            Some(client) => {
                let started_at = chrono::Utc::now();
                let payload = {
                    let server_id = server_id.clone();
                    let hostname = hostname::get()
                        .ok()
                        .map(|h| h.to_string_lossy().into_owned());
                    move || {
                        serde_json::json!({
                            "id": server_id,
                            "started_at": started_at,
                            "last_seen": chrono::Utc::now(),
                            "version": env!("CARGO_PKG_VERSION"),
                            "hostname": hostname,
                            "pid": std::process::id(),
                        })
                    }
                };
                let claim = NodeClaim::claim_at_boot(
                    client.clone(),
                    NodeRole::Server,
                    &server_id,
                    payload(),
                )
                .await?;
                Some(claim.spawn_heartbeat(payload))
            }
            None => None,
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
        // entry as the lease/steal scanner.

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

        {
            let cleanup_db = db.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(3600));
                loop {
                    interval.tick().await;
                    common::deadline::bounded(
                        "idempotency.cleanup",
                        common::deadline::BACKGROUND_STEP,
                        crate::middleware::idempotency::cleanup_expired_keys(&cleanup_db),
                    )
                    .await;
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
            metrics,
            prometheus_registry,
            dispatcher_permits,
            plugin_lifecycle: Default::default(),
            login_throttle: Arc::new(crate::utils::login_throttle::LoginThrottle::new(
                app_config.auth.login_failure_limit,
                Duration::from_secs(app_config.auth.login_failure_window_secs),
            )),
        };

        crate::handlers::system::spawn_queue_depth_sampler(state.clone(), Duration::from_secs(5));

        if let Some(idle_for) = app_config.plugin.pool_idle_timeout() {
            let plugins = state.plugins.clone();
            tokio::spawn(async move {
                // Sweeping at a fraction of the timeout bounds how long past
                // it an idle instance can linger.
                let mut interval =
                    tokio::time::interval((idle_for / 4).max(Duration::from_secs(1)));
                loop {
                    interval.tick().await;
                    let plugins = plugins.clone();
                    let _ =
                        tokio::task::spawn_blocking(move || plugins.evict_idle_instances(idle_for))
                            .await;
                }
            });
        }

        {
            let detector_state = state.clone();
            let detector_config = app_config.mq.dlq.clone();
            tokio::spawn(async move {
                run_stuck_job_detector(detector_state, detector_config).await;
            });
            info!("Stuck job detector started");
        }

        let _failures = sync_plugins(&state).await?;
        // Converge onto plugin lifecycle changes made through any replica,
        // and retry plugins that failed to load here.
        tokio::spawn(crate::plugin_lifecycle::run(state.clone()));

        // Boot-time orphan recovery: a restart leaves this server's in-flight
        // submissions/judgements/code-runs tagged with our own `server_id`,
        // but the drivers judging them died with the old process. Release
        // them BEFORE spawning the dispatcher, whose first reclaim scan then
        // re-dispatches them: the steal sweeper with lease/steal on, the
        // released-work scan with it off. Releasing also stops them from
        // counting against this server's in-flight budget. Best-effort: a
        // failure here must not block startup.
        release_own_inflight(&db, &server_id, "Startup recovery").await;

        let dispatcher = crate::dispatcher::Dispatcher::spawn(crate::dispatcher::DispatcherDeps {
            state: state.clone(),
            redis_client: redis_client.as_deref().cloned(),
            server_id: server_id.clone(),
            operation_result_queue_base,
            config: app_config.server.clone(),
        });

        let shutdown_state = state.clone();
        let app = build_router(state).layer(cors_layer(&app_config.server.cors)?);

        let addr_str = format!("{}:{}", app_config.server.host, app_config.server.port);
        let addr: SocketAddr = addr_str
            .parse()
            .with_context(|| format!("Invalid server address: {}", addr_str))?;

        info!("Server running at http://{}", addr);

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("Failed to bind to {}", addr))?;

        Ok(Self {
            listener,
            app,
            addr,
            dispatcher,
            db,
            server_id,
            state: shutdown_state,
            node_heartbeat,
            _telemetry_guard: telemetry_guard,
        })
    }

    pub async fn serve(mut self) -> anyhow::Result<()> {
        let serve_result = serve_with_shutdown(self.listener, self.app)
            .await
            .with_context(|| format!("Server runtime error at http://{}", self.addr));
        self.dispatcher.shutdown().await;
        // From here on, plugin calls are refused (a clean error, not a call
        // that outlives the runtime).
        let gate = self.state.plugins.call_gate().cloned();
        if let Some(gate) = &gate {
            gate.close();
        }
        // The drivers of this server's in-flight work die with the process:
        // hand the work to the rest of the fleet now instead of leaving it
        // owned by a server that is gone.
        release_own_inflight(&self.db, &self.server_id, "Shutdown").await;
        crate::plugin_lifecycle::stop(&self.state).await;
        if let Err(e) = crate::plugin_fleet::retire(&self.db, &self.server_id).await {
            warn!(error = %e, "Shutdown: failed to remove this replica's plugin report");
        }
        // Calls already running bridge back into this runtime from their
        // blocking threads; let them finish before it is torn down.
        if let Some(gate) = gate
            && !gate.drain(PLUGIN_CALL_DRAIN).await
        {
            warn!(
                in_flight = gate.in_flight(),
                "Shutdown: plugin calls still running after {:?}; exiting anyway",
                PLUGIN_CALL_DRAIN
            );
        }
        // Give up the server ID last, once nothing of this replica is running.
        if let Some(heartbeat) = self.node_heartbeat.as_mut() {
            heartbeat.shutdown().await;
        }
        serve_result
    }
}

/// CORS for a split frontend origin. `expose_headers` lists every response
/// header the web client reads cross-origin: download filenames, rate-limit
/// backoff, the request id for error reports and `Date` for the server clock
/// offset.
fn cors_layer(cors: &crate::config::CorsConfig) -> anyhow::Result<CorsLayer> {
    let allow_origins = cors
        .allow_origins
        .iter()
        .map(|origin| {
            origin
                .parse::<HeaderValue>()
                .with_context(|| format!("Invalid CORS origin: {}", origin))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(CorsLayer::new()
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
        .expose_headers([
            header::CONTENT_DISPOSITION,
            header::DATE,
            header::RETRY_AFTER,
            HeaderName::from_static("x-request-id"),
        ])
        .allow_credentials(true)
        .max_age(Duration::from_secs(cors.max_age)))
}

/// How long shutdown waits for running plugin calls. Inside the usual 10s
/// stop grace of container supervisors, so a normal stop is not a SIGKILL.
const PLUGIN_CALL_DRAIN: Duration = Duration::from_secs(8);

/// Release every in-flight row this server owns so the fleet re-dispatches
/// it (see [`crate::dispatcher::steal::release_owned_leases`]).
async fn release_own_inflight(db: &sea_orm::DatabaseConnection, server_id: &str, phase: &str) {
    match crate::dispatcher::steal::release_owned_leases(db, server_id).await {
        Ok((submissions, code_runs, judgements)) => info!(
            server_id,
            submissions, code_runs, judgements, "{phase}: released own in-flight work"
        ),
        Err(e) => warn!(
            server_id,
            error = %e,
            "{phase}: failed to release own in-flight work; it is recovered at this \
             server's next boot or by the stuck-job detector"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn cors_exposes_the_headers_the_web_client_reads() {
        let cors = crate::config::CorsConfig {
            allow_origins: vec!["http://web.example".into()],
            ..Default::default()
        };
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .layer(cors_layer(&cors).unwrap());
        let res = app
            .oneshot(
                Request::get("/")
                    .header(header::ORIGIN, "http://web.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let exposed = res
            .headers()
            .get(header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        for name in ["content-disposition", "date", "retry-after", "x-request-id"] {
            assert!(exposed.contains(name), "{name} not exposed: {exposed}");
        }
    }
}
