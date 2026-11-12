//! The live result batches of one kind on this server.
//!
//! [`BatchTable`] owns every batch's lifecycle: it creates the result
//! channel, inserts the batch, and is the only code that removes it. The
//! `batch_active` gauge is therefore raised in [`BatchTable::open`] and
//! lowered in one private function with the same attributes, so it cannot
//! drift, and nothing outside the table can re-create a removed batch.
//!
//! A batch is live from `open` until it is removed: drained by the waiter that
//! took its last result, cancelled, failed while starting, superseded by a
//! retry, or reaped. The reaper expires a batch older than its maximum age in
//! two passes: the first marks it expired and runs the kind's expiry hook, the
//! next removes it.
//!
//! Each batch also carries kind-specific state `S`: the waiter correlation ids
//! of an operation batch, or the per-test-case progress of an evaluate batch.
//! It lives and dies with the batch.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use broccoli_server_sdk::types::TestCaseVerdict;
use common::worker::TaskResult;
use dashmap::DashMap;
use opentelemetry::KeyValue;

/// A result type that flows through a [`BatchTable`].
pub trait BatchResult: Send + 'static {
    /// The `batch.kind` metric attribute.
    const KIND: &'static str;
}

impl BatchResult for TaskResult {
    const KIND: &'static str = "operation";
}

impl BatchResult for TestCaseVerdict {
    const KIND: &'static str = "evaluate";
}

/// Why a batch leaves the table.
#[derive(Debug, Clone, Copy)]
pub enum Removal<'a> {
    /// Every result was produced and taken.
    Drained,
    /// The owning plugin (or the host on its behalf) cancelled it.
    Cancelled { plugin_id: &'a str },
    /// Starting the batch failed after it was opened.
    Failed,
    /// A retry replaced it with a fresh batch.
    Superseded,
    /// The reaper removed it after it expired.
    Reaped,
}

struct Batch<T, S> {
    result_rx: flume::Receiver<T>,
    pending: Arc<AtomicUsize>,
    created_at: Instant,
    expired: bool,
    state: S,
}

struct Inner<T, S> {
    batches: DashMap<String, Batch<T, S>>,
    metrics: Option<common::metrics::Metrics>,
}

/// The live batches of result type `T`, each with kind state `S`. Cloning
/// shares the table.
pub struct BatchTable<T, S> {
    inner: Arc<Inner<T, S>>,
}

impl<T, S> Clone for BatchTable<T, S> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T: BatchResult, S> Default for BatchTable<T, S> {
    fn default() -> Self {
        Self::new(None)
    }
}

/// The producing side of one batch: every result goes through
/// [`BatchSender::deliver`], which sends before it decrements the pending
/// count. A waiter that sees the batch as no longer pending therefore finds
/// the last result already buffered.
pub struct BatchSender<T> {
    tx: flume::Sender<T>,
    pending: Arc<AtomicUsize>,
    metrics: Option<common::metrics::Metrics>,
}

impl<T> Clone for BatchSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            pending: self.pending.clone(),
            metrics: self.metrics.clone(),
        }
    }
}

impl<T: BatchResult> BatchSender<T> {
    /// Deliver one of the batch's expected results. The channel holds exactly
    /// the expected number of results, so this never blocks; a result beyond
    /// that count is a producer bug and is dropped with an error log.
    pub fn deliver(&self, result: T) {
        match self.tx.try_send(result) {
            Ok(()) | Err(flume::TrySendError::Disconnected(_)) => {}
            Err(flume::TrySendError::Full(_)) => {
                tracing::error!(
                    batch_kind = T::KIND,
                    "Batch produced more results than it expected; dropping the extra result"
                );
                return;
            }
        }
        // `try_update` instead of `fetch_sub` so an over-delivery can never
        // wrap the count.
        let _ = self
            .pending
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1));
        if let Some(metrics) = self.metrics.as_ref() {
            metrics
                .batch_pending_items
                .add(-1, &[KeyValue::new("batch.kind", T::KIND)]);
        }
    }
}

impl<T: BatchResult, S> BatchTable<T, S> {
    pub fn new(metrics: Option<common::metrics::Metrics>) -> Self {
        Self {
            inner: Arc::new(Inner {
                batches: DashMap::new(),
                metrics,
            }),
        }
    }

