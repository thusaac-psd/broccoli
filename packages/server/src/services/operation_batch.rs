use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use broccoli_server_sdk::types::{
    DetachedOperationCallbackAction, DetachedOperationCallbackEvent,
    DetachedOperationCallbackInput, DetachedOperationCallbackOutput, DetachedOperationSession,
    OperationResult, OperationTask, SessionFile, StartDetachedWindowedOperationInput,
};
use common::storage::BlobStore;
use common::worker::{Task, TaskResult};
use futures::stream::{self, StreamExt, TryStreamExt};
use mq::{MqQueue, config::PublishConfig};
use opentelemetry::KeyValue;
use plugin_core::retry::{PoolRetryPolicy, call_raw_with_pool_retry};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::host_funcs::context::OperationHostDeps;
use crate::registry::{EvaluateCaseRef, OperationBatches, OperationWaiter, OperationWaiters};
use crate::services::batch_table::{BatchSender, Removal};
use crate::services::batch_wait::{BatchWait, WaitCtx, WaitPolicy};
use crate::services::windowed_session::{
    SessionAction, SessionDecision, SlotEvent, SlotOutcome, WindowedSession, run_windowed_session,
};

const INLINE_FILE_BLOB_THRESHOLD_BYTES: usize = 1_048_576;

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait OperationTaskPublisher: Send + Sync {
    async fn publish_operation_task(
        &self,
        target_queue: &str,
        task: &Task,
        publish_config: Option<PublishConfig>,
    ) -> anyhow::Result<()>;
}

pub struct MqOperationTaskPublisher {
    mq: Arc<MqQueue>,
}

impl MqOperationTaskPublisher {
    pub fn new(mq: Arc<MqQueue>) -> Self {
        Self { mq }
    }
}

