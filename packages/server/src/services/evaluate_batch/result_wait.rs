use super::*;

/// The evaluate batch result-wait policy: a fixed tick, and an empty tick keeps
/// waiting only while one of the batch's test cases may still produce a
/// verdict (see [`EvaluateCases`]).
pub(super) struct EvaluateWait {
    timeout: Duration,
}

impl WaitPolicy<TestCaseVerdict, EvaluateCases> for EvaluateWait {
    const DISCONNECTED: &'static str = "Evaluate batch channel disconnected";

    fn next_tick(&self, _waited: Duration) -> Option<Duration> {
        Some(self.timeout.min(EVALUATE_RESULT_WAIT_TICK))
    }

    // A zero timeout is a single non-blocking poll, never extended.
    fn extend(&self, wait: &WaitCtx<'_, TestCaseVerdict, EvaluateCases>) -> bool {
        let extend = !self.timeout.is_zero()
            && wait
                .batches
                .should_extend_wait(wait.batch_id, self.timeout, Instant::now());
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

    fn on_delivered(
        &self,
        wait: &WaitCtx<'_, TestCaseVerdict, EvaluateCases>,
        verdict: &TestCaseVerdict,
    ) {
        wait.batches
            .mark_case_completed(wait.batch_id, verdict.test_case_id);
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
}

pub(super) fn evaluate_wait<'a>(
    plugin_id: &'a str,
    batches: &'a EvaluateBatches,
    batch_id: &'a str,
    timeout: Duration,
) -> anyhow::Result<BatchWait<'a, TestCaseVerdict, EvaluateCases, EvaluateWait>> {
    BatchWait::open(plugin_id, batches, batch_id, EvaluateWait { timeout })
}

/// Wait for the batch's next verdict, blocking the calling thread. Kept for the
/// Extism host-fn boundary, which cannot await.
pub fn next_evaluate_result(
    plugin_id: &str,
    batches: &EvaluateBatches,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TestCaseVerdict>> {
    evaluate_wait(plugin_id, batches, batch_id, timeout)?.blocking()
}

/// Async sibling of [`next_evaluate_result`]. Awaits each result via flume's
/// `recv_async` so a detached windowed-evaluate slot is a cheap future, not a
/// `spawn_blocking` OS thread. A dropped sender (cancelled/superseded batch)
/// returns `Disconnected` immediately, so superseded slots don't linger to the
/// 30-min ceiling.
pub async fn next_evaluate_result_async(
    plugin_id: &str,
    batches: &EvaluateBatches,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TestCaseVerdict>> {
    evaluate_wait(plugin_id, batches, batch_id, timeout)?
        .wait()
        .await
}

pub fn cancel_evaluate_batch(plugin_id: &str, batches: &EvaluateBatches, batch_id: &str) {
    batches.remove(batch_id, Removal::Cancelled { plugin_id });
    tracing::info!(
        plugin_id = %plugin_id,
        batch_id = %batch_id,
        "Evaluate batch cancelled"
    );
}
