//! The result-wait loop shared by operation and evaluate batches.
//!
//! A batch's results arrive on its channel while its pending count tracks
//! what is still in flight. Waiting for the next result is the same loop for
//! both batch kinds: receive one tick at a time, keep waiting while the batch is
//! still pending and its policy allows, drain a result buffered in the
//! send-before-decrement race before giving up, and record the outcome metrics.
//! The kinds differ only in policy (how long a wait may extend, and what a
//! delivered result updates), which [`WaitPolicy`] captures.
//!
//! The loop has two drivers that differ only in their receive call:
//! [`BatchWait::blocking`] for the Extism host-fn boundary, which runs in a sync
//! plugin context and cannot await, and [`BatchWait::wait`] for the detached
//! windowed drivers, where a waiting slot is a cheap future rather than a
//! `spawn_blocking` thread and a dropped sender (a cancelled or superseded
//! batch) ends the wait at once.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::anyhow;
use opentelemetry::KeyValue;

use crate::services::batch_table::{BatchResult, BatchTable};

/// What one batch kind decides about its result wait.
pub(crate) trait WaitPolicy<T, S> {
    /// The error a wait returns once every sender is gone.
    const DISCONNECTED: &'static str;

    /// Receive budget for the next tick, or `None` once the wait has reached
    /// its ceiling and gives up.
    fn next_tick(&self, waited: Duration) -> Option<Duration>;

    /// Whether an empty tick on a still-pending batch keeps waiting.
    fn extend(&self, wait: &WaitCtx<'_, T, S>) -> bool;

    /// Kind-specific bookkeeping for a delivered result (completion marks,
    /// traces, the `result` outcome metric, the receive log). Runs before the
    /// drained batch is removed.
    fn on_delivered(&self, wait: &WaitCtx<'_, T, S>, result: &T);
}

/// The batch-kind-independent state of one result wait.
pub(crate) struct WaitCtx<'a, T, S> {
    pub plugin_id: &'a str,
    pub batch_id: &'a str,
    pub metrics: Option<&'a common::metrics::Metrics>,
    pub batches: &'a BatchTable<T, S>,
    result_rx: flume::Receiver<T>,
    pending_count: Arc<AtomicUsize>,
    started: Instant,
}

impl<T: BatchResult, S> WaitCtx<'_, T, S> {
    /// Record one wait outcome on the batch wait metrics; `extra` is appended
    /// after the shared `batch.kind`/`plugin.id`/`outcome` attributes.
    pub fn record_outcome(&self, outcome: &'static str, extra: Option<KeyValue>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let [kind, plugin, outcome] = [
            KeyValue::new("batch.kind", T::KIND),
            KeyValue::new("plugin.id", self.plugin_id.to_string()),
            KeyValue::new("outcome", outcome),
        ];
        let waited = self.started.elapsed().as_secs_f64();
        let record = |attrs: &[KeyValue]| {
            metrics.batch_wait_duration.record(waited, attrs);
            metrics.batch_results_total.add(1, attrs);
        };
        match extra {
            Some(extra) => record(&[kind, plugin, outcome, extra]),
            None => record(&[kind, plugin, outcome]),
        }
    }
}

/// One wait for a batch's next result.
pub(crate) struct BatchWait<'a, T, S, P> {
    ctx: WaitCtx<'a, T, S>,
    policy: P,
}

/// What one tick's receive produced.
enum Received<T> {
    Result(T),
    Empty,
    Disconnected,
}

impl<'a, T: BatchResult, S, P: WaitPolicy<T, S>> BatchWait<'a, T, S, P> {
    /// Look the batch up and start the wait clock.
    pub fn open(
        plugin_id: &'a str,
        batches: &'a BatchTable<T, S>,
        batch_id: &'a str,
        policy: P,
    ) -> anyhow::Result<Self> {
        let (result_rx, pending_count) = batches
            .receiver(batch_id)
            .ok_or_else(|| anyhow!("Batch not found: {}", batch_id))?;
        Ok(Self {
            ctx: WaitCtx {
                plugin_id,
                batch_id,
                metrics: batches.metrics(),
                batches,
                result_rx,
                pending_count,
                started: Instant::now(),
            },
            policy,
        })
    }

    /// Block the calling thread until the next result, a give-up, or a
    /// disconnect.
    pub fn blocking(self) -> anyhow::Result<Option<T>> {
        loop {
            let Some(tick) = self.policy.next_tick(self.ctx.started.elapsed()) else {
                return Ok(self.give_up());
            };
            let received = match self.ctx.result_rx.recv_timeout(tick) {
                Ok(result) => Received::Result(result),
                Err(flume::RecvTimeoutError::Timeout) => Received::Empty,
                Err(flume::RecvTimeoutError::Disconnected) => Received::Disconnected,
            };
            if let Some(outcome) = self.step(received) {
                return outcome;
            }
        }
    }

    /// Await the next result, a give-up, or a disconnect.
    pub async fn wait(self) -> anyhow::Result<Option<T>> {
        loop {
            let Some(tick) = self.policy.next_tick(self.ctx.started.elapsed()) else {
                return Ok(self.give_up());
            };
            let received = match tokio::time::timeout(tick, self.ctx.result_rx.recv_async()).await {
                Ok(Ok(result)) => Received::Result(result),
                Ok(Err(_disconnected)) => Received::Disconnected,
                Err(_elapsed) => Received::Empty,
            };
            if let Some(outcome) = self.step(received) {
                return outcome;
            }
        }
    }