    fn kind_attr() -> [KeyValue; 1] {
        [KeyValue::new("batch.kind", T::KIND)]
    }

    /// Open a batch that will produce exactly `expected` results, and return
    /// its producing side.
    pub fn open(&self, batch_id: &str, expected: usize, state: S) -> BatchSender<T> {
        // Capacity `expected` never blocks a producer; `max(1)` because a
        // zero-capacity flume channel is a rendezvous channel.
        let (tx, result_rx) = flume::bounded(expected.max(1));
        let pending = Arc::new(AtomicUsize::new(expected));
        self.inner.batches.insert(
            batch_id.to_string(),
            Batch {
                result_rx,
                pending: pending.clone(),
                created_at: Instant::now(),
                expired: false,
                state,
            },
        );
        if let Some(metrics) = self.inner.metrics.as_ref() {
            metrics.batch_active.add(1, &Self::kind_attr());
            metrics
                .batch_pending_items
                .add(expected as i64, &Self::kind_attr());
        }
        BatchSender {
            tx,
            pending,
            metrics: self.inner.metrics.clone(),
        }
    }

    /// Remove a batch, returning its kind state. `None` when it is already
    /// gone.
    pub fn remove(&self, batch_id: &str, reason: Removal<'_>) -> Option<S> {
        let (_, batch) = self.inner.batches.remove(batch_id)?;
        self.record_removal(reason);
        Some(batch.state)
    }

    /// Remove the batch once every result was produced and taken. Returns
    /// whether this call removed it.
    pub fn remove_if_drained(&self, batch_id: &str) -> bool {
        let removed = self
            .inner
            .batches
            .remove_if(batch_id, |_, batch| {
                batch.pending.load(Ordering::SeqCst) == 0 && batch.result_rx.is_empty()
            })
            .is_some();
        if removed {
            self.record_removal(Removal::Drained);
        }
        removed
    }

    fn record_removal(&self, reason: Removal<'_>) {
        let Some(metrics) = self.inner.metrics.as_ref() else {
            return;
        };
        metrics.batch_active.add(-1, &Self::kind_attr());
        match reason {
            Removal::Cancelled { plugin_id } => metrics.batch_cancelled_total.add(
                1,
                &[
                    KeyValue::new("batch.kind", T::KIND),
                    KeyValue::new("plugin.id", plugin_id.to_string()),
                ],
            ),
            Removal::Reaped => metrics.batch_reaped_total.add(
                1,
                &[
                    KeyValue::new("batch.kind", T::KIND),
                    KeyValue::new("phase", "reaped"),
                ],
            ),
            Removal::Drained | Removal::Failed | Removal::Superseded => {}
        }
    }

    /// The batch's result receiver and pending count, for a waiter.
    pub(crate) fn receiver(
        &self,
        batch_id: &str,
    ) -> Option<(flume::Receiver<T>, Arc<AtomicUsize>)> {
        self.inner
            .batches
            .get(batch_id)
            .map(|batch| (batch.result_rx.clone(), batch.pending.clone()))
    }

    /// Read the batch's kind state, if the batch is live.
    pub fn read<R>(&self, batch_id: &str, f: impl FnOnce(&S) -> R) -> Option<R> {
        self.inner
            .batches
            .get(batch_id)
            .map(|batch| f(&batch.state))
    }

    /// Update the batch's kind state, if the batch is live. A removed batch
    /// is never re-created.
    pub fn update<R>(&self, batch_id: &str, f: impl FnOnce(&mut S) -> R) -> Option<R> {
        self.inner
            .batches
            .get_mut(batch_id)
            .map(|mut batch| f(&mut batch.state))
    }

    pub fn metrics(&self) -> Option<&common::metrics::Metrics> {
        self.inner.metrics.as_ref()
    }

    pub fn contains(&self, batch_id: &str) -> bool {
        self.inner.batches.contains_key(batch_id)
    }