#[async_trait]
impl OperationTaskPublisher for MqOperationTaskPublisher {
    async fn publish_operation_task(
        &self,
        target_queue: &str,
        task: &Task,
        publish_config: Option<PublishConfig>,
    ) -> anyhow::Result<()> {
        self.mq
            .publish(target_queue, None, task, publish_config)
            .await
            .map(|_| ())
            .with_context(|| "MQ publish error")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationResultE2eLabels {
    task_type: String,
    operation: String,
    worker_id: String,
    outcome: &'static str,
}

fn operation_result_e2e_labels(
    task_result: &TaskResult,
    outcome: &'static str,
) -> OperationResultE2eLabels {
    OperationResultE2eLabels {
        task_type: task_result
            .task_type
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        operation: task_result
            .operation
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        worker_id: task_result
            .worker_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        outcome,
    }
}

fn record_operation_result_e2e(
    metrics: Option<&common::metrics::Metrics>,
    task_result: &TaskResult,
    outcome: &'static str,
) {
    let Some(metrics) = metrics else {
        return;
    };
    let Some(enqueued_at_unix_ms) = task_result.enqueued_at_unix_ms else {
        return;
    };

    let labels = operation_result_e2e_labels(task_result, outcome);
    let now = chrono::Utc::now().timestamp_millis();
    let duration_seconds = now.saturating_sub(enqueued_at_unix_ms) as f64 / 1_000.0;
    let attrs = [
        KeyValue::new("task_type", labels.task_type),
        KeyValue::new("operation", labels.operation),
        KeyValue::new("worker.id", labels.worker_id),
        KeyValue::new("outcome", labels.outcome),
        KeyValue::new("task_success", task_result.success.to_string()),
    ];
    metrics
        .operation_result_e2e_duration
        .record(duration_seconds, &attrs);
}

pub async fn start_operation_batch(
    plugin_id: String,
    deps: OperationHostDeps,
    operations: Vec<OperationTask>,
) -> anyhow::Result<String> {
    // Fast-fail before allocating batch state if MQ isn't configured. Each
    // per-op future also re-checks (since they clone `deps`), but rejecting
    // here preserves the early-return semantic (and avoids opening a batch
    // whose ops can never publish).
    deps.operation_publisher
        .as_ref()
        .ok_or_else(|| anyhow!("MQ not available"))?;

    let batch_id = Uuid::new_v4().to_string();

    let correlation_ids = operations
        .iter()
        .map(|_| Uuid::new_v4().to_string())
        .collect::<Vec<_>>();
    let batch_tx =
        deps.operation_batches
            .open(&batch_id, correlation_ids.clone(), correlation_ids.clone());

    tracing::info!(
        plugin_id = %plugin_id,
        batch_id = %batch_id,
        operation_count = operations.len(),
        "Starting operation batch"
    );

    if let Some(metrics) = deps.metrics.as_ref() {
        metrics.batch_started_total.add(
            1,
            &[
                KeyValue::new("batch.kind", "operation"),
                KeyValue::new("plugin.id", plugin_id.clone()),
            ],
        );
    }

    // Parallelize per-op blob-externalize + mq.publish via
    // buffer_unordered. Each per-op future preserves the waiter-insert ->
    // publish ordering invariant by sequential await inside one
    // closure; cross-op ordering is intentionally unordered. First error
    // short-circuits via try_collect; in-flight peers are dropped.
    let publish_concurrency = deps.operation_batch_publish_concurrency.max(1);
    let per_op_inputs: Vec<(String, OperationTask)> =
        correlation_ids.iter().cloned().zip(operations).collect();

    stream::iter(per_op_inputs.into_iter().map(|(correlation_id, op)| {
        let deps = deps.clone();
        let batch_tx = batch_tx.clone();
        let plugin_id = plugin_id.clone();
        let batch_id = batch_id.clone();
        async move {
            let (op_tx, op_rx) = tokio::sync::oneshot::channel();
            let evaluate_case = op
                .evaluate_batch_id
                .clone()
                .zip(op.test_case_id)
                .map(|(batch_id, test_case_id)| EvaluateCaseRef {
                    batch_id,
                    test_case_id,
                });

            // Waiter insert must precede publish for THIS op, or a fast
            // reply could arrive before anyone is waiting for it. These
            // sync calls happen before the awaits below.
            if let Some(case) = evaluate_case.as_ref() {
                deps.evaluate_batches.record_ops(
                    &case.batch_id,
                    case.test_case_id,
                    &batch_id,
                    vec![correlation_id.clone()],
                );
            }
            deps.operation_waiters.insert(
                correlation_id.clone(),
                OperationWaiter {
                    result_tx: op_tx,
                    evaluate_case,
                },
            );
            spawn_waiter_forwarder(correlation_id.clone(), op_rx, batch_tx);

            // Resolve the target queue for a pinned operation. An invalid worker
            // id, OR a valid id whose worker has no live heartbeat, falls back to
            // the shared queue rather than stranding the task on a dead worker's
            // private queue. Only pinned ops pay the Redis liveness check, and it
            // fails open so a transient Redis hiccup never drops a live pin.
            let effective_target = match op.target_worker_id.as_deref() {
                Some(worker_id) if !crate::config::is_valid_server_id(worker_id) => {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        target = %worker_id,
                        "Rejecting operation with invalid target_worker_id; falling back to shared queue"
                    );
                    None
                }
                Some(worker_id)
                    if !target_worker_is_live(deps.redis_client.as_deref(), worker_id).await =>
                {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        target = %worker_id,
                        "Pinned target worker has no live heartbeat; falling back to shared queue"
                    );
                    None
                }
                other => other,
            };
            let target_queue =
                target_operation_queue(&deps.operation_queue_name, effective_target);

            let op = externalize_large_inline_files(op, deps.blob_store.clone())
                .await
                .with_context(|| "Blob store error")?;

            let task = Task {
                id: correlation_id.clone(),
                task_type: "operation".to_string(),
                executor_name: "operation".to_string(),
                payload: serde_json::to_value(&op)
                    .with_context(|| "Failed to serialize operation")?,
                result_queue: deps.operation_result_queue_name.clone(),
                operation_batch_id: None,
                reply_queue: Some(deps.operation_result_queue_name.clone()),
                priority: normalize_publish_priority(op.priority, &correlation_id),
                trace_context: common::observability::inject_trace_context(),
                enqueued_at_unix_ms: Some(chrono::Utc::now().timestamp_millis()),
            };

            let publisher = deps
                .operation_publisher
                .as_ref()
                .ok_or_else(|| anyhow!("MQ not available"))?;
            let publish_start = Instant::now();
            let publish_result = publisher
                .publish_operation_task(
                    &target_queue,
                    &task,
                    task.priority
                        .map(|p| PublishConfig::builder().priority(p).build()),
                )
                .await;
            record_mq_publish(
                deps.metrics.as_ref(),
                &target_queue,
                "operation_task",
                if publish_result.is_ok() {
                    "success"
                } else {
                    "error"
                },
                publish_start,
            );
            publish_result.with_context(|| "MQ publish error")?;

            tracing::debug!(
                batch_id = %batch_id,
                correlation_id = %correlation_id,
                "Operation dispatched"
            );

            Ok::<(), anyhow::Error>(())
        }
    }))
    .buffer_unordered(publish_concurrency)
    .try_collect::<Vec<()>>()
    .await
    .inspect_err(|_| cleanup_failed_operation_batch(&deps, &batch_id))?;