    /// Handle one tick's receive; `None` keeps waiting.
    fn step(&self, received: Received<T>) -> Option<anyhow::Result<Option<T>>> {
        match received {
            Received::Result(result) => Some(Ok(Some(self.deliver(result)))),
            Received::Empty
                if self.ctx.pending_count.load(Ordering::SeqCst) > 0
                    && self.policy.extend(&self.ctx) =>
            {
                None
            }
            Received::Empty => Some(Ok(self.give_up())),
            Received::Disconnected => {
                self.ctx.record_outcome("disconnected", None);
                Some(Err(anyhow!(P::DISCONNECTED)))
            }
        }
    }

    /// Give up the wait, unless a result is already buffered. The batch's
    /// producer sends each result BEFORE decrementing the pending count, so the
    /// instant a give-up observes the batch as no longer pending (or the
    /// ceiling elapses) a completed result may ALREADY be in the channel.
    /// Returning `None` without this drain drops it, which the caller maps to
    /// a timeout and a spurious SystemError (or a wasted retry) under load.
    pub(crate) fn give_up(&self) -> Option<T> {
        if let Ok(result) = self.ctx.result_rx.try_recv() {
            return Some(self.deliver(result));
        }
        self.ctx.record_outcome("timeout", None);
        None
    }

    /// Run the delivery bookkeeping, and remove the batch once this result
    /// drained it.
    fn deliver(&self, result: T) -> T {
        let ctx = &self.ctx;
        self.policy.on_delivered(ctx, &result);
        ctx.batches.remove_if_drained(ctx.batch_id);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::batch_table::BatchSender;
    use common::worker::TaskResult;

    type Table = BatchTable<TaskResult, ()>;

    /// A minimal policy: fixed 10ms ticks, never past `ceiling`, extending
    /// while pending.
    struct TestWait {
        ceiling: Duration,
    }

    impl WaitPolicy<TaskResult, ()> for TestWait {
        const DISCONNECTED: &'static str = "test channel disconnected";

        fn next_tick(&self, waited: Duration) -> Option<Duration> {
            (waited < self.ceiling).then_some(Duration::from_millis(10))
        }

        fn extend(&self, _wait: &WaitCtx<'_, TaskResult, ()>) -> bool {
            true
        }

        fn on_delivered(&self, _wait: &WaitCtx<'_, TaskResult, ()>, _result: &TaskResult) {}
    }

    fn result(n: u32) -> TaskResult {
        TaskResult {
            task_id: n.to_string(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: None,
            operation: None,
            worker_id: None,
            enqueued_at_unix_ms: None,
        }
    }

    fn batch(keys: &[u32]) -> (Table, BatchSender<TaskResult>) {
        let batches = Table::default();
        let tx = batches.open("b", keys.iter().map(u32::to_string), ());
        (batches, tx)
    }

    fn open(batches: &Table, ceiling: Duration) -> BatchWait<'_, TaskResult, (), TestWait> {
        BatchWait::open("p", batches, "b", TestWait { ceiling }).unwrap()
    }

    fn id(result: Option<TaskResult>) -> Option<String> {
        result.map(|r| r.task_id)
    }

    #[test]
    fn unknown_batch_is_an_error() {
        let batches = Table::default();
        let err = BatchWait::open(
            "p",
            &batches,
            "nope",
            TestWait {
                ceiling: Duration::ZERO,
            },
        )
        .err()
        .expect("missing batch");
        assert_eq!(err.to_string(), "Batch not found: nope");
    }

    #[test]
    fn last_delivered_result_removes_the_drained_batch() {
        let (batches, tx) = batch(&[1, 2]);
        tx.deliver(result(1));
        tx.deliver(result(2));

        let wait = open(&batches, Duration::from_secs(1));
        assert_eq!(id(wait.blocking().unwrap()).as_deref(), Some("1"));
        assert!(batches.contains("b"), "a buffered result remains");

        let wait = open(&batches, Duration::from_secs(1));
        assert_eq!(id(wait.blocking().unwrap()).as_deref(), Some("2"));
        assert!(!batches.contains("b"), "the drained batch is removed");
    }

    #[test]
    fn disconnected_channel_is_the_policy_error() {
        let (batches, tx) = batch(&[1]);
        drop(tx);
        let err = open(&batches, Duration::from_secs(1))
            .blocking()
            .unwrap_err();
        assert_eq!(err.to_string(), "test channel disconnected");
    }

    #[tokio::test]
    async fn pending_batch_waits_past_empty_ticks_until_the_result_lands() {
        let (batches, tx) = batch(&[7]);
        let wait = tokio::spawn({
            let batches = batches.clone();
            async move { id(open(&batches, Duration::from_secs(5)).wait().await.unwrap()) }
        });
        // Let the wait run through empty ticks before the result exists.
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
        tx.deliver(result(7));
        assert_eq!(wait.await.unwrap().as_deref(), Some("7"));
    }

    #[test]
    fn give_up_drains_a_result_buffered_before_the_pending_decrement() {
        let (batches, tx) = batch(&[3]);
        tx.deliver(result(3));
        let wait = open(&batches, Duration::ZERO);
        assert_eq!(id(wait.give_up()).as_deref(), Some("3"));
    }

    #[tokio::test]
    async fn settled_batch_gives_up_after_one_empty_tick() {
        let (batches, _tx) = batch(&[]);
        let got = open(&batches, Duration::from_secs(5)).wait().await.unwrap();
        assert!(got.is_none());
    }
}
