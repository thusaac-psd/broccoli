use super::*;

use crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry;

/// The evaluate batch result-wait policy: a fixed tick, and an empty tick keeps
/// waiting only while the ops registry says a test case's operation execution
/// budget remains.
pub(super) struct EvaluateWait<'a> {
    evaluate_ops_registry: &'a EvaluateBatchOpsRegistry,
    timeout: Duration,
}

impl WaitPolicy<TestCaseVerdict> for EvaluateWait<'_> {
    const KIND: &'static str = "evaluate";
    const DISCONNECTED: &'static str = "Evaluate batch channel disconnected";

    fn next_tick(&self, _waited: Duration) -> Option<Duration> {
        Some(self.timeout.min(EVALUATE_RESULT_WAIT_TICK))
    }

    // A zero timeout is a single non-blocking poll, never extended.
    fn extend(&self, wait: &WaitCtx<'_, TestCaseVerdict>) -> bool {
        let extend = !self.timeout.is_zero()
            && self
                .evaluate_ops_registry
                .should_extend_wait_for_execution_timeout(wait.batch_id, self.timeout);
        if extend {
            tracing::debug!(
                plugin_id = %wait.plugin_id,
                batch_id = %wait.batch_id,
                timeout_ms = self.timeout.as_millis(),
                "Evaluate result wait extended while operation execution budget remains"
            );
        }
        extend
    }

    fn on_delivered(&self, wait: &WaitCtx<'_, TestCaseVerdict>, verdict: &TestCaseVerdict) {
        self.evaluate_ops_registry
            .mark_test_case_completed(wait.batch_id, verdict.test_case_id);
        wait.record_outcome(
            "result",
            Some(KeyValue::new("verdict", verdict.verdict.to_string())),
        );
        tracing::debug!(
            plugin_id = %wait.plugin_id,
            batch_id = %wait.batch_id,
            test_case_id = verdict.test_case_id,
            verdict = %verdict.verdict,
            "Evaluate result received"
        );
    }

    fn on_drained(&self, batch_id: &str) {
        self.evaluate_ops_registry.remove_batch(batch_id);
    }
}

pub(super) fn evaluate_wait<'a>(
    plugin_id: &'a str,
    batches: &'a EvaluateBatches,
    metrics: Option<&'a common::metrics::Metrics>,
    evaluate_ops_registry: &'a EvaluateBatchOpsRegistry,
    batch_id: &'a str,
    timeout: Duration,
) -> anyhow::Result<BatchWait<'a, TestCaseVerdict, EvaluateWait<'a>>> {
    BatchWait::open(
        plugin_id,
        batches,
        metrics,
        batch_id,
        EvaluateWait {
            evaluate_ops_registry,
            timeout,
        },
    )
}

/// Wait for the batch's next verdict, blocking the calling thread. Kept for the
/// Extism host-fn boundary, which cannot await.
pub fn next_evaluate_result(
    plugin_id: &str,
    batches: &EvaluateBatches,
    metrics: Option<&common::metrics::Metrics>,
    evaluate_ops_registry: &EvaluateBatchOpsRegistry,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TestCaseVerdict>> {
    evaluate_wait(
        plugin_id,
        batches,
        metrics,
        evaluate_ops_registry,
        batch_id,
        timeout,
    )?
    .blocking()
}

/// Async sibling of [`next_evaluate_result`]. Awaits each result via flume's
/// `recv_async` so a detached windowed-evaluate slot is a cheap future, not a
/// `spawn_blocking` OS thread. A dropped sender (cancelled/superseded batch)
/// returns `Disconnected` immediately, so superseded slots don't linger to the
/// 30-min ceiling.
pub async fn next_evaluate_result_async(
    plugin_id: &str,
    batches: &EvaluateBatches,
    metrics: Option<&common::metrics::Metrics>,
    evaluate_ops_registry: &EvaluateBatchOpsRegistry,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TestCaseVerdict>> {
    evaluate_wait(
        plugin_id,
        batches,
        metrics,
        evaluate_ops_registry,
        batch_id,
        timeout,
    )?
    .wait()
    .await
}

pub fn cancel_evaluate_batch(
    plugin_id: &str,
    batches: &EvaluateBatches,
    metrics: Option<&common::metrics::Metrics>,
    evaluate_ops_registry: &EvaluateBatchOpsRegistry,
    batch_id: &str,
) {
    if batches.remove(batch_id).is_some() {
        evaluate_ops_registry.remove_batch(batch_id);
        if let Some(metrics) = metrics {
            let attrs = [
                KeyValue::new("batch.kind", "evaluate"),
                KeyValue::new("plugin.id", plugin_id.to_string()),
            ];
            metrics.batch_cancelled_total.add(1, &attrs);
            metrics
                .batch_active
                .add(-1, &[KeyValue::new("batch.kind", "evaluate")]);
        }
    }

    tracing::info!(
        plugin_id = %plugin_id,
        batch_id = %batch_id,
        "Evaluate batch cancelled"
    );
}