    Ok(batch_id)
}

/// Remove an operation batch that failed to start, together with its
/// waiters and the operation refs it recorded on evaluate cases.
fn cleanup_failed_operation_batch(deps: &OperationHostDeps, batch_id: &str) {
    let Some(correlation_ids) = deps.operation_batches.remove(batch_id, Removal::Failed) else {
        return;
    };
    for correlation_id in &correlation_ids {
        if let Some((_, waiter)) = deps.operation_waiters.remove(correlation_id)
            && let Some(case) = waiter.evaluate_case
        {
            deps.evaluate_batches.remove_operation_batch(
                &case.batch_id,
                case.test_case_id,
                batch_id,
            );
        }
    }
}

pub fn start_detached_windowed_operation(
    plugin_id: String,
    plugin_manager: Arc<dyn plugin_core::traits::PluginManager>,
    deps: OperationHostDeps,
    input: StartDetachedWindowedOperationInput,
) -> anyhow::Result<DetachedOperationSession> {
    if input.callback_fn.trim().is_empty() {
        return Err(anyhow!("Detached operation callback_fn cannot be empty"));
    }

    let session_id = Uuid::new_v4().to_string();
    let session = DetachedOperationSession {
        session_id: session_id.clone(),
    };
    tokio::spawn(run_detached_windowed_operation(
        plugin_id,
        plugin_manager,
        deps,
        session_id,
        input,
    ));
    Ok(session)
}

/// [`WindowedSession`] driver for detached operation batches. Operation is the
/// plain shape: a `usize` slot index, an MQ round-trip per slot, a raw
/// `TaskResult` decoded into an `OperationResult`, no server-side retry, and no
/// terminal hooks.
struct OperationDriver {
    plugin_id: String,
    session_id: String,
    callback_fn: String,
    plugin_manager: Arc<dyn plugin_core::traits::PluginManager>,
    deps: OperationHostDeps,
}

#[async_trait]
impl WindowedSession for OperationDriver {
    type Item = OperationTask;
    type Raw = TaskResult;
    type Final = OperationResult;
    type SlotId = usize;

    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn slot_id(&self, index: usize, _item: &OperationTask) -> usize {
        index
    }

    async fn start_slot(
        &self,
        slot_id: usize,
        item: OperationTask,
        timeout: Duration,
        tx: mpsc::Sender<SlotOutcome<usize, TaskResult>>,
    ) -> anyhow::Result<String> {
        let batch_id =
            start_operation_batch(self.plugin_id.clone(), self.deps.clone(), vec![item]).await?;
        let wait_batch_id = batch_id.clone();
        let wait_plugin_id = self.plugin_id.clone();
        let deps = self.deps.clone();
        tokio::spawn(async move {
            // Await the result as a cheap future instead of occupying a blocking
            // thread for the op's whole lifetime.
            let result = next_operation_result_async(
                &wait_plugin_id,
                &deps.operation_batches,
                &wait_batch_id,
                timeout,
            )
            .await;
            let _ = tx
                .send(SlotOutcome {
                    slot_id,
                    batch_id: wait_batch_id,
                    result,
                })
                .await;
        });
        Ok(batch_id)
    }

    fn decode(&self, raw: TaskResult) -> Result<OperationResult, String> {
        operation_result_from_task_result(raw).map_err(|e| e.to_string())
    }

    fn timeout_message(&self, slot_id: usize) -> String {
        format!("Operation {slot_id} timed out")
    }

    async fn call_callback(
        &self,
        event: SlotEvent<usize, OperationResult>,
        state: serde_json::Value,
        completed: usize,
        total: usize,
    ) -> anyhow::Result<SessionDecision<usize>> {
        let cb_event = match event {
            SlotEvent::Result { slot_id, result } => DetachedOperationCallbackEvent::Result {
                operation_index: slot_id,
                result,
            },
            SlotEvent::Timeout { message } => DetachedOperationCallbackEvent::Timeout { message },
            SlotEvent::Exhausted => DetachedOperationCallbackEvent::Exhausted,
        };
        let input = DetachedOperationCallbackInput {
            session_id: self.session_id.clone(),
            state,
            event: cb_event,
            completed,
            total,
        };
        let output = call_detached_operation_callback(
            self.plugin_manager.as_ref(),
            &self.plugin_id,
            &self.callback_fn,
            input,
        )
        .await?;
        Ok(SessionDecision {
            state: output.state,
            action: match output.action {
                DetachedOperationCallbackAction::Continue => SessionAction::Continue,
                DetachedOperationCallbackAction::Finish => SessionAction::Finish,
                DetachedOperationCallbackAction::Cancel => SessionAction::Cancel,
            },
            refill: output.refill,
            cancel_ids: output.cancel_operation_indices,
        })
    }

