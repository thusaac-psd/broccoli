use common::db::DatabaseConnection;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sea_orm::{DatabaseBackend, Statement};
use serde::Serialize;
use tokio::time::timeout;
use tracing::{instrument, warn};
use utoipa::ToSchema;

use crate::state::AppState;

/// Total time budget for the deep check. The probes run concurrently, so this
/// bounds the whole response, and it stays below the 2 s probe timeout a load
/// balancer typically uses: a slow dependency reads as `down`, never as a
/// timed-out probe.
const PROBE_BUDGET: Duration = Duration::from_millis(1500);

#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Aggregate status: `"ok"` when no probed component is down,
    /// `"degraded"` otherwise. A busy database is not down.
    #[schema(example = "ok")]
    pub status: String,
    /// Database probe result: `"ok"`, `"busy"` when every pooled connection
    /// is in use (the API answers `DATABASE_BUSY` meanwhile), or `"down"`.
    #[schema(example = "ok")]
    pub db: String,
    /// Message-queue (Redis) probe result: `"ok"` or `"down"`.
    #[schema(example = "ok")]
    pub mq: String,
    /// Blob storage probe result: `"ok"` or `"down"`. Object storage is
    /// checked at most every few seconds; a database or filesystem store
    /// reports `"ok"`.
    #[schema(example = "ok")]
    pub storage: String,
    /// Server version, matching the `Cargo.toml` `[package].version`.
    #[schema(example = "0.2.0")]
    pub version: String,
    /// Short Git SHA captured at build time (or `"unknown"` if not built
    /// from a git checkout).
    #[schema(example = "abc1234")]
    pub git_sha: String,
    /// Plugin readiness on this replica: `"ok"` when every plugin enabled in
    /// the shared desired state is loaded here, `"degraded"` otherwise. It
    /// does not change `status` or the HTTP code, so one plugin that fails
    /// everywhere cannot take the whole fleet out of rotation; a load
    /// balancer that should drain such a replica can match on this field.
    /// Which plugins and why is in the authenticated admin plugin API
    /// (`fleet`), not here: this endpoint is public, and load errors carry
    /// file paths and database messages.
    #[schema(example = "ok")]
    pub plugins: String,
}

/// Outcome of one dependency probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Ok,
    /// Every pooled connection is in use, and the probe found none free: the API
    /// answers `DATABASE_BUSY` meanwhile and serves requests as connections
    /// come free, so this does not fail readiness.
    Busy,
    Down,
}

impl Probe {
    fn as_str(self) -> &'static str {
        match self {
            Probe::Ok => "ok",
            Probe::Busy => "busy",
            Probe::Down => "down",
        }
    }

    fn healthy(self) -> bool {
        self != Probe::Down
    }

    /// Run `ping` within `PROBE_BUDGET`, logging why it failed.
    async fn run<E: std::fmt::Display>(
        component: &'static str,
        ping: impl std::future::Future<Output = Result<(), E>>,
    ) -> Self {
        match timeout(PROBE_BUDGET, ping).await {
            Ok(outcome) => Probe::settle(component, outcome, |_| false),
            Err(_) => {
                warn!(component, "Health check: ping timed out");
                Probe::Down
            }
        }
    }

    /// What `outcome` says about `component`: an error `busy` holds for is
    /// [`Probe::Busy`], any other one [`Probe::Down`].
    fn settle<E: std::fmt::Display>(
        component: &'static str,
        outcome: Result<(), E>,
        busy: impl Fn(&E) -> bool,
    ) -> Self {
        match outcome {
            Ok(()) => Probe::Ok,
            Err(e) if busy(&e) => {
                warn!(component, error = %e, "Health check: every connection is in use");
                Probe::Busy
            }
            Err(e) => {
                warn!(component, error = %e, "Health check: ping failed");
                Probe::Down
            }
        }
    }

    /// The database probe. Waiting for a connection and running `SELECT 1`
    /// share `PROBE_BUDGET`; a wait that outlasts it while the pool is
    /// saturated reads as busy by the rule the API answers `DATABASE_BUSY` by
    /// ([`common::db::outage`]), and a statement that does not answer on a
    /// connection it holds reads as down.
    async fn database(db: &DatabaseConnection) -> Self {
        let deadline = tokio::time::Instant::now() + PROBE_BUDGET;
        let ping = async {
            let mut conn = db.acquire_within(PROBE_BUDGET).await?;
            let select = Statement::from_string(DatabaseBackend::Postgres, "SELECT 1");
            match tokio::time::timeout_at(deadline, conn.execute(select)).await {
                Ok(outcome) => outcome.map(|_| ()),
                Err(_) => Err(sea_orm::DbErr::Custom("SELECT 1 timed out".into())),
            }
        };
        Probe::settle("db", ping.await, |e| {
            common::db::outage(e) == Some(common::db::Outage::Busy)
        })
    }
}