    /// One reaper pass at `now`: remove batches that expired on an earlier
    /// pass, and expire (running `on_expire`) live batches older than
    /// `max_age`.
    pub fn reap(&self, max_age: Duration, now: Instant, mut on_expire: impl FnMut(&str, &S)) {
        let mut expired_count = 0u32;
        let mut reaped = Vec::new();
        for mut entry in self.inner.batches.iter_mut() {
            if now.saturating_duration_since(entry.created_at) <= max_age {
                continue;
            }
            if entry.expired {
                reaped.push(entry.key().clone());
                continue;
            }
            on_expire(entry.key(), &entry.state);
            entry.expired = true;
            expired_count += 1;
        }
        if let Some(metrics) = self.inner.metrics.as_ref()
            && expired_count > 0
        {
            metrics.batch_reaped_total.add(
                u64::from(expired_count),
                &[
                    KeyValue::new("batch.kind", T::KIND),
                    KeyValue::new("phase", "poisoned"),
                ],
            );
        }
        let reaped_count = reaped
            .iter()
            .filter(|batch_id| self.remove(batch_id, Removal::Reaped).is_some())
            .count();
        if expired_count > 0 || reaped_count > 0 {
            tracing::warn!(
                expired_count,
                reaped_count,
                batch_kind = T::KIND,
                "Batch reaper cycle"
            );
        }
    }
}

impl<T: BatchResult, S: Send + Sync + 'static> BatchTable<T, S>
where
    T: Sync,
{
    /// Run [`BatchTable::reap`] once a minute for the life of the process.
    pub fn spawn_reaper(
        &self,
        max_age: Duration,
        on_expire: impl Fn(&str, &S) + Send + Sync + 'static,
    ) {
        let table = self.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(60));
            loop {
                interval.tick().await;
                table.reap(max_age, Instant::now(), &on_expire);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Table = BatchTable<TaskResult, Vec<&'static str>>;

    fn result(id: &str) -> TaskResult {
        TaskResult {
            task_id: id.into(),
            success: true,
            output: serde_json::json!({}),
            error: None,
            task_type: None,
            operation: None,
            worker_id: None,
            enqueued_at_unix_ms: None,
        }
    }

    #[test]
    fn batch_drains_only_after_every_result_is_taken() {
        let table = Table::default();
        let tx = table.open("b", 2, vec![]);
        let (rx, _) = table.receiver("b").unwrap();

        tx.deliver(result("1"));
        assert!(!table.remove_if_drained("b"), "one result still pending");
        tx.deliver(result("2"));
        assert!(!table.remove_if_drained("b"), "results still buffered");
        rx.try_recv().unwrap();
        rx.try_recv().unwrap();
        assert!(table.remove_if_drained("b"));
        assert!(!table.contains("b"));
    }

    #[test]
    fn delivery_beyond_the_expected_count_is_dropped_without_blocking() {
        let table = Table::default();
        let tx = table.open("b", 1, vec![]);
        tx.deliver(result("1"));
        tx.deliver(result("extra"));
        let (rx, pending) = table.receiver("b").unwrap();
        assert_eq!(pending.load(Ordering::SeqCst), 0);
        assert_eq!(rx.try_recv().unwrap().task_id, "1");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn removed_batch_is_never_recreated_by_a_state_update() {
        let table = Table::default();
        table.open("b", 1, vec!["k"]);
        assert_eq!(table.remove("b", Removal::Failed), Some(vec!["k"]));
        assert_eq!(table.update("b", |keys| keys.push("late")), None);
        assert!(!table.contains("b"));
        assert_eq!(table.remove("b", Removal::Failed), None);
    }

    #[test]
    fn reaper_expires_then_removes_on_the_next_pass() {
        let table = Table::default();
        table.open("old", 1, vec!["k"]);
        let max_age = Duration::from_secs(60);
        let start = Instant::now();

        let mut expired = Vec::new();
        table.reap(max_age, start, |id, _| expired.push(id.to_string()));
        assert!(expired.is_empty(), "a young batch is left alone");

        let later = start + Duration::from_secs(120);
        table.reap(max_age, later, |id, keys| {
            expired.push(format!("{id}:{}", keys.join(",")))
        });
        assert_eq!(expired, vec!["old:k"]);
        assert!(table.contains("old"), "the first pass only expires");

        table.reap(max_age, later, |_, _| panic!("expired only once"));
        assert!(!table.contains("old"));
    }
}
