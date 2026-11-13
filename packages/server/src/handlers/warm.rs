//! Contest cache pre-warm endpoints.
//!
//! `POST /contests/{id}/prewarm` enumerates a contest's distinct testcase blobs
//! and broadcasts a warm job to every live worker over Redis pub/sub; each worker
//! fetches the blobs into its local file cache. `GET .../prewarm/status`
//! aggregates per-worker progress against the live heartbeat registry so an admin
//! can watch the warm complete before the contest opens.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use broccoli_server_sdk::permissions as perm;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde::Deserialize;
use tracing::{info, instrument};
use utoipa::IntoParams;

use common::warm::{
    WARM_CHANNEL, WARM_KEY_TTL_SECS, WarmJob, WarmProgress, WarmState, warm_contest_key,
    warm_job_key, warm_progress_pattern,
};

use crate::error::AppError;
use crate::extractors::auth::{AuthUser, FreshAuthUser};
use crate::extractors::path::AppPath;
use crate::handlers::system::read_workers;
use crate::models::warm::{PrewarmResponse, WarmStatusResponse, WorkerWarmStatus};
use crate::state::AppState;
use crate::utils::contest::find_contest;

/// Distinct input + answer blob hashes across all testcases of a contest's
/// problems. `$1` (contest id) is referenced twice; Postgres binds by position.
const ENUMERATE_BLOBS_SQL: &str = r#"
    SELECT DISTINCT h FROM (
        SELECT tc.input_blob_hash AS h
        FROM test_case tc
        JOIN contest_problem cp ON cp.problem_id = tc.problem_id
        WHERE cp.contest_id = $1 AND tc.input_blob_hash IS NOT NULL
        UNION
        SELECT tc.expected_output_blob_hash AS h
        FROM test_case tc
        JOIN contest_problem cp ON cp.problem_id = tc.problem_id
        WHERE cp.contest_id = $1 AND tc.expected_output_blob_hash IS NOT NULL
    ) sub
"#;

#[utoipa::path(
    post,
    path = "/{id}/prewarm",
    tag = "Contests",
    operation_id = "prewarmContest",
    summary = "Pre-warm worker caches with a contest's testcase blobs",
    params(("id" = i32, Path, description = "Contest id")),
    responses(
        (status = 200, description = "Warm job published", body = PrewarmResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)"),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)"),
        (status = 404, description = "Contest not found (NOT_FOUND)"),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id = id))]
pub async fn prewarm_contest(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
) -> Result<Json<PrewarmResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    find_contest(&state.db, id).await?;

    let client = state.redis_client.as_ref().ok_or_else(|| {
        AppError::Internal(
            "Cache warming requires the message queue (Redis) to be configured".into(),
        )
    })?;

    let hashes = enumerate_contest_blob_hashes(&state.db, id).await?;
    let total_blobs = hashes.len() as u32;
    let job = publish_warm_job(client, id, hashes, state.clock.now()).await?;

    let live_workers = read_workers(&state).await.len() as u32;

    info!(
        job_id = %job.job_id,
        total_blobs,
        live_workers,
        "Published contest cache pre-warm job"
    );

    Ok(Json(PrewarmResponse {
        job_id: job.job_id,
        contest_id: id,
        total_blobs,
        live_workers,
    }))
}

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WarmStatusQuery {
    /// Specific warm job to report; defaults to the contest's most recent warm.
    pub job: Option<String>,
}

#[utoipa::path(
    get,
    path = "/{id}/prewarm/status",
    tag = "Contests",
    operation_id = "getPrewarmStatus",
    summary = "Per-worker progress of a contest cache pre-warm",
    params(("id" = i32, Path, description = "Contest id"), WarmStatusQuery),
    responses(
        (status = 200, description = "Aggregated warm status", body = WarmStatusResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)"),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)"),
        (status = 404, description = "Contest not found (NOT_FOUND)"),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user), fields(contest_id = id))]
pub async fn prewarm_status(
    auth_user: AuthUser,
    State(state): State<AppState>,
    AppPath(id): AppPath<i32>,
    Query(query): Query<WarmStatusQuery>,
) -> Result<Json<WarmStatusResponse>, AppError> {
    auth_user.require_permission(perm::CONTEST_MANAGE)?;
    find_contest(&state.db, id).await?;

    let client = state.redis_client.as_ref().ok_or_else(|| {
        AppError::Internal(
            "Cache warming requires the message queue (Redis) to be configured".into(),
        )
    })?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .map_err(redis_err)?;

    // Resolve the job: explicit query param, else the contest's latest warm.
    let job_id = match query.job {
        Some(j) => Some(j),
        None => conn
            .get::<_, Option<String>>(warm_contest_key(id))
            .await
            .map_err(redis_err)?,
    };

    let Some(job_id) = job_id else {
        // No warm has ever been triggered for this contest.
        return Ok(Json(WarmStatusResponse {
            job_id: None,
            contest_id: id,
            total_blobs: 0,
            overall_fraction: 0.0,
            workers_total: 0,
            workers_complete: 0,
            workers: Vec::new(),
        }));
    };

    let total_blobs = match conn
        .get::<_, Option<String>>(warm_job_key(&job_id))
        .await
        .map_err(redis_err)?
    {
        Some(raw) => serde_json::from_str::<WarmJob>(&raw)
            .map(|j| j.total())
            .unwrap_or(0),
        None => 0,
    };

    let mut by_worker: HashMap<String, WarmProgress> = read_progress(&mut conn, &job_id)
        .await?
        .into_iter()
        .map(|p| (p.worker_id.clone(), p))
        .collect();

    let live = read_workers(&state).await;

    let mut workers = Vec::new();
    let mut fraction_sum = 0.0;
    let mut workers_complete = 0u32;

    // Live workers: report their progress (or "pending" if not yet started).
    for w in &live {
        let entry = by_worker.remove(&w.id);
        let ws = worker_status(&w.id, total_blobs, entry);
        fraction_sum += ws.fraction;
        if ws.state == "complete" {
            workers_complete += 1;
        }
        workers.push(ws);
    }
    let workers_total = live.len() as u32;

    // Workers that reported progress but are no longer live (warmed then died);
    // shown for transparency but excluded from the live overall fraction.
    for (_, p) in by_worker {
        let wid = p.worker_id.clone();
        workers.push(WorkerWarmStatus {
            stale: true,
            ..worker_status(&wid, total_blobs, Some(p))
        });
    }

    let overall_fraction = if workers_total > 0 {
        fraction_sum / workers_total as f64
    } else if total_blobs == 0 {
        1.0
    } else {
        0.0
    };

    Ok(Json(WarmStatusResponse {
        job_id: Some(job_id),
        contest_id: id,
        total_blobs,
        overall_fraction,
        workers_total,
        workers_complete,
        workers,
    }))
}

