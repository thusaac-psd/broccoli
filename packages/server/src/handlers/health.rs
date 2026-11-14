use common::db::DatabaseConnection;
use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
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
    /// Aggregate status: `"ok"` when every probed component is healthy,
    /// `"degraded"` otherwise.
    #[schema(example = "ok")]
    pub status: String,
    /// Database probe result: `"ok"` or `"down"`.
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
    Down,
}

impl Probe {
    fn as_str(self) -> &'static str {
        match self {
            Probe::Ok => "ok",
            Probe::Down => "down",
        }
    }

    fn healthy(self) -> bool {
        self == Probe::Ok
    }

    /// Run `ping` within `PROBE_BUDGET`, logging why it failed.
    async fn run<E: std::fmt::Display>(
        component: &'static str,
        ping: impl std::future::Future<Output = Result<(), E>>,
    ) -> Self {
        match timeout(PROBE_BUDGET, ping).await {
            Ok(Ok(())) => Probe::Ok,
            Ok(Err(e)) => {
                warn!(component, error = %e, "Health check: ping failed");
                Probe::Down
            }
            Err(_) => {
                warn!(component, "Health check: ping timed out");
                Probe::Down
            }
        }
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
        Probe::run("db", ping_db(db)),
        Probe::run("mq", mq.ping()),
        Probe::run("storage", blob_store.probe()),
    );
    DependencyHealth { db, mq, storage }
}

/// The deep readiness / monitoring check, shared between the documented
/// `/api/v1/health` route and the plain `/healthz` route: the shared
/// dependencies plus this replica's plugin readiness. Load balancers should
/// probe [`livez`] instead: every server shares the same database and Redis, so
/// a dependency blip would fail this check on every replica at once and leave
/// the balancer with no upstream at all.
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

async fn ping_db(db: &DatabaseConnection) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT 1".to_string(),
    ))
    .await
    .map(|_| ())
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
/// 5 s probe interval. This is what a load balancer should probe; `/healthz`
/// is the deep check for readiness and monitoring.
pub async fn livez() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "status": "alive",
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

#[utoipa::path(
    get,
    path = "/health",
    tag = "Meta",
    operation_id = "getHealth",
    summary = "Server readiness and dependency health",
    description = "Public, unauthenticated. Pings the database, message queue and blob storage concurrently, \
                   within a 1.5s total budget. Returns 200 with `status: \"ok\"` when all probed \
                   components are healthy; 503 with `status: \"degraded\"` otherwise. This is the \
                   deep readiness check; load balancers should probe the shallow `GET /livez`, \
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
