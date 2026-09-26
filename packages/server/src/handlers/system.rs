use std::collections::HashMap;
use std::time::Duration;

use axum::{Json, extract::State};
use broccoli_server_sdk::permissions as perm;
use chrono::Utc;
use common::SubmissionStatus;
use opentelemetry::KeyValue;
use redis::AsyncCommands;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, PaginatorTrait, QueryFilter, QuerySelect,
    Statement,
};
use tracing::{instrument, warn};

use common::worker::HeartbeatPayload;

// visibility-bypass-audited: all three handlers require perm::SYSTEM_VIEW,
// pinned by tests/integration/system.rs's `contestant_cannot_list_workers`,
// `contestant_cannot_list_queues`, and `contestant_cannot_view_system_overview`.
// This is aggregate operator telemetry (worker/queue/DLQ counts), never a
// per-row Contest/Problem/Submission/Clarification view the kernel governs.
use crate::entity::{code_run, dead_letter_message, submission, submission_judgement};
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::AuthUser;
use crate::models::system::{
    QueueInfo, QueuesResponse, SystemOverviewResponse, WorkerInfo, WorkersResponse,
};
use crate::state::AppState;

pub(crate) const HEARTBEAT_KEY_PREFIX: &str = common::worker::WORKER_HEARTBEAT_KEY_PREFIX;
const STALE_AFTER_SECS: i64 = 10;