/// A live worker's progress (or `pending` before it reports any).
fn worker_status(
    worker_id: &str,
    total_blobs: u32,
    progress: Option<WarmProgress>,
) -> WorkerWarmStatus {
    match progress {
        Some(p) => WorkerWarmStatus {
            worker_id: worker_id.to_string(),
            warmed: p.warmed,
            total: p.total,
            fraction: p.fraction(),
            state: warm_state_str(p.state),
            stale: false,
            reported: true,
            error: p.error,
        },
        None => WorkerWarmStatus {
            worker_id: worker_id.to_string(),
            warmed: 0,
            total: total_blobs,
            fraction: if total_blobs == 0 { 1.0 } else { 0.0 },
            state: "pending".to_string(),
            stale: false,
            reported: false,
            error: None,
        },
    }
}

fn warm_state_str(state: WarmState) -> String {
    match state {
        WarmState::Pending => "pending",
        WarmState::Warming => "warming",
        WarmState::Complete => "complete",
        WarmState::Error => "error",
    }
    .to_string()
}

async fn enumerate_contest_blob_hashes(
    db: &DatabaseConnection,
    contest_id: i32,
) -> Result<Vec<String>, AppError> {
    let stmt = Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        ENUMERATE_BLOBS_SQL,
        [contest_id.into()],
    );
    let rows = db.query_all_raw(stmt).await?;
    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<String>("", "h").ok())
        .collect())
}

async fn publish_warm_job(
    client: &redis::Client,
    contest_id: i32,
    hashes: Vec<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<WarmJob, AppError> {
    let job = WarmJob {
        job_id: uuid::Uuid::new_v4().to_string(),
        contest_id,
        hashes,
        created_at_unix_ms: now.timestamp_millis(),
    };
    let body = serde_json::to_string(&job)
        .map_err(|e| AppError::Internal(format!("Failed to serialize warm job: {e}")))?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .map_err(redis_err)?;

    // Store the full payload (so late/reconnecting workers can read it) + map the
    // contest to this job, then notify workers.
    let _: () = redis::cmd("SET")
        .arg(warm_job_key(&job.job_id))
        .arg(&body)
        .arg("EX")
        .arg(WARM_KEY_TTL_SECS)
        .query_async(&mut conn)
        .await
        .map_err(redis_err)?;
    let _: () = redis::cmd("SET")
        .arg(warm_contest_key(contest_id))
        .arg(&job.job_id)
        .arg("EX")
        .arg(WARM_KEY_TTL_SECS)
        .query_async(&mut conn)
        .await
        .map_err(redis_err)?;
    let _: i64 = redis::cmd("PUBLISH")
        .arg(WARM_CHANNEL)
        .arg(&job.job_id)
        .query_async(&mut conn)
        .await
        .map_err(redis_err)?;

    Ok(job)
}

async fn read_progress(
    conn: &mut MultiplexedConnection,
    job_id: &str,
) -> Result<Vec<WarmProgress>, AppError> {
    let pattern = warm_progress_pattern(job_id);

    // Collect matching keys, then MGET - same SCAN-then-MGET shape as the worker
    // heartbeat reader (the SCAN iterator borrows conn, so drain it first).
    let keys: Vec<String> = {
        let mut iter = conn
            .scan_match::<&str, String>(&pattern)
            .await
            .map_err(redis_err)?;
        let mut acc = Vec::new();
        while let Some(item) = iter.next_item().await {
            acc.push(item.map_err(redis_err)?);
        }
        acc
    };

    if keys.is_empty() {
        return Ok(Vec::new());
    }

    let values: Vec<Option<String>> = conn.mget(&keys).await.map_err(redis_err)?;
    Ok(values
        .into_iter()
        .filter_map(|v| v.and_then(|s| serde_json::from_str::<WarmProgress>(&s).ok()))
        .collect())
}

fn redis_err(e: redis::RedisError) -> AppError {
    AppError::Internal(format!("Redis error: {e}"))
}