/// Result of probing the shared dependencies, see [`probe_dependencies`].
#[derive(Debug, Clone, Copy)]
pub struct DependencyHealth {
    db: Probe,
    mq: Probe,
    storage: Probe,
}

impl DependencyHealth {
    pub fn db(&self) -> &'static str {
        self.db.as_str()
    }

    pub fn mq(&self) -> &'static str {
        self.mq.as_str()
    }

    pub fn storage(&self) -> &'static str {
        self.storage.as_str()
    }

    pub fn healthy(&self) -> bool {
        self.db.healthy() && self.mq.healthy() && self.storage.healthy()
    }
}

/// Probes the database, MQ and blob storage concurrently; the whole probe is
/// bounded by `PROBE_BUDGET`, not budget-per-dependency.
pub async fn probe_dependencies(
    db: &DatabaseConnection,
    mq: &mq::Mq,
    blob_store: &dyn common::storage::BlobStore,
) -> DependencyHealth {
    let (db, mq, storage) = tokio::join!(
        Probe::database(db),
        Probe::run("mq", mq.ping()),
        Probe::run("storage", blob_store.probe()),
    );
    DependencyHealth { db, mq, storage }
}

/// The deep readiness / monitoring check, shared between the documented
/// `/api/v1/health` route and the plain `/healthz` route: the shared
/// dependencies plus this replica's plugin readiness. Load balancers should
/// probe [`readyz`] instead: every server shares the same database and Redis,
/// so a dependency blip would fail this check on every replica at once and
/// leave the balancer with no upstream at all.
pub async fn compute_health(state: &AppState) -> HealthResponse {
    let deps = probe_dependencies(&state.db, &state.mq, state.blob_store.as_ref()).await;
    let plugins_ready = crate::plugin_lifecycle::not_ready(state).is_empty();

    HealthResponse {
        status: if deps.healthy() { "ok" } else { "degraded" }.to_string(),
        db: deps.db().to_string(),
        mq: deps.mq().to_string(),
        storage: deps.storage().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: option_env!("BROCCOLI_GIT_SHA")
            .unwrap_or("unknown")
            .to_string(),
        plugins: if plugins_ready { "ok" } else { "degraded" }.to_string(),
    }
}

fn status_code(body: &HealthResponse) -> StatusCode {
    if body.status == "ok" {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Plain-router handler for `GET /healthz`. Not part of the OpenAPI surface.
#[instrument(skip(state))]
pub async fn healthz(State(state): State<AppState>) -> impl IntoResponse {
    let body = compute_health(&state).await;
    (status_code(&body), Json(body))
}

/// `GET /livez`: shallow liveness. The process is up and its HTTP stack is
/// serving; no database, Redis or object-storage call is made, so it keeps
/// answering while shared infrastructure is down and costs next to nothing at a
/// 5 s probe interval. A load balancer probes [`readyz`], which adds whether
/// this replica should take browsers; `/healthz` is the deep check for
/// monitoring.
pub async fn livez() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "alive",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// `GET /readyz`: whether this replica should take browsers, the probe the
/// shipped gateways health-check. It fails (503 `not_ready`) while a peer
/// with a fresh heartbeat serves every enabled plugin this replica serves
/// and more (`PluginLifecycle::ready`): a request pinned here would miss
/// that plugin (its contest type, language, evaluator, checker or route)
/// until the load here succeeds, so the gateway re-pins its browsers to a
/// ready replica. The admin system overview names what each server lacks.
/// It makes no dependency call, so a database or Redis blip never ejects
/// every replica at once, and its public body names no plugin.
pub async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.plugin_lifecycle.ready() {
        (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready" })),
        )
            .into_response()
    } else {
        // Not ready is the state a gateway probes for, not a failure.
        let mut response = (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "status": "not_ready" })),
        )
            .into_response();
        response
            .extensions_mut()
            .insert(crate::error::DesignedState);
        response
    }
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "Meta",
    operation_id = "getHealth",
    summary = "Server dependency health",
    description = "Public, unauthenticated. Pings the database, message queue and blob storage concurrently, \
                   within a 1.5s total budget. Returns 200 with `status: \"ok\"` when no probed \
                   component is down (a database whose pool is saturated reads `db: \"busy\"` \
                   and is not down); 503 with `status: \"degraded\"` otherwise. This is the \
                   deep check for monitoring; load balancers should probe `GET /readyz`, \
                   which makes no dependency calls.",
    responses(
        (status = 200, description = "All components healthy", body = HealthResponse),
        (status = 503, description = "One or more components are down", body = HealthResponse),
    ),
)]
#[instrument(skip(state))]
pub async fn get_health(State(state): State<AppState>) -> impl IntoResponse {
    let body = compute_health(&state).await;
    (status_code(&body), Json(body))
}
