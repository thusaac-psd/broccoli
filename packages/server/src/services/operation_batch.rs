use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
use opentelemetry::KeyValue;
use plugin_core::retry::{PoolRetryPolicy, call_raw_with_pool_retry};
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::host_funcs::context::OperationHostDeps;
use crate::registry::{BatchState, OperationBatches, OperationWaiter, OperationWaiters};
use crate::services::batch_wait::{BatchWait, WaitCtx, WaitPolicy};
use crate::services::windowed_session::{
    SessionAction, SessionDecision, SlotEvent, SlotOutcome, WindowedSession, run_windowed_session,
};

const INLINE_FILE_BLOB_THRESHOLD_BYTES: usize = 1_048_576;

#[async_trait]
pub trait OperationTaskPublisher: Send + Sync {
    /// Publish `task`, pinned to worker `pin` while that worker is live.
    async fn publish_operation_task(
        &self,
        task: &Task,
        pin: Option<&str>,
    ) -> anyhow::Result<mq::Route>;
}

#[async_trait]
impl OperationTaskPublisher for mq::Mq {
    async fn publish_operation_task(
        &self,
        task: &Task,
        pin: Option<&str>,
    ) -> anyhow::Result<mq::Route> {
        Ok(self.publish_task(task, pin).await?)
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

/// Record an operation's time from its publish to its result's arrival, both
/// on this server's monotonic clock.
pub(crate) fn record_operation_result_e2e(
    metrics: &common::metrics::Metrics,
    task_result: &TaskResult,
    outcome: &'static str,
    elapsed: Duration,
) {
    let labels = operation_result_e2e_labels(task_result, outcome);
    let duration_seconds = elapsed.as_secs_f64();
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
    // here preserves the early-return semantic (and avoids registering a
    // BatchState whose ops can never publish).
    deps.operation_publisher
        .as_ref()
        .ok_or_else(|| anyhow!("MQ not available"))?;

    let batch_id = Uuid::new_v4().to_string();

    let (batch_tx, batch_rx) = flume::unbounded();
    let pending_count = Arc::new(AtomicUsize::new(operations.len()));
    let cleanup_keys = Arc::new(
        operations
            .iter()
            .map(|_| Uuid::new_v4().to_string())
            .collect::<Vec<_>>(),
    );
    let evaluate_refs = operations
        .iter()
        .filter_map(|op| op.evaluate_batch_id.clone().zip(op.test_case_id))
        .collect::<Vec<_>>();

    deps.operation_batches.insert(
        batch_id.clone(),
        BatchState {
            result_rx: batch_rx,
            pending_count: pending_count.clone(),
            created_at: Instant::now(),
            cleanup_keys: cleanup_keys.clone(),
            poisoned: AtomicBool::new(false),
        },
    );

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
        metrics
            .batch_active
            .add(1, &[KeyValue::new("batch.kind", "operation")]);
    }

    // Parallelize per-op blob-externalize + mq.publish via
    // buffer_unordered. Each per-op future preserves the waiter-insert ->
    // publish ordering invariant by sequential await inside one
    // closure; cross-op ordering is intentionally unordered. First error
    // short-circuits via try_collect; in-flight peers are dropped.
    let publish_concurrency = deps.operation_batch_publish_concurrency.max(1);
    let per_op_inputs: Vec<(String, OperationTask)> =
        cleanup_keys.iter().cloned().zip(operations).collect();

    stream::iter(per_op_inputs.into_iter().map(|(correlation_id, op)| {
        let deps = deps.clone();
        let batch_tx = batch_tx.clone();
        let pending_count = pending_count.clone();
        let plugin_id = plugin_id.clone();
        let batch_id = batch_id.clone();
        async move {
            let (op_tx, op_rx) = tokio::sync::oneshot::channel();

            // Waiter insert must precede publish for THIS op, or a fast
            // reply could arrive before anyone is waiting for it. These
            // two sync calls happen before the awaits below.
            deps.operation_waiters
                .insert(correlation_id.clone(), OperationWaiter::new(op_tx));
            spawn_waiter_forwarder(
                correlation_id.clone(),
                op_rx,
                batch_tx,
                pending_count,
                deps.metrics.clone(),
            );

            // A pin is a cache-locality hint: an invalid worker id is dropped,
            // and the queue routes a pin for a dead worker to the shared stream.
            let pin = match op.target_worker_id.as_deref() {
                Some(worker_id) if !crate::config::is_valid_server_id(worker_id) => {
                    tracing::warn!(
                        plugin_id = %plugin_id,
                        target = %worker_id,
                        "Ignoring an invalid target_worker_id"
                    );
                    None
                }
                other => other.map(str::to_string),
            };

            if let (Some(eval_batch_id), Some(test_case_id)) =
                (op.evaluate_batch_id.clone(), op.test_case_id)
            {
                deps.evaluate_ops_registry.record_ops(
                    &eval_batch_id,
                    test_case_id,
                    &batch_id,
                    std::iter::once(correlation_id.clone()),
                );
            }

            let op = externalize_large_inline_files(op, deps.blob_store.clone())
                .await
                .with_context(|| "Blob store error")?;

            let task = Task {
                id: correlation_id.clone(),
                task_type: "operation".to_string(),
                executor_name: "operation".to_string(),
                payload: serde_json::to_value(&op)
                    .with_context(|| "Failed to serialize operation")?,
                reply_to: deps.server_id.clone(),
                operation_batch_id: None,
                trace_context: common::observability::inject_trace_context(),
            };

            let publisher = deps
                .operation_publisher
                .as_ref()
                .ok_or_else(|| anyhow!("MQ not available"))?;
            let publish_start = Instant::now();
            let publish_result = publisher
                .publish_operation_task(&task, pin.as_deref())
                .await;
            record_mq_publish(
                deps.metrics.as_ref(),
                match &publish_result {
                    Ok(mq::Route::Pinned) => mq::StreamRole::Private,
                    Ok(mq::Route::Shared) | Err(_) => mq::StreamRole::Shared,
                },
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
    .inspect_err(|_| {
        cleanup_failed_operation_batch(
            &deps,
            &batch_id,
            cleanup_keys.as_ref(),
            &evaluate_refs,
            plugin_id.as_str(),
        );
    })?;

    Ok(batch_id)
}

fn cleanup_failed_operation_batch(
    deps: &OperationHostDeps,
    batch_id: &str,
    cleanup_keys: &[String],
    evaluate_refs: &[(String, i32)],
    plugin_id: &str,
) {
    deps.operation_batches.remove(batch_id);
    for correlation_id in cleanup_keys {
        deps.operation_waiters.remove(correlation_id);
    }
    for (evaluate_batch_id, test_case_id) in evaluate_refs {
        deps.evaluate_ops_registry.remove_operation_batch(
            evaluate_batch_id,
            *test_case_id,
            batch_id,
        );
    }
    if let Some(metrics) = deps.metrics.as_ref() {
        metrics.batch_active.add(
            -1,
            &[
                KeyValue::new("batch.kind", "operation"),
                KeyValue::new("plugin.id", plugin_id.to_string()),
            ],
        );
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
            let batches = deps.operation_batches.clone();
            let metrics = deps.metrics.clone();
            // Await the result as a cheap future instead of occupying a blocking
            // thread for the op's whole lifetime.
            let result = next_operation_result_async(
                &wait_plugin_id,
                &batches,
                metrics.as_ref(),
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
            deps.metrics.as_ref(),
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
                deps.metrics.as_ref(),
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

impl WaitPolicy<TaskResult> for OperationWait {
    const KIND: &'static str = "operation";
    const DISCONNECTED: &'static str = "Batch channel disconnected";

    fn next_tick(&self, waited: Duration) -> Option<Duration> {
        (waited < self.ceiling).then(|| (self.ceiling - waited).min(OPERATION_RESULT_WAIT_TICK))
    }

    // Pending means the operation is in flight: keep waiting (system
    // slowness) and give up only once it is no longer pending (accounted for,
    // no result) or the ceiling is reached.
    fn extend(&self, _wait: &WaitCtx<'_, TaskResult>) -> bool {
        true
    }

    fn on_delivered(&self, wait: &WaitCtx<'_, TaskResult>, task_result: &TaskResult) {
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
    metrics: Option<&'a common::metrics::Metrics>,
    batch_id: &'a str,
    timeout: Duration,
) -> anyhow::Result<BatchWait<'a, TaskResult, OperationWait>> {
    let ceiling = timeout.max(operation_infra_floor());
    BatchWait::open(
        plugin_id,
        batches,
        metrics,
        batch_id,
        OperationWait { ceiling },
    )
}

/// Wait for the batch's next operation result, blocking the calling thread.
/// Kept for the Extism host-function boundary, which runs in a sync plugin
/// context and cannot await.
pub fn next_operation_result(
    plugin_id: &str,
    batches: &OperationBatches,
    metrics: Option<&common::metrics::Metrics>,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TaskResult>> {
    operation_wait(plugin_id, batches, metrics, batch_id, timeout)?.blocking()
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
    metrics: Option<&common::metrics::Metrics>,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TaskResult>> {
    operation_wait(plugin_id, batches, metrics, batch_id, timeout)?
        .wait()
        .await
}

pub fn cancel_operation_batch(
    plugin_id: &str,
    batches: &OperationBatches,
    waiters: &OperationWaiters,
    metrics: Option<&common::metrics::Metrics>,
    batch_id: &str,
) {
    if let Some((_, batch)) = batches.remove(batch_id) {
        for key in batch.cleanup_keys.iter() {
            waiters.remove(key);
        }
        if let Some(metrics) = metrics {
            let attrs = [
                KeyValue::new("batch.kind", "operation"),
                KeyValue::new("plugin.id", plugin_id.to_string()),
            ];
            metrics.batch_cancelled_total.add(1, &attrs);
            metrics
                .batch_active
                .add(-1, &[KeyValue::new("batch.kind", "operation")]);
        }
    }

    tracing::info!(
        plugin_id = %plugin_id,
        batch_id = %batch_id,
        "Operation batch cancelled"
    );
}

fn spawn_waiter_forwarder(
    correlation_id: String,
    op_rx: tokio::sync::oneshot::Receiver<TaskResult>,
    batch_tx: flume::Sender<TaskResult>,
    pending_count: Arc<AtomicUsize>,
    metrics: Option<common::metrics::Metrics>,
) {
    if let Some(metrics) = metrics.as_ref() {
        metrics
            .batch_pending_items
            .add(1, &[KeyValue::new("batch.kind", "operation")]);
    }

    tokio::spawn(async move {
        match op_rx.await {
            Ok(result) => {
                let _ = batch_tx.send(result);
                pending_count.fetch_sub(1, Ordering::SeqCst);
                if let Some(metrics) = metrics.as_ref() {
                    metrics
                        .batch_pending_items
                        .add(-1, &[KeyValue::new("batch.kind", "operation")]);
                }
            }
            Err(_) => {
                let error_result = TaskResult {
                    task_id: correlation_id,
                    success: false,
                    output: serde_json::json!({}),
                    error: Some("Operation cancelled or timed out".into()),
                    task_type: Some("operation".to_string()),
                    operation: Some("operation".to_string()),
                    worker_id: None,
                };
                let _ = batch_tx.send(error_result);
                pending_count.fetch_sub(1, Ordering::SeqCst);
                if let Some(metrics) = metrics.as_ref() {
                    metrics
                        .batch_pending_items
                        .add(-1, &[KeyValue::new("batch.kind", "operation")]);
                }
            }
        }
    });
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
    stream: mq::StreamRole,
    message_type: &'static str,
    outcome: &'static str,
    start: Instant,
) {
    let Some(metrics) = metrics else {
        return;
    };
    let attrs = [
        KeyValue::new("queue", stream.as_str()),
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

    fn test_operation_batch(
        rx: flume::Receiver<TaskResult>,
        pending: usize,
    ) -> BatchState<TaskResult> {
        BatchState {
            result_rx: rx,
            pending_count: Arc::new(std::sync::atomic::AtomicUsize::new(pending)),
            created_at: Instant::now(),
            cleanup_keys: Arc::new(Vec::new()),
            poisoned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    fn dummy_task_result(task_id: &str) -> TaskResult {
        TaskResult {
            task_id: task_id.to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: None,
            operation: None,
            worker_id: None,
        }
    }

    #[tokio::test]
    async fn async_wait_delivers_a_result() {
        let batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let (tx, rx) = flume::unbounded::<TaskResult>();
        batches.insert("b1".to_string(), test_operation_batch(rx, 1));
        tx.send(dummy_task_result("op-1")).unwrap();

        let got = next_operation_result_async("p", &batches, None, "b1", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(got.unwrap().task_id, "op-1");
    }

    #[tokio::test]
    async fn async_wait_drains_buffered_result_when_giving_up() {
        // Lost-result race regression: the waiter-forwarder does
        // `batch_tx.send(result)` BEFORE `pending_count.fetch_sub(1)`, so the
        // instant the wait observes `pending_count == 0` the delivered result is
        // ALREADY buffered in the channel. A give-up branch that returns
        // `Ok(None)` without draining drops that completed result, which
        // `wait_for_operation_results` then mis-maps to a "timed out" SystemError
        // under load. `pending == 0` + a buffered result + `Duration::ZERO`
        // (ceiling 0 via the test infra floor) forces the give-up branch on the
        // first poll, exactly the runtime race window.
        let batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let (tx, rx) = flume::unbounded::<TaskResult>();
        batches.insert("b1".to_string(), test_operation_batch(rx, 0));
        tx.send(dummy_task_result("op-1")).unwrap();

        let got = next_operation_result_async("p", &batches, None, "b1", Duration::ZERO)
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
        // Sync sibling of the lost-result race (the Extism host-fn boundary).
        let batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let (tx, rx) = flume::unbounded::<TaskResult>();
        batches.insert("b1".to_string(), test_operation_batch(rx, 0));
        tx.send(dummy_task_result("op-1")).unwrap();

        let got = next_operation_result("p", &batches, None, "b1", Duration::ZERO).unwrap();
        assert_eq!(
            got.expect("a delivered result must never be dropped by a give-up branch")
                .task_id,
            "op-1"
        );
    }

    #[tokio::test]
    async fn async_wait_ends_promptly_on_dropped_sender() {
        // The leak fix: an orphaned op (sender dropped, e.g. its batch was
        // cancelled/superseded) must terminate the async wait at once, NOT linger
        // to the infra ceiling. pending stays > 0 so only the disconnect can end it.
        let batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let (tx, rx) = flume::unbounded::<TaskResult>();
        batches.insert("b2".to_string(), test_operation_batch(rx, 1));
        drop(tx);

        let outcome = tokio::time::timeout(
            Duration::from_millis(500),
            next_operation_result_async("p", &batches, None, "b2", Duration::from_secs(30)),
        )
        .await;
        assert!(
            outcome.is_ok(),
            "wait must end promptly on a dropped sender, not linger to the ceiling"
        );
        assert!(
            outcome.unwrap().is_err(),
            "dropped sender surfaces as an error"
        );
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
            target_worker_id: None,
            evaluate_batch_id: None,
            test_case_id: None,
        }
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

    #[tokio::test]
    async fn start_operation_batch_fails_when_mq_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            FilesystemBlobStore::new(temp.path().to_path_buf(), 2_000_000)
                .await
                .unwrap(),
        );
        let deps = OperationHostDeps {
            plugin_manager: None,
            operation_publisher: None,
            operation_batches: Arc::new(dashmap::DashMap::new()),
            operation_waiters: Arc::new(dashmap::DashMap::new()),
            server_id: "server-1".to_string(),
            blob_store,
            metrics: None,
            evaluate_ops_registry:
                crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry::default(),
            operation_batch_publish_concurrency: 32,
        };

        let err = start_operation_batch("plugin".to_string(), deps, vec![])
            .await
            .unwrap_err();
        assert!(err.to_string().contains("MQ not available"));
    }

    #[tokio::test]
    async fn failed_operation_batch_cleanup_removes_batch_waiters_and_eval_refs() {
        let temp = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(
            FilesystemBlobStore::new(temp.path().to_path_buf(), 2_000_000)
                .await
                .unwrap(),
        );
        let operation_batches = Arc::new(dashmap::DashMap::new());
        let operation_waiters = Arc::new(dashmap::DashMap::new());
        let evaluate_ops_registry =
            crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry::default();
        let cleanup_keys = vec!["op-1".to_string(), "op-2".to_string()];
        let (_batch_tx, batch_rx) = flume::unbounded();
        let (waiter_tx, _waiter_rx) = tokio::sync::oneshot::channel();

        operation_batches.insert(
            "batch-1".to_string(),
            BatchState {
                result_rx: batch_rx,
                pending_count: Arc::new(AtomicUsize::new(2)),
                created_at: Instant::now(),
                cleanup_keys: Arc::new(cleanup_keys.clone()),
                poisoned: AtomicBool::new(false),
            },
        );
        operation_waiters.insert("op-1".to_string(), OperationWaiter::new(waiter_tx));
        evaluate_ops_registry.record_ops("eval-1", 10, "batch-1", cleanup_keys.clone());

        let deps = OperationHostDeps {
            plugin_manager: None,
            operation_publisher: None,
            operation_batches: operation_batches.clone(),
            operation_waiters: operation_waiters.clone(),
            server_id: "server-1".to_string(),
            blob_store,
            metrics: None,
            evaluate_ops_registry: evaluate_ops_registry.clone(),
            operation_batch_publish_concurrency: 32,
        };

        cleanup_failed_operation_batch(
            &deps,
            "batch-1",
            &cleanup_keys,
            &[("eval-1".to_string(), 10)],
            "plugin",
        );

        assert!(operation_batches.get("batch-1").is_none());
        assert!(operation_waiters.get("op-1").is_none());
        assert!(
            evaluate_ops_registry
                .operation_task_ids_for_test_cases("eval-1", &[10])
                .is_empty()
        );
    }

    #[test]
    fn cancel_operation_batch_removes_waiters() {
        let batches = Arc::new(dashmap::DashMap::new());
        let waiters = Arc::new(dashmap::DashMap::new());
        let (batch_tx, batch_rx) = flume::unbounded();
        drop(batch_tx);
        let (waiter_tx, _waiter_rx) = tokio::sync::oneshot::channel();
        let cleanup_keys = Arc::new(vec!["task-1".to_string()]);
        batches.insert(
            "batch-1".to_string(),
            BatchState {
                result_rx: batch_rx,
                pending_count: Arc::new(AtomicUsize::new(1)),
                created_at: Instant::now(),
                cleanup_keys: cleanup_keys.clone(),
                poisoned: AtomicBool::new(false),
            },
        );
        waiters.insert("task-1".to_string(), OperationWaiter::new(waiter_tx));

        cancel_operation_batch("plugin", &batches, &waiters, None, "batch-1");

        assert!(batches.get("batch-1").is_none());
        assert!(waiters.get("task-1").is_none());
    }
}