#[utoipa::path(
    get,
    path = "/workers",
    tag = "System",
    operation_id = "listSystemWorkers",
    summary = "List active workers",
    description = "Returns workers that have written a heartbeat to Redis within the past ~15 seconds. Requires `system:view` permission.",
    responses(
        (status = 200, description = "List of workers", body = WorkersResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn list_workers(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<WorkersResponse>, AppError> {
    auth_user.require_permission(perm::SYSTEM_VIEW)?;

    let workers = read_workers(&state).await;
    Ok(Json(WorkersResponse { workers }))
}

#[utoipa::path(
    get,
    path = "/queues",
    tag = "System",
    operation_id = "listSystemQueues",
    summary = "List MQ queue depths",
    description = "Returns the current depth of each Broccoli MQ queue (operation tasks, results, DLQ). Requires `system:view` permission.",
    responses(
        (status = 200, description = "Queue depths", body = QueuesResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn list_queues(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<QueuesResponse>, AppError> {
    auth_user.require_permission(perm::SYSTEM_VIEW)?;

    let queues = read_queues(&state).await;
    Ok(Json(QueuesResponse { queues }))
}

#[utoipa::path(
    get,
    path = "/overview",
    tag = "System",
    operation_id = "getSystemOverview",
    summary = "Aggregated system health",
    description = "Returns workers, queue depths, in-progress submissions, and unresolved DLQ count in a single response. Requires `system:view` permission.",
    responses(
        (status = 200, description = "System overview", body = SystemOverviewResponse),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user))]
pub async fn system_overview(
    auth_user: AuthUser,
    State(state): State<AppState>,
) -> Result<Json<SystemOverviewResponse>, AppError> {
    auth_user.require_permission(perm::SYSTEM_VIEW)?;

    let workers = read_workers(&state).await;
    let queues = read_queues(&state).await;

    let submissions_in_progress = submission::Entity::find()
        .filter(submission::Column::Status.is_in(vec![
            SubmissionStatus::Pending.to_string(),
            SubmissionStatus::Compiling.to_string(),
            SubmissionStatus::Running.to_string(),
        ]))
        .count(&state.db)
        .await?;

    let dlq_unresolved_count = dead_letter_message::Entity::find()
        .filter(dead_letter_message::Column::Resolved.eq(false))
        .count(&state.db)
        .await?;

    Ok(Json(SystemOverviewResponse {
        workers,
        queues,
        submissions_in_progress,
        dlq_unresolved_count,
    }))
}

/// Returns the set of worker IDs that have a live (non-stale) heartbeat in
/// Redis. Used by admin endpoints that need to validate `target_worker_id`
/// values before they are persisted on submissions.
pub(crate) async fn live_worker_ids(state: &AppState) -> std::collections::HashSet<String> {
    read_workers(state)
        .await
        .into_iter()
        .filter(|w| !w.stale)
        .map(|w| w.id)
        .collect()
}

pub fn spawn_queue_depth_sampler(state: AppState, interval: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut previous_queue_depths = HashMap::new();
        let mut previous_submission_in_flight = HashMap::new();
        let mut previous_submission_judge_queue_depth = 0;

        loop {
            ticker.tick().await;
            record_queue_depths(&state, &mut previous_queue_depths).await;
            record_submission_lifecycle_metrics(
                &state,
                &mut previous_submission_in_flight,
                &mut previous_submission_judge_queue_depth,
            )
            .await;
        }
    });
}

async fn record_queue_depths(state: &AppState, previous: &mut HashMap<QueueDepthKey, u64>) {
    let queues = read_queues(state).await;
    let samples = queues
        .iter()
        .map(|queue| {
            QueueDepthSample::new(queue_role(state, &queue.name), &queue.name, queue.depth)
        })
        .collect();

    for delta in queue_depth_deltas(previous, samples) {
        let attrs = [
            KeyValue::new("queue.role", delta.key.role.clone()),
            KeyValue::new("queue.name", delta.key.name.clone()),
        ];
        state.metrics.mq_queue_depth.add(delta.delta, &attrs);
    }
}

async fn record_submission_lifecycle_metrics(
    state: &AppState,
    previous_in_flight: &mut HashMap<SubmissionInFlightKey, u64>,
    previous_judge_queue_depth: &mut u64,
) {
    match read_submission_in_flight_samples(state).await {
        Ok(samples) => {
            for delta in submission_in_flight_deltas(previous_in_flight, samples) {
                let attrs = [
                    KeyValue::new("row.kind", delta.key.row_kind.clone()),
                    KeyValue::new("state", delta.key.state.clone()),
                ];
                state.metrics.submission_in_flight.add(delta.delta, &attrs);
            }
        }
        Err(e) => warn!(error = ?e, "Submission in-flight metric sampling failed"),
    }

    match crate::dispatcher::queue_depth::count_queued_rows(&state.db).await {
        Ok(depth) => {
            let delta = submission_judge_queue_depth_delta(previous_judge_queue_depth, depth);
            if delta != 0 {
                state
                    .metrics
                    .submission_judge_queue_depth
                    .add(delta, &[KeyValue::new("source", "postgres_status_queued")]);
            }
        }
        Err(e) => warn!(error = ?e, "Submission judge queue depth metric sampling failed"),
    }

    match read_pending_submission_ages(state).await {
        Ok(samples) => {
            for sample in samples {
                let attrs = [
                    KeyValue::new("row.kind", sample.row_kind),
                    KeyValue::new("state", "Pending"),
                ];
                state
                    .metrics
                    .submission_age_in_pending_seconds
                    .record(sample.age_seconds, &attrs);
            }
        }
        Err(e) => warn!(error = ?e, "Submission pending-age metric sampling failed"),
    }
}

fn queue_role(state: &AppState, queue_name: &str) -> &'static str {
    if queue_name == state.config.mq.operation_queue_name {
        "operation"
    } else if queue_name
        .strip_prefix(&state.config.mq.operation_queue_name)
        .is_some_and(|suffix| suffix.starts_with(":worker:"))
    {
        "operation_private"
    } else if queue_name == state.config.mq.operation_result_queue_name {
        "operation_result"
    } else if queue_name == state.config.mq.operation_dlq_queue_name {
        "operation_dlq"
    } else {
        "unknown"
    }
}

fn queue_depth_deltas(
    previous: &mut HashMap<QueueDepthKey, u64>,
    samples: Vec<QueueDepthSample>,
) -> Vec<QueueDepthDelta> {
    let mut current = HashMap::new();
    for sample in samples {
        current.insert(sample.key, sample.depth);
    }

    let mut deltas = Vec::new();
    for (key, depth) in &current {
        let prior = previous.get(key).copied().unwrap_or(0);
        let delta = *depth as i64 - prior as i64;
        if delta != 0 {
            deltas.push(QueueDepthDelta {
                key: key.clone(),
                delta,
            });
        }
    }

    for (key, depth) in previous.iter() {
        if !current.contains_key(key) && *depth != 0 {
            deltas.push(QueueDepthDelta {
                key: key.clone(),
                delta: -(*depth as i64),
            });
        }
    }

    deltas.sort_by(|a, b| a.key.cmp(&b.key));
    *previous = current;
    deltas
}

async fn read_submission_in_flight_samples(
    state: &AppState,
) -> Result<Vec<SubmissionInFlightSample>, AppError> {
    let mut samples = Vec::new();
    for (row_kind, table) in [
        ("submission", "submission"),
        ("code_run", "code_run"),
        ("submission_judgement", "submission_judgement"),
    ] {
        samples.extend(read_status_count_samples(state, row_kind, table).await?);
    }
    Ok(samples)
}

async fn read_status_count_samples(
    state: &AppState,
    row_kind: &'static str,
    table: &'static str,
) -> Result<Vec<SubmissionInFlightSample>, AppError> {
    let sql = format!(
        r#"
        SELECT status::text AS state, COUNT(*)::bigint AS count
          FROM {table}
         WHERE status IN ('Queued', 'Pending', 'Compiling', 'Running')
         GROUP BY status
        "#
    );
    let rows = state
        .db
        .query_all_raw(Statement::from_string(DbBackend::Postgres, sql))
        .await?;

    let mut samples = Vec::with_capacity(rows.len());
    for row in rows {
        let state = row
            .try_get::<String>("", "state")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        let count = row
            .try_get::<i64>("", "count")
            .map_err(|e| AppError::Internal(e.to_string()))?;
        if count > 0 {
            samples.push(SubmissionInFlightSample::new(row_kind, state, count as u64));
        }
    }
    Ok(samples)
}

struct PendingAgeSample {
    row_kind: &'static str,
    age_seconds: f64,
}

async fn read_pending_submission_ages(state: &AppState) -> Result<Vec<PendingAgeSample>, AppError> {
    let now = Utc::now();
    let mut samples = Vec::new();

    // Only the two timestamp columns are needed to compute age. Select just those
    // rather than hydrating whole rows: submission/code_run each carry a `files`
    // JSON blob (and code_run also `custom_test_cases`) that can reach tens of MB,
    // and this runs on the admin system-overview poll every few seconds — loading
    // every column of every pending row would shovel that much data each tick
    // under a judging backlog.
    type PendingTs = (Option<chrono::DateTime<Utc>>, chrono::DateTime<Utc>);

    let submission_ts: Vec<PendingTs> = submission::Entity::find()
        .select_only()
        .column(submission::Column::LeaseHeartbeatAt)
        .column(submission::Column::CreatedAt)
        .filter(submission::Column::Status.eq(SubmissionStatus::Pending))
        .into_tuple()
        .all(&state.db)
        .await?;
    for (heartbeat, created_at) in submission_ts {
        samples.push(PendingAgeSample {
            row_kind: "submission",
            age_seconds: age_seconds(heartbeat.unwrap_or(created_at), now),
        });
    }

    let code_run_ts: Vec<PendingTs> = code_run::Entity::find()
        .select_only()
        .column(code_run::Column::LeaseHeartbeatAt)
        .column(code_run::Column::CreatedAt)
        .filter(code_run::Column::Status.eq(SubmissionStatus::Pending))
        .into_tuple()
        .all(&state.db)
        .await?;
    for (heartbeat, created_at) in code_run_ts {
        samples.push(PendingAgeSample {
            row_kind: "code_run",
            age_seconds: age_seconds(heartbeat.unwrap_or(created_at), now),
        });
    }

    let judgement_ts: Vec<PendingTs> = submission_judgement::Entity::find()
        .select_only()
        .column(submission_judgement::Column::LeaseHeartbeatAt)
        .column(submission_judgement::Column::CreatedAt)
        .filter(submission_judgement::Column::Status.eq(SubmissionStatus::Pending))
        .into_tuple()
        .all(&state.db)
        .await?;
    for (heartbeat, created_at) in judgement_ts {
        samples.push(PendingAgeSample {
            row_kind: "submission_judgement",
            age_seconds: age_seconds(heartbeat.unwrap_or(created_at), now),
        });
    }

    Ok(samples)
}

fn age_seconds(created_at: chrono::DateTime<Utc>, now: chrono::DateTime<Utc>) -> f64 {
    std::cmp::max(now.signed_duration_since(created_at).num_milliseconds(), 0) as f64 / 1_000.0
}

fn submission_judge_queue_depth_delta(previous: &mut u64, current: u64) -> i64 {
    let delta = current as i64 - *previous as i64;
    *previous = current;
    delta
}

fn submission_in_flight_deltas(
    previous: &mut HashMap<SubmissionInFlightKey, u64>,
    samples: Vec<SubmissionInFlightSample>,
) -> Vec<SubmissionInFlightDelta> {
    let mut current = HashMap::new();
    for sample in samples {
        current.insert(sample.key, sample.count);
    }

    let mut deltas = Vec::new();
    for (key, count) in &current {
        let prior = previous.get(key).copied().unwrap_or(0);
        let delta = *count as i64 - prior as i64;
        if delta != 0 {
            deltas.push(SubmissionInFlightDelta {
                key: key.clone(),
                delta,
            });
        }
    }

    for (key, count) in previous.iter() {
        if !current.contains_key(key) && *count != 0 {
            deltas.push(SubmissionInFlightDelta {
                key: key.clone(),
                delta: -(*count as i64),
            });
        }
    }

    deltas.sort_by(|a, b| a.key.cmp(&b.key));
    *previous = current;
    deltas
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct QueueDepthKey {
    role: String,
    name: String,
}

#[derive(Debug, Clone)]
struct QueueDepthSample {
    key: QueueDepthKey,
    depth: u64,
}

impl QueueDepthSample {
    fn new(role: impl Into<String>, name: impl Into<String>, depth: u64) -> Self {
        Self {
            key: QueueDepthKey {
                role: role.into(),
                name: name.into(),
            },
            depth,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct QueueDepthDelta {
    key: QueueDepthKey,
    delta: i64,
}

impl QueueDepthDelta {
    #[cfg(test)]
    fn new(role: impl Into<String>, name: impl Into<String>, delta: i64) -> Self {
        Self {
            key: QueueDepthKey {
                role: role.into(),
                name: name.into(),
            },
            delta,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
struct SubmissionInFlightKey {
    row_kind: String,
    state: String,
}

#[derive(Debug, Clone)]
struct SubmissionInFlightSample {
    key: SubmissionInFlightKey,
    count: u64,
}

impl SubmissionInFlightSample {
    fn new(row_kind: impl Into<String>, state: impl Into<String>, count: u64) -> Self {
        Self {
            key: SubmissionInFlightKey {
                row_kind: row_kind.into(),
                state: state.into(),
            },
            count,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct SubmissionInFlightDelta {
    key: SubmissionInFlightKey,
    delta: i64,
}

impl SubmissionInFlightDelta {
    #[cfg(test)]
    fn new(row_kind: impl Into<String>, state: impl Into<String>, delta: i64) -> Self {
        Self {
            key: SubmissionInFlightKey {
                row_kind: row_kind.into(),
                state: state.into(),
            },
            delta,
        }
    }
}

pub(crate) async fn read_workers(state: &AppState) -> Vec<WorkerInfo> {
    let Some(client) = state.redis_client.as_ref() else {
        return Vec::new();
    };

    let mut conn = match client.get_multiplexed_async_connection().await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Failed to connect to Redis for worker heartbeats");
            return Vec::new();
        }
    };

    let pattern = format!("{HEARTBEAT_KEY_PREFIX}*");
    let keys: Vec<String> = match conn.scan_match::<&str, String>(&pattern).await {
        Ok(mut iter) => {
            let mut acc: Vec<String> = Vec::new();
            while let Some(item) = iter.next_item().await {
                match item {
                    Ok(key) => acc.push(key),
                    Err(e) => {
                        warn!(error = %e, "Worker heartbeat SCAN entry failed");
                        return Vec::new();
                    }
                }
            }
            acc
        }
        Err(e) => {
            warn!(error = %e, "Worker heartbeat SCAN failed");
            return Vec::new();
        }
    };

    if keys.is_empty() {
        return Vec::new();
    }

    let values: Vec<Option<String>> = match conn.mget(&keys).await {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "Worker heartbeat MGET failed");
            return Vec::new();
        }
    };

    let now = Utc::now();
    let mut workers: Vec<WorkerInfo> = values
        .into_iter()
        .filter_map(|v| v.and_then(|s| serde_json::from_str::<HeartbeatPayload>(&s).ok()))
        .map(|p| {
            let elapsed = (now - p.last_seen).num_seconds();
            WorkerInfo {
                id: p.id,
                started_at: p.started_at,
                last_seen: p.last_seen,
                seconds_since_last_seen: elapsed.max(0) as u64,
                stale: elapsed > STALE_AFTER_SECS,
                in_flight: p.in_flight,
                max_concurrency: p.max_concurrency,
                fairness_mode: Some(p.fairness_mode),
                sandbox_backend: p.sandbox_backend,
                version: p.version,
                hostname: p.hostname,
                ip_addresses: p.ip_addresses,
                os: p.os,
                arch: p.arch,
                cpu_count: p.cpu_count,
                pid: p.pid,
            }
        })
        .collect();

    workers.sort_by(|a, b| a.id.cmp(&b.id));
    workers
}

pub(crate) async fn read_queues(state: &AppState) -> Vec<QueueInfo> {
    let Some(mq) = state.mq.as_ref() else {
        return Vec::new();
    };

    let mut queue_names = vec![
        state.config.mq.operation_queue_name.clone(),
        state.config.mq.operation_result_queue_name.clone(),
        state.config.mq.operation_dlq_queue_name.clone(),
    ];

    for worker in read_workers(state).await {
        if !worker.stale {
            queue_names.push(format!(
                "{}:worker:{}",
                state.config.mq.operation_queue_name, worker.id
            ));
        }
    }
    queue_names.sort();
    queue_names.dedup();

    let mut out = Vec::with_capacity(queue_names.len());
    for name in queue_names {
        let breakdown: HashMap<String, u64> = match mq.size(&name).await {
            Ok(map) => map,
            Err(e) => {
                warn!(queue = %name, error = %e, "MQ size lookup failed");
                HashMap::new()
            }
        };
        let depth: u64 = breakdown.values().sum();
        out.push(QueueInfo {
            name,
            depth,
            breakdown,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heartbeat_payload_reads_fairness_mode_and_tolerates_legacy() {
        let with = r#"{"id":"w","started_at":"2026-05-01T00:00:00Z","last_seen":"2026-05-01T00:00:00Z","in_flight":0,"max_concurrency":4,"fairness_mode":"pinned","sandbox_backend":"isolate","version":"0.1.0"}"#;
        let p: HeartbeatPayload = serde_json::from_str(with).unwrap();
        assert_eq!(p.fairness_mode, "pinned");

        // Legacy worker heartbeats predating the field must still parse (the
        // shared payload defaults `fairness_mode` to "unknown").
        let legacy = r#"{"id":"w","started_at":"2026-05-01T00:00:00Z","last_seen":"2026-05-01T00:00:00Z","in_flight":0,"max_concurrency":null,"sandbox_backend":"isolate","version":"0.1.0"}"#;
        let p: HeartbeatPayload = serde_json::from_str(legacy).unwrap();
        assert_eq!(p.fairness_mode, "unknown");
    }

    #[test]
    fn queue_depth_delta_tracks_absolute_samples() {
        let mut previous = HashMap::new();

        let deltas = queue_depth_deltas(
            &mut previous,
            vec![
                QueueDepthSample::new("operation", "operation_tasks", 4),
                QueueDepthSample::new("dlq", "operation_dlq", 1),
            ],
        );
        assert_eq!(
            deltas,
            vec![
                QueueDepthDelta::new("dlq", "operation_dlq", 1),
                QueueDepthDelta::new("operation", "operation_tasks", 4),
            ]
        );

        let deltas = queue_depth_deltas(
            &mut previous,
            vec![QueueDepthSample::new("operation", "operation_tasks", 2)],
        );
        assert_eq!(
            deltas,
            vec![
                QueueDepthDelta::new("dlq", "operation_dlq", -1),
                QueueDepthDelta::new("operation", "operation_tasks", -2),
            ]
        );
    }

    #[test]
    fn submission_in_flight_delta_tracks_state_samples() {
        let mut previous = HashMap::new();

        let deltas = submission_in_flight_deltas(
            &mut previous,
            vec![
                SubmissionInFlightSample::new("submission", "Queued", 4),
                SubmissionInFlightSample::new("submission", "Pending", 2),
            ],
        );
        assert_eq!(
            deltas,
            vec![
                SubmissionInFlightDelta::new("submission", "Pending", 2),
                SubmissionInFlightDelta::new("submission", "Queued", 4),
            ]
        );

        let deltas = submission_in_flight_deltas(
            &mut previous,
            vec![SubmissionInFlightSample::new("submission", "Pending", 1)],
        );
        assert_eq!(
            deltas,
            vec![
                SubmissionInFlightDelta::new("submission", "Pending", -1),
                SubmissionInFlightDelta::new("submission", "Queued", -4),
            ]
        );
    }

    #[test]
    fn submission_judge_queue_depth_delta_tracks_absolute_count() {
        let mut previous = 0;

        assert_eq!(submission_judge_queue_depth_delta(&mut previous, 7), 7);
        assert_eq!(submission_judge_queue_depth_delta(&mut previous, 3), -4);
        assert_eq!(submission_judge_queue_depth_delta(&mut previous, 3), 0);
    }

    #[test]
    fn submission_lifecycle_sampler_metrics_export_contract_labels() {
        let _guard = crate::metrics_test_lock();
        let (metrics, registry) =
            common::observability::init_metrics("broccoli-submission-lifecycle-test");

        metrics.submission_in_flight.add(
            2,
            &[
                KeyValue::new("row.kind", "submission"),
                KeyValue::new("state", "Pending"),
            ],
        );
        metrics
            .submission_judge_queue_depth
            .add(3, &[KeyValue::new("source", "postgres_status_queued")]);
        metrics.submission_age_in_pending_seconds.record(
            4.0,
            &[
                KeyValue::new("row.kind", "submission"),
                KeyValue::new("state", "Pending"),
            ],
        );

        let families = registry.gather();
        let in_flight = families
            .iter()
            .find(|family| family.name() == "broccoli_submission_in_flight")
            .expect("submission in-flight metric should be exported");
        assert!(in_flight.get_metric().iter().any(|metric| {
            metric
                .get_label()
                .iter()
                .any(|label| label.name() == "row_kind" && label.value() == "submission")
        }));
        assert!(in_flight.get_metric().iter().any(|metric| {
            metric
                .get_label()
                .iter()
                .any(|label| label.name() == "state" && label.value() == "Pending")
        }));

        let queue_depth = families
            .iter()
            .find(|family| family.name() == "broccoli_submission_judge_queue_depth")
            .expect("submission judge queue depth metric should be exported");
        assert!(queue_depth.get_metric().iter().any(|metric| {
            metric
                .get_label()
                .iter()
                .any(|label| label.name() == "source" && label.value() == "postgres_status_queued")
        }));

        let pending_age = families
            .iter()
            .find(|family| family.name() == "broccoli_submission_age_in_pending_seconds")
            .expect("submission pending-age metric should be exported");
        assert!(pending_age.get_metric().iter().any(|metric| {
            metric
                .get_label()
                .iter()
                .any(|label| label.name() == "state" && label.value() == "Pending")
        }));
    }
}