    async fn cancel_active(&self, active: &[(usize, String)]) {
        cancel_active_operation_slots(&self.plugin_id, &self.deps, active);
    }

    async fn cancel_selected(&self, active: &mut Vec<(usize, String)>, ids: &HashSet<usize>) {
        cancel_operation_indices(&self.plugin_id, &self.deps, active, ids);
    }
}

async fn run_detached_windowed_operation(
    plugin_id: String,
    plugin_manager: Arc<dyn plugin_core::traits::PluginManager>,
    deps: OperationHostDeps,
    session_id: String,
    input: StartDetachedWindowedOperationInput,
) {
    let StartDetachedWindowedOperationInput {
        operations,
        concurrency,
        result_timeout_ms,
        callback_fn,
        state,
    } = input;
    let driver = OperationDriver {
        plugin_id,
        session_id,
        callback_fn,
        plugin_manager,
        deps,
    };
    run_windowed_session(
        driver,
        operations,
        concurrency,
        Duration::from_millis(result_timeout_ms),
        state,
    )
    .await;
}

async fn call_detached_operation_callback(
    plugin_manager: &dyn plugin_core::traits::PluginManager,
    plugin_id: &str,
    callback_fn: &str,
    input: DetachedOperationCallbackInput,
) -> anyhow::Result<DetachedOperationCallbackOutput> {
    let input_bytes = serde_json::to_vec(&input)
        .with_context(|| "Failed to serialize detached operation callback input")?;
    let output_bytes = call_raw_with_pool_retry(
        plugin_manager,
        plugin_id,
        callback_fn,
        input_bytes,
        PoolRetryPolicy::default(),
    )
    .await
    .with_context(|| "Detached operation callback failed")?;
    serde_json::from_slice::<DetachedOperationCallbackOutput>(&output_bytes)
        .with_context(|| "Failed to deserialize detached operation callback output")
}

fn operation_result_from_task_result(task_result: TaskResult) -> anyhow::Result<OperationResult> {
    serde_json::from_value::<OperationResult>(task_result.output).map_err(|_| {
        anyhow!(
            "{}",
            task_result
                .error
                .unwrap_or_else(|| "Worker did not return an OperationResult".to_string())
        )
    })
}

fn cancel_active_operation_slots(
    plugin_id: &str,
    deps: &OperationHostDeps,
    active: &[(usize, String)],
) {
    for (_, batch_id) in active {
        cancel_operation_batch(
            plugin_id,
            &deps.operation_batches,
            &deps.operation_waiters,
            batch_id,
        );
    }
}

fn cancel_operation_indices(
    plugin_id: &str,
    deps: &OperationHostDeps,
    active: &mut Vec<(usize, String)>,
    indices: &HashSet<usize>,
) {
    let mut cancelled = Vec::new();
    active.retain(|(idx, batch_id)| {
        if indices.contains(idx) {
            cancel_operation_batch(
                plugin_id,
                &deps.operation_batches,
                &deps.operation_waiters,
                batch_id,
            );
            cancelled.push(*idx);
            false
        } else {
            true
        }
    });
    if !cancelled.is_empty() {
        tracing::debug!(?cancelled, "Detached operation slots cancelled by callback");
    }
}

const OPERATION_RESULT_WAIT_TICK: Duration = Duration::from_millis(50);

/// Minimum time an in-flight operation is allowed before the result-wait gives
/// up, independent of the (small) solution-derived `timeout`. Large in
/// production so a slow / backed-up worker yields SLOW results, not failures -
/// the operation's real time limit is enforced inside isolate, and a genuinely
/// dead worker is reclaimed by the dispatcher lease/steal. Without this, a deep
/// queue or cold-blob IO under load hard-times-out every operation at the small
/// budget and mass-fails submissions. Zero in tests so the explicit `timeout`
/// still governs.
#[cfg(not(test))]
fn operation_infra_floor() -> Duration {
    Duration::from_secs(30 * 60)
}
#[cfg(test)]
fn operation_infra_floor() -> Duration {
    Duration::from_secs(0)
}

/// The operation batch result-wait policy: wait while the operation is still
/// in flight (pending), up to a large infrastructure ceiling, instead of a hard
/// timeout at the small budget. A queued op (waiting for a backed-up worker) or
/// a slowly-executing op thus degrades to a slow result, never a spurious
/// failure.
struct OperationWait {
    ceiling: Duration,
}

impl WaitPolicy<TaskResult, Vec<String>> for OperationWait {
    const DISCONNECTED: &'static str = "Batch channel disconnected";

