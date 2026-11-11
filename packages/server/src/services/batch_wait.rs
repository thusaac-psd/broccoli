//! The result-wait loop shared by operation and evaluate batches.
//!
//! A batch's results arrive on its flume channel while `pending_count` tracks
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
use dashmap::DashMap;
use opentelemetry::KeyValue;

use crate::registry::BatchState;

/// What one batch kind decides about its result wait.
pub(crate) trait WaitPolicy<T> {
    /// The `batch.kind` metric label.
    const KIND: &'static str;
    /// The error a wait returns once every sender is gone.
    const DISCONNECTED: &'static str;

    /// Receive budget for the next tick, or `None` once the wait has reached
    /// its ceiling and gives up.
    fn next_tick(&self, waited: Duration) -> Option<Duration>;

    /// Whether an empty tick on a still-pending batch keeps waiting.
    fn extend(&self, wait: &WaitCtx<'_, T>) -> bool;

    /// Kind-specific bookkeeping for a delivered result (completion marks,
    /// traces, the `result` outcome metric, the receive log). Runs before the
    /// shared drained-batch cleanup.
    fn on_delivered(&self, wait: &WaitCtx<'_, T>, result: &T);

    /// Extra cleanup once a delivered result drained and removed the batch.
    fn on_drained(&self, _batch_id: &str) {}
}

/// The batch-kind-independent state of one result wait.
pub(crate) struct WaitCtx<'a, T> {
    pub plugin_id: &'a str,
    pub batch_id: &'a str,
    pub metrics: Option<&'a common::metrics::Metrics>,
    kind: &'static str,
    batches: &'a DashMap<String, BatchState<T>>,
    result_rx: flume::Receiver<T>,
    pending_count: Arc<AtomicUsize>,
    started: Instant,
}

impl<T> WaitCtx<'_, T> {
    /// Record one wait outcome on the batch wait metrics; `extra` is appended
    /// after the shared `batch.kind`/`plugin.id`/`outcome` attributes.
    pub fn record_outcome(&self, outcome: &'static str, extra: Option<KeyValue>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        let [kind, plugin, outcome] = [
            KeyValue::new("batch.kind", self.kind),
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
pub(crate) struct BatchWait<'a, T, P> {
    ctx: WaitCtx<'a, T>,
    policy: P,
}

/// What one tick's receive produced.
enum Received<T> {
    Result(T),
    Empty,
    Disconnected,
}

impl<'a, T, P: WaitPolicy<T>> BatchWait<'a, T, P> {
    /// Look the batch up and start the wait clock.
    pub fn open(
        plugin_id: &'a str,
        batches: &'a DashMap<String, BatchState<T>>,
        metrics: Option<&'a common::metrics::Metrics>,
        batch_id: &'a str,
        policy: P,
    ) -> anyhow::Result<Self> {
        let (result_rx, pending_count) = {
            let batch = batches
                .get(batch_id)
                .ok_or_else(|| anyhow!("Batch not found: {}", batch_id))?;
            (batch.result_rx.clone(), batch.pending_count.clone())
        };
        Ok(Self {
            ctx: WaitCtx {
                plugin_id,
                batch_id,
                metrics,
                kind: P::KIND,
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
    /// producer sends each result BEFORE decrementing `pending_count`, so the
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
        if ctx.pending_count.load(Ordering::SeqCst) == 0
            && ctx.result_rx.is_empty()
            && ctx.batches.remove(ctx.batch_id).is_some()
        {
            self.policy.on_drained(ctx.batch_id);
            if let Some(metrics) = ctx.metrics {
                metrics
                    .batch_active
                    .add(-1, &[KeyValue::new("batch.kind", ctx.kind)]);
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A minimal policy: fixed 10ms ticks, never past `ceiling`, extending
    /// while pending.
    struct TestWait {
        ceiling: Duration,
    }

    impl WaitPolicy<u32> for TestWait {
        const KIND: &'static str = "test";
        const DISCONNECTED: &'static str = "test channel disconnected";

        fn next_tick(&self, waited: Duration) -> Option<Duration> {
            (waited < self.ceiling).then_some(Duration::from_millis(10))
        }

        fn extend(&self, _wait: &WaitCtx<'_, u32>) -> bool {
            true
        }

        fn on_delivered(&self, _wait: &WaitCtx<'_, u32>, _result: &u32) {}
    }

    fn batch(
        pending: usize,
    ) -> (
        DashMap<String, BatchState<u32>>,
        flume::Sender<u32>,
        Arc<AtomicUsize>,
    ) {
        let (tx, rx) = flume::unbounded();
        let pending_count = Arc::new(AtomicUsize::new(pending));
        let batches = DashMap::new();
        batches.insert(
            "b".to_string(),
            BatchState {
                result_rx: rx,
                pending_count: pending_count.clone(),
                created_at: Instant::now(),
                cleanup_keys: Arc::new(Vec::new()),
                poisoned: AtomicBool::new(false),
            },
        );
        (batches, tx, pending_count)
    }

    fn open<'a>(
        batches: &'a DashMap<String, BatchState<u32>>,
        ceiling: Duration,
    ) -> BatchWait<'a, u32, TestWait> {
        BatchWait::open("p", batches, None, "b", TestWait { ceiling }).unwrap()
    }

    #[test]
    fn unknown_batch_is_an_error() {
        let batches = DashMap::<String, BatchState<u32>>::new();
        let err = BatchWait::open(
            "p",
            &batches,
            None,
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
        let (batches, tx, _pending) = batch(0);
        tx.send(1).unwrap();
        tx.send(2).unwrap();

        let wait = open(&batches, Duration::from_secs(1));
        assert_eq!(wait.blocking().unwrap(), Some(1));
        assert!(batches.contains_key("b"), "a buffered result remains");

        let wait = open(&batches, Duration::from_secs(1));
        assert_eq!(wait.blocking().unwrap(), Some(2));
        assert!(!batches.contains_key("b"), "the drained batch is removed");
    }

    #[test]
    fn disconnected_channel_is_the_policy_error() {
        let (batches, tx, _pending) = batch(1);
        drop(tx);
        let err = open(&batches, Duration::from_secs(1))
            .blocking()
            .unwrap_err();
        assert_eq!(err.to_string(), "test channel disconnected");
    }

    #[tokio::test]
    async fn pending_batch_waits_past_empty_ticks_until_the_result_lands() {
        let (batches, tx, pending) = batch(1);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tx.send(7).unwrap();
            pending.store(0, Ordering::SeqCst);
        });
        let got = open(&batches, Duration::from_secs(5)).wait().await.unwrap();
        assert_eq!(got, Some(7));
    }

    #[tokio::test]
    async fn settled_batch_gives_up_after_one_empty_tick() {
        let (batches, _tx, _pending) = batch(0);
        let got = open(&batches, Duration::from_secs(5)).wait().await.unwrap();
        assert_eq!(got, None);
    }
}
