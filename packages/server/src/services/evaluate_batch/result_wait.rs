use super::*;

pub fn next_evaluate_result(
    plugin_id: &str,
    batches: &EvaluateBatches,
    metrics: Option<&common::metrics::Metrics>,
    evaluate_ops_registry: &crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TestCaseVerdict>> {
    let (result_rx, pending_count) = {
        let batch = batches
            .get(batch_id)
            .ok_or_else(|| anyhow!("Batch not found: {}", batch_id))?;
        (batch.result_rx.clone(), batch.pending_count.clone())
    };
    let ctx = EvaluateVerdictCtx {
        plugin_id,
        batches,
        metrics,
        evaluate_ops_registry,
        batch_id,
        result_rx: &result_rx,
        pending_count: &pending_count,
    };

    let wait_start = Instant::now();
    if timeout.is_zero() {
        return ctx.handle_receive(
            wait_start,
            result_rx.try_recv().map_err(|err| match err {
                flume::TryRecvError::Empty => flume::RecvTimeoutError::Timeout,
                flume::TryRecvError::Disconnected => flume::RecvTimeoutError::Disconnected,
            }),
        );
    }

    loop {
        match result_rx.recv_timeout(next_evaluate_wait_tick(timeout)) {
            Ok(verdict) => {
                return ctx.handle_verdict(wait_start, verdict);
            }
            Err(flume::RecvTimeoutError::Timeout) => {
                if pending_count.load(Ordering::SeqCst) > 0
                    && evaluate_ops_registry
                        .should_extend_wait_for_execution_timeout(batch_id, timeout)
                {
                    tracing::debug!(
                        plugin_id = %plugin_id,
                        batch_id = %batch_id,
                        timeout_ms = timeout.as_millis(),
                        "Evaluate result wait extended while operation execution budget remains"
                    );
                    continue;
                }

                // The dispatch task sends the verdict BEFORE decrementing
                // pending_count, so this give-up branch can observe the timeout
                // with a completed verdict already buffered. Deliver it instead
                // of dropping it into a spurious timeout.
                if let Some(delivered) = ctx.drain_before_giveup(wait_start) {
                    return delivered;
                }

                if let Some(metrics) = metrics {
                    let attrs = [
                        KeyValue::new("batch.kind", "evaluate"),
                        KeyValue::new("plugin.id", plugin_id.to_string()),
                        KeyValue::new("outcome", "timeout"),
                    ];
                    metrics
                        .batch_wait_duration
                        .record(wait_start.elapsed().as_secs_f64(), &attrs);
                    metrics.batch_results_total.add(1, &attrs);
                }
                return Ok(None);
            }
            Err(flume::RecvTimeoutError::Disconnected) => {
                if let Some(metrics) = metrics {
                    let attrs = [
                        KeyValue::new("batch.kind", "evaluate"),
                        KeyValue::new("plugin.id", plugin_id.to_string()),
                        KeyValue::new("outcome", "disconnected"),
                    ];
                    metrics
                        .batch_wait_duration
                        .record(wait_start.elapsed().as_secs_f64(), &attrs);
                    metrics.batch_results_total.add(1, &attrs);
                }
                return Err(anyhow!("Evaluate batch channel disconnected"));
            }
        }
    }
}

/// Async sibling of [`next_evaluate_result`]. Awaits each result via flume's
/// `recv_async` so a detached windowed-evaluate slot is a cheap future, not a
/// `spawn_blocking` OS thread. A dropped sender (cancelled/superseded batch)
/// returns `Disconnected` immediately, so superseded slots don't linger to the
/// 30-min ceiling. The sync version is kept for the Extism host-fn boundary.
pub async fn next_evaluate_result_async(
    plugin_id: &str,
    batches: &EvaluateBatches,
    metrics: Option<&common::metrics::Metrics>,
    evaluate_ops_registry: &crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry,
    batch_id: &str,
    timeout: Duration,
) -> anyhow::Result<Option<TestCaseVerdict>> {
    let (result_rx, pending_count) = {
        let batch = batches
            .get(batch_id)
            .ok_or_else(|| anyhow!("Batch not found: {}", batch_id))?;
        (batch.result_rx.clone(), batch.pending_count.clone())
    };
    let ctx = EvaluateVerdictCtx {
        plugin_id,
        batches,
        metrics,
        evaluate_ops_registry,
        batch_id,
        result_rx: &result_rx,
        pending_count: &pending_count,
    };

    let wait_start = Instant::now();
    if timeout.is_zero() {
        return ctx.handle_receive(
            wait_start,
            result_rx.try_recv().map_err(|err| match err {
                flume::TryRecvError::Empty => flume::RecvTimeoutError::Timeout,
                flume::TryRecvError::Disconnected => flume::RecvTimeoutError::Disconnected,
            }),
        );
    }

    loop {
        match tokio::time::timeout(next_evaluate_wait_tick(timeout), result_rx.recv_async()).await {
            Ok(Ok(verdict)) => {
                return ctx.handle_verdict(wait_start, verdict);
            }
            Ok(Err(_recv_error)) => {
                record_evaluate_wait_metric(plugin_id, metrics, wait_start, "disconnected");
                return Err(anyhow!("Evaluate batch channel disconnected"));
            }
            Err(_elapsed) => {
                if pending_count.load(Ordering::SeqCst) > 0
                    && evaluate_ops_registry
                        .should_extend_wait_for_execution_timeout(batch_id, timeout)
                {
                    tracing::debug!(
                        plugin_id = %plugin_id,
                        batch_id = %batch_id,
                        timeout_ms = timeout.as_millis(),
                        "Evaluate result wait extended while operation execution budget remains"
                    );
                    continue;
                }
                // Same race as the sync path: a verdict may have landed in the
                // channel just as this branch decided to give up. Deliver it.
                if let Some(delivered) = ctx.drain_before_giveup(wait_start) {
                    return delivered;
                }
                record_evaluate_wait_metric(plugin_id, metrics, wait_start, "timeout");
                return Ok(None);
            }
        }
    }
}

fn record_evaluate_wait_metric(
    plugin_id: &str,
    metrics: Option<&common::metrics::Metrics>,
    wait_start: Instant,
    outcome: &'static str,
) {
    if let Some(metrics) = metrics {
        let attrs = [
            KeyValue::new("batch.kind", "evaluate"),
            KeyValue::new("plugin.id", plugin_id.to_string()),
            KeyValue::new("outcome", outcome),
        ];
        metrics
            .batch_wait_duration
            .record(wait_start.elapsed().as_secs_f64(), &attrs);
        metrics.batch_results_total.add(1, &attrs);
    }
}

fn next_evaluate_wait_tick(timeout: Duration) -> Duration {
    timeout.min(EVALUATE_RESULT_WAIT_TICK)
}

impl EvaluateVerdictCtx<'_> {
    fn handle_receive(
        &self,
        wait_start: Instant,
        result: Result<TestCaseVerdict, flume::RecvTimeoutError>,
    ) -> anyhow::Result<Option<TestCaseVerdict>> {
        let plugin_id = self.plugin_id;
        let metrics = self.metrics;
        match result {
            Ok(verdict) => self.handle_verdict(wait_start, verdict),
            Err(flume::RecvTimeoutError::Timeout) => {
                if let Some(metrics) = metrics {
                    let attrs = [
                        KeyValue::new("batch.kind", "evaluate"),
                        KeyValue::new("plugin.id", plugin_id.to_string()),
                        KeyValue::new("outcome", "timeout"),
                    ];
                    metrics
                        .batch_wait_duration
                        .record(wait_start.elapsed().as_secs_f64(), &attrs);
                    metrics.batch_results_total.add(1, &attrs);
                }
                Ok(None)
            }
            Err(flume::RecvTimeoutError::Disconnected) => {
                if let Some(metrics) = metrics {
                    let attrs = [
                        KeyValue::new("batch.kind", "evaluate"),
                        KeyValue::new("plugin.id", plugin_id.to_string()),
                        KeyValue::new("outcome", "disconnected"),
                    ];
                    metrics
                        .batch_wait_duration
                        .record(wait_start.elapsed().as_secs_f64(), &attrs);
                    metrics.batch_results_total.add(1, &attrs);
                }
                Err(anyhow!("Evaluate batch channel disconnected"))
            }
        }
    }
}

pub fn cancel_evaluate_batch(
    plugin_id: &str,
    batches: &EvaluateBatches,
    metrics: Option<&common::metrics::Metrics>,
    evaluate_ops_registry: &crate::host_funcs::evaluate_ops_registry::EvaluateBatchOpsRegistry,
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