    fn next_tick(&self, waited: Duration) -> Option<Duration> {
        (waited < self.ceiling).then(|| (self.ceiling - waited).min(OPERATION_RESULT_WAIT_TICK))
    }

    // Pending means the operation is in flight: keep waiting (system
    // slowness) and give up only once it is no longer pending (accounted for,
    // no result) or the ceiling is reached.
    fn extend(&self, _wait: &WaitCtx<'_, TaskResult, Vec<String>>) -> bool {
        true
    }

    fn on_delivered(&self, wait: &WaitCtx<'_, TaskResult, Vec<String>>, task_result: &TaskResult) {
        record_operation_result_e2e(wait.metrics, task_result, "delivered");
        wait.record_outcome(
            "result",
            Some(KeyValue::new(
                "task_success",
                task_result.success.to_string(),
            )),
        );
        tracing::debug!(
            plugin_id = %wait.plugin_id,
            batch_id = %wait.batch_id,
            task_id = %task_result.task_id,
            "Operation result received"
        );
    }
}

fn operation_wait<'a>(
    plugin_id: &'a str,
    batches: &'a OperationBatches,
    batch_id: &'a str,
    timeout: Duration,
) -> anyhow::Result<BatchWait<'a, TaskResult, Vec<String>, OperationWait>> {
    let ceiling = timeout.max(operation_infra_floor());
    BatchWait::open(plugin_id, batches, batch_id, OperationWait { ceiling })
}

