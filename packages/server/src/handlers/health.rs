use std::time::Duration;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
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
    /// Message-queue probe result: `"ok"`, `"down"`, or `"disabled"` when
    /// MQ is not configured.
    #[schema(example = "ok")]
    pub mq: String,
    /// Server version, matching the `Cargo.toml` `[package].version`.
    #[schema(example = "0.2.0")]
    pub version: String,
    /// Short Git SHA captured at build time (or `"unknown"` if not built
    /// from a git checkout).
    #[schema(example = "abc1234")]
    pub git_sha: String,
}

/// Outcome of one dependency probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Probe {
    Ok,
    Down,
    Disabled,
}

impl Probe {
    fn as_str(self) -> &'static str {
        match self {
            Probe::Ok => "ok",
            Probe::Down => "down",
            Probe::Disabled => "disabled",
        }
    }

    fn healthy(self) -> bool {
        match self {
            Probe::Ok | Probe::Disabled => true,
            Probe::Down => false,
        }
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

/// Probes the database and MQ backends concurrently and returns a populated
/// [`HealthResponse`]. The whole check is bounded by `PROBE_BUDGET`.
///
/// This is the deep readiness / monitoring check, shared between the documented
/// `/api/v1/health` route and the plain `/healthz` route. Load balancers should
/// probe [`livez`] instead: every server shares the same database and Redis, so
/// a dependency blip would fail this check on every replica at once and leave
/// the balancer with no upstream at all.
pub async fn compute_health(
    db: &DatabaseConnection,
    redis_client: Option<&redis::Client>,
    mq_enabled: bool,
) -> HealthResponse {
    let mq_probe = async {
        match (mq_enabled, redis_client) {
            (false, _) => Probe::Disabled,
            (true, Some(client)) => Probe::run("mq", ping_redis(client)).await,
            (true, None) => {
                warn!("Health check: MQ enabled but no Redis client available");
                Probe::Down
            }
        }
    };
    let (db_status, mq_status) = tokio::join!(Probe::run("db", ping_db(db)), mq_probe);

    let status = if db_status.healthy() && mq_status.healthy() {
        "ok"
    } else {
        "degraded"
    };

    HealthResponse {
        status: status.to_string(),
        db: db_status.as_str().to_string(),
        mq: mq_status.as_str().to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        git_sha: option_env!("BROCCOLI_GIT_SHA")
            .unwrap_or("unknown")
            .to_string(),
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

async fn ping_redis(client: &redis::Client) -> Result<(), redis::RedisError> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let _pong: String = redis::cmd("PING").query_async(&mut conn).await?;
    Ok(())
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
    let body = compute_health(&state.db, state.redis_client.as_deref(), state.mq.is_some()).await;
    (status_code(&body), Json(body))
}

/// `GET /livez`: shallow liveness. The process is up and its HTTP stack is
/// serving; no database, Redis or object-storage call is made, so it keeps
/// answering while shared infrastructure is down and costs next to nothing at a
/// 5 s probe interval. This is what a load balancer should probe; `/healthz`
/// is the deep check for readiness and monitoring.
///
/// Served on the main router and on the dedicated `server.healthz_listen`
/// runtime. The dedicated runtime also answers `/healthz` with this handler (as
/// it always has): pinging the DB or Redis from there would run through pool
/// driver tasks on the *main* runtime and stall with it, defeating the point of
/// the dedicated listener.
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
    description = "Public, unauthenticated. Pings the database and message queue concurrently, \
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
    let body = compute_health(&state.db, state.redis_client.as_deref(), state.mq.is_some()).await;
    (status_code(&body), Json(body))
}