/// Wait for the batch's next operation result, blocking the calling thread.
/// Kept for the Extism host-function boundary, which runs in a sync plugin
/// context and cannot await.
pub fn next_operation_result(
    plugin_id: &str,
    batches: &OperationBatches,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TaskResult>> {
    operation_wait(plugin_id, batches, batch_id, timeout)?.blocking()
}

/// Async sibling of [`next_operation_result`]. Awaits the result via flume's
/// `recv_async`, so a waiting detached-driver slot is a cheap future instead of
/// a `spawn_blocking` OS thread - this is what lets the server hold thousands of
/// in-flight waits without the thread blow-up. On a dropped sender (the batch was
/// removed because the submission was cancelled/superseded) the await returns
/// `Disconnected` immediately, so a superseded wait never lingers to the infra
/// ceiling.
pub async fn next_operation_result_async(
    plugin_id: &str,
    batches: &OperationBatches,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TaskResult>> {
    operation_wait(plugin_id, batches, batch_id, timeout)?
        .wait()
        .await
}

pub fn cancel_operation_batch(
    plugin_id: &str,
    batches: &OperationBatches,
    waiters: &OperationWaiters,
    batch_id: &str,
) {
    if let Some(correlation_ids) = batches.remove(batch_id, Removal::Cancelled { plugin_id }) {
        release_waiters(waiters, &correlation_ids);
    }

    tracing::info!(
        plugin_id = %plugin_id,
        batch_id = %batch_id,
        "Operation batch cancelled"
    );
}

/// Drop the waiters of a batch that will not take their results.
pub fn release_waiters(waiters: &OperationWaiters, correlation_ids: &[String]) {
    for correlation_id in correlation_ids {
        waiters.remove(correlation_id);
    }
}

/// Forward one operation's result into its batch. A dropped waiter (cancelled
/// batch, removed by the reaper) still delivers an error result, so the batch
/// always receives exactly one result per operation.
fn spawn_waiter_forwarder(
    correlation_id: String,
    op_rx: tokio::sync::oneshot::Receiver<TaskResult>,
    batch_tx: BatchSender<TaskResult>,
) {
    tokio::spawn(async move {
        let result = op_rx.await.unwrap_or_else(|_| TaskResult {
            task_id: correlation_id,
            success: false,
            output: serde_json::json!({}),
            error: Some("Operation cancelled or timed out".into()),
            task_type: Some("operation".to_string()),
            operation: Some("operation".to_string()),
            worker_id: None,
            enqueued_at_unix_ms: None,
        });
        batch_tx.deliver(result);
    });
}

fn target_operation_queue(shared_queue: &str, target_worker_id: Option<&str>) -> String {
    match target_worker_id {
        Some(worker_id) if crate::config::is_valid_server_id(worker_id) => {
            common::worker::worker_private_queue_name(shared_queue, worker_id)
        }
        _ => shared_queue.to_string(),
    }
}

/// Whether a pinned target worker currently has a live heartbeat in Redis.
/// Worker heartbeat keys carry a 15s TTL (see the worker heartbeat writer), so a
/// simple `EXISTS` is a fresh-liveness signal. Fails OPEN: a missing Redis handle
/// or a lookup error returns `true` (keep the pin), so a transient Redis hiccup
/// never mis-routes a legitimately pinned task off its target worker.
async fn target_worker_is_live(redis: Option<&redis::Client>, worker_id: &str) -> bool {
    let Some(client) = redis else {
        return true;
    };
    let Ok(mut conn) = client.get_multiplexed_async_connection().await else {
        return true;
    };
    let key = format!("{}{worker_id}", common::worker::WORKER_HEARTBEAT_KEY_PREFIX);
    let exists: Result<i64, redis::RedisError> =
        redis::cmd("EXISTS").arg(&key).query_async(&mut conn).await;
    match exists {
        Ok(n) => n > 0,
        // Fail OPEN (see doc above): a lookup error keeps the pin rather than
        // mis-routing a legitimately pinned task off its target worker on a
        // transient Redis hiccup. `matches!(exists, Ok(n) if n > 0)` silently
        // failed CLOSED, contradicting this function's documented contract.
        Err(_) => true,
    }
}

/// Clamp a plugin-authored operation priority into broccoli_queue's valid
/// `1..=5` range (1 = highest, 5 = lowest). `PublishConfig::builder().priority`
/// `assert!`s the value is in range and PANICS otherwise; the priority comes
/// straight off a plugin's `OperationTask`, so a stray `0` or `>5` would abort
/// the publish and, on the detached path, kill the whole windowed session
/// (every pending op then stalls to the 30-minute ceiling). Clamp instead of
/// panicking, and warn so the plugin bug stays visible.
fn normalize_publish_priority(priority: Option<u8>, correlation_id: &str) -> Option<u8> {
    priority.map(|p| {
        let clamped = p.clamp(1, 5);
        if clamped != p {
            tracing::warn!(
                correlation_id,
                requested = p,
                clamped,
                "operation priority out of broccoli_queue's 1..=5 range; clamped to avoid a publish panic"
            );
        }
        clamped
    })
}

async fn externalize_large_inline_files(
    mut op: OperationTask,
    blob_store: Arc<dyn BlobStore>,
) -> Result<OperationTask, common::storage::StorageError> {
    let mut replaced = 0usize;
    let mut replaced_bytes = 0usize;

    for env in &mut op.environments {
        for (_path, file) in &mut env.files_in {
            let SessionFile::Content { content } = file else {
                continue;
            };
            if content.len() < INLINE_FILE_BLOB_THRESHOLD_BYTES {
                continue;
            }

            let hash = blob_store.put(content.as_bytes()).await?;
            replaced += 1;
            replaced_bytes += content.len();
            *file = SessionFile::Blob {
                hash: hash.to_hex(),
            };
        }
    }

    if replaced > 0 {
        tracing::info!(
            replaced,
            replaced_bytes,
            "Externalized large inline operation files to blob storage"
        );
    }

    Ok(op)
}

fn record_mq_publish(
    metrics: Option<&common::metrics::Metrics>,
    queue: &str,
    message_type: &'static str,
    outcome: &'static str,
    start: Instant,
) {
    let Some(metrics) = metrics else {
        return;
    };
    let attrs = [
        KeyValue::new("queue", queue.to_string()),
        KeyValue::new("message.type", message_type),
        KeyValue::new("outcome", outcome),
    ];
    metrics
        .mq_publish_duration
        .record(start.elapsed().as_secs_f64(), &attrs);
    metrics.mq_publish_messages_total.add(1, &attrs);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use broccoli_server_sdk::types::{Environment, OperationTask, SessionFile};
    use common::storage::BlobStore;
    use common::storage::filesystem::FilesystemBlobStore;

    use super::*;

    fn dummy_task_result(task_id: &str) -> TaskResult {
        TaskResult {
            task_id: task_id.to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: None,
            operation: None,
            worker_id: None,
            enqueued_at_unix_ms: None,
        }
    }

    #[tokio::test]
    async fn async_wait_delivers_a_result() {
        let batches = OperationBatches::default();
        let tx = batches.open("b1", ["op-1".to_string()], Vec::new());
        tx.deliver(dummy_task_result("op-1"));

        let got = next_operation_result_async("p", &batches, "b1", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(got.unwrap().task_id, "op-1");
        assert!(!batches.contains("b1"), "the drained batch is removed");
    }

    #[tokio::test]
    async fn async_wait_drains_buffered_result_when_giving_up() {
        // Each result is buffered BEFORE the pending count drops, so the
        // instant the wait observes the batch as settled the result is already
        // in the channel. A give-up that returns `Ok(None)` without draining
        // drops it, which `wait_for_operation_results` then maps to a "timed
        // out" SystemError. `Duration::ZERO` (ceiling 0 via the test infra
        // floor) forces the give-up branch on the first poll.
        let batches = OperationBatches::default();
        let tx = batches.open("b1", ["op-1".to_string()], Vec::new());
        tx.deliver(dummy_task_result("op-1"));

        let got = next_operation_result_async("p", &batches, "b1", Duration::ZERO)
            .await
            .unwrap();
        assert_eq!(
            got.expect("a delivered result must never be dropped by a give-up branch")
                .task_id,
            "op-1"
        );
    }

    #[test]
    fn sync_wait_drains_buffered_result_when_giving_up() {
        // Sync sibling of the give-up drain (the Extism host-fn boundary).
        let batches = OperationBatches::default();
        let tx = batches.open("b1", ["op-1".to_string()], Vec::new());
        tx.deliver(dummy_task_result("op-1"));

        let got = next_operation_result("p", &batches, "b1", Duration::ZERO).unwrap();
        assert_eq!(
            got.expect("a delivered result must never be dropped by a give-up branch")
                .task_id,
            "op-1"
        );
    }

    #[tokio::test]
    async fn async_wait_ends_promptly_on_dropped_sender() {
        // An orphaned op (sender dropped) must end the async wait at once, not
        // linger to the infra ceiling. The batch stays pending, so only the
        // disconnect can end it.
        let batches = OperationBatches::default();
        drop(batches.open("b2", ["op-1".to_string()], Vec::new()));

        let outcome =
            next_operation_result_async("p", &batches, "b2", Duration::from_secs(30)).await;
        assert!(outcome.is_err(), "dropped sender surfaces as an error");
    }

    #[test]
    fn operation_result_e2e_labels_prefer_result_metadata() {
        let result = TaskResult {
            task_id: "op-1".to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: Some("operation".to_string()),
            operation: Some("compile".to_string()),
            worker_id: Some("worker-a".to_string()),
            enqueued_at_unix_ms: Some(1_234),
        };

        let labels = operation_result_e2e_labels(&result, "delivered");

        assert_eq!(labels.task_type, "operation");
        assert_eq!(labels.operation, "compile");
        assert_eq!(labels.worker_id, "worker-a");
        assert_eq!(labels.outcome, "delivered");
    }

    fn operation_with_file(file: SessionFile) -> OperationTask {
        OperationTask {
            environments: vec![Environment {
                id: "env".to_string(),
                files_in: vec![("input.txt".to_string(), file)],
            }],
            tasks: vec![],
            channels: vec![],
            priority: None,
            target_worker_id: None,
            evaluate_batch_id: None,
            test_case_id: None,
        }
    }

    #[test]
    fn invalid_target_worker_falls_back_to_shared_queue() {
        assert_eq!(target_operation_queue("ops", Some("../bad")), "ops");
        assert_eq!(target_operation_queue("ops", None), "ops");
    }

    #[test]
    fn valid_target_worker_uses_private_queue() {
        assert_eq!(
            target_operation_queue("ops", Some("worker-1")),
            "ops:worker:worker-1"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn target_worker_is_live_reflects_the_heartbeat_key() {
        use testcontainers::runners::AsyncRunner;
        use testcontainers_modules::redis::Redis;

        let container = Redis::default().start().await.expect("start redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port");
        let client =
            redis::Client::open(format!("redis://127.0.0.1:{port}")).expect("redis client");
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .expect("connect");

        // Mirror the worker heartbeat writer: a key with a TTL under the prefix.
        let key = format!(
            "{}worker-alive",
            common::worker::WORKER_HEARTBEAT_KEY_PREFIX
        );
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg("{}")
            .arg("EX")
            .arg(15)
            .query_async(&mut conn)
            .await
            .expect("seed heartbeat");

        assert!(
            target_worker_is_live(Some(&client), "worker-alive").await,
            "a worker with a live heartbeat key is live"
        );
        assert!(
            !target_worker_is_live(Some(&client), "worker-dead").await,
            "a worker with no heartbeat key is not live"
        );
        assert!(
            target_worker_is_live(None, "anything").await,
            "no Redis handle fails open so a legitimately pinned task keeps its target"
        );
    }

    #[test]
    fn normalize_publish_priority_clamps_out_of_range() {
        // None stays None; in-range passes through; 0 and >5 clamp into 1..=5 so
        // the broccoli_queue `assert!(1..=5)` in `.priority()` cannot panic.
        assert_eq!(normalize_publish_priority(None, "cid"), None);
        assert_eq!(normalize_publish_priority(Some(1), "cid"), Some(1));
        assert_eq!(normalize_publish_priority(Some(5), "cid"), Some(5));
        assert_eq!(normalize_publish_priority(Some(3), "cid"), Some(3));
        assert_eq!(normalize_publish_priority(Some(0), "cid"), Some(1));
        assert_eq!(normalize_publish_priority(Some(6), "cid"), Some(5));
        assert_eq!(normalize_publish_priority(Some(u8::MAX), "cid"), Some(5));
    }

    #[tokio::test]
    async fn externalizes_large_inline_files() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(
            FilesystemBlobStore::new(temp.path().to_path_buf(), 2_000_000)
                .await
                .unwrap(),
        );
        let content = "x".repeat(INLINE_FILE_BLOB_THRESHOLD_BYTES);

        let op = externalize_large_inline_files(
            operation_with_file(SessionFile::Content {
                content: content.clone(),
            }),
            store.clone(),
        )
        .await
        .unwrap();

        let SessionFile::Blob { hash } = &op.environments[0].files_in[0].1 else {
            panic!("large inline file should be replaced with a blob reference");
        };
        assert!(
            store
                .exists(&common::storage::ContentHash::compute(content.as_bytes()))
                .await
                .unwrap()
        );
        assert!(!hash.is_empty());
    }

    fn deps(
        blob_store: Arc<dyn BlobStore>,
        operation_batches: OperationBatches,
        operation_waiters: OperationWaiters,
        evaluate_batches: crate::registry::EvaluateBatches,
    ) -> OperationHostDeps {
        OperationHostDeps {
            plugin_manager: None,
            operation_publisher: None,
            operation_batches,
            operation_waiters,
            operation_queue_name: "ops".to_string(),
            operation_result_queue_name: "ops-results".to_string(),
            blob_store,
            metrics: None,
            evaluate_batches,
            operation_batch_publish_concurrency: 32,
            redis_client: None,
        }
    }

    async fn blob_store(temp: &tempfile::TempDir) -> Arc<dyn BlobStore> {
        Arc::new(
            FilesystemBlobStore::new(temp.path().to_path_buf(), 2_000_000)
                .await
                .unwrap(),
        )
    }

    fn waiter(evaluate_case: Option<EvaluateCaseRef>) -> OperationWaiter {
        OperationWaiter {
            result_tx: tokio::sync::oneshot::channel().0,
            evaluate_case,
        }
    }

    #[tokio::test]
    async fn start_operation_batch_fails_when_mq_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let deps = deps(
            blob_store(&temp).await,
            OperationBatches::default(),
            Arc::default(),
            Default::default(),
        );

        let err = start_operation_batch("plugin".to_string(), deps, vec![])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("MQ not available"));
    }

    #[tokio::test]
    async fn failed_operation_batch_cleanup_removes_batch_waiters_and_eval_refs() {
        let temp = tempfile::tempdir().unwrap();
        let operation_batches = OperationBatches::default();
        let operation_waiters: OperationWaiters = Arc::default();
        let evaluate_batches = crate::registry::EvaluateBatches::default();
        evaluate_batches.open(
            "eval-1",
            ["10".to_string()],
            crate::services::evaluate_batch::EvaluateCases::new([10]),
        );
        let correlation_ids = vec!["op-1".to_string(), "op-2".to_string()];
        operation_batches.open("batch-1", correlation_ids.clone(), correlation_ids.clone());
        for id in &correlation_ids {
            evaluate_batches.record_ops("eval-1", 10, "batch-1", vec![id.clone()]);
            operation_waiters.insert(
                id.clone(),
                waiter(Some(EvaluateCaseRef {
                    batch_id: "eval-1".into(),
                    test_case_id: 10,
                })),
            );
        }
        evaluate_batches.record_ops("eval-1", 10, "batch-0", vec!["kept".into()]);

        let deps = deps(
            blob_store(&temp).await,
            operation_batches.clone(),
            operation_waiters.clone(),
            evaluate_batches.clone(),
        );
        cleanup_failed_operation_batch(&deps, "batch-1");

        assert!(!operation_batches.contains("batch-1"));
        assert!(operation_waiters.is_empty());
        assert_eq!(evaluate_batches.operation_task_ids("eval-1"), vec!["kept"]);
    }

    #[test]
    fn cancel_operation_batch_removes_waiters() {
        let batches = OperationBatches::default();
        let waiters: OperationWaiters = Arc::default();
        batches.open(
            "batch-1",
            ["task-1".to_string()],
            vec!["task-1".to_string()],
        );
        waiters.insert("task-1".to_string(), waiter(None));

        cancel_operation_batch("plugin", &batches, &waiters, "batch-1");

        assert!(!batches.contains("batch-1"));
        assert!(waiters.get("task-1").is_none());
    }
}
