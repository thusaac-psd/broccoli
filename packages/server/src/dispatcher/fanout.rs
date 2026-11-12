//! Bounded per-server fan-out semaphore for evaluator-batch test-case dispatch.
//!
//! `start_evaluate_batch` (`services/evaluate_batch.rs`) historically spawned
//! one `tokio::task` per test case in a tight loop. Under a 1000-submission
//! burst with ICPC's typical 20 testcases/submission, that produced 20,000
//! futures contending on the inner `evaluator_slots` semaphore in milliseconds.
//! Memory spiked from per-task allocations, `plugin_pool_contention_total`
//! inflated, and the scheduler thrashed waking permits.
//!
//! `FanoutSemaphore` caps the **spawn** itself: the dispatcher acquires a
//! permit before spawning each test-case task and the permit is held for the
//! whole task lifetime. This makes the outer fan-out a steady stream rather
//! than an explosion, regardless of incoming batch size.
//!
//! Companion to (but intentionally simpler than) `DispatcherSemaphore` in
//! [`crate::dispatcher::permits`]: that one has a queue + reject policy for
//! admission control at the public API boundary, this one just blocks.

use std::sync::Arc;
use std::time::Instant;

use opentelemetry::KeyValue;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Heuristic threshold below which an acquire is considered "instant" and the
/// saturation counter is not incremented. 1ms of jitter is well below scheduler
/// hop noise. The wait-duration histogram records every acquire regardless, so
/// operators see the full distribution; this threshold only gates the binary
/// "did we block" counter.
const SATURATION_THRESHOLD_MS: f64 = 1.0;

#[derive(Clone)]
pub struct FanoutSemaphore {
    permits: Arc<Semaphore>,
    metrics: Option<common::metrics::Metrics>,
}

impl FanoutSemaphore {
    /// Construct a fan-out semaphore with `concurrency` permits. Inputs of 0
    /// are clamped to 1 to avoid the `Semaphore::new(0)` "always-blocked"
    /// footgun.
    pub fn new(concurrency: usize, metrics: Option<common::metrics::Metrics>) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            metrics,
        }
    }

    /// Acquire one permit, awaiting if none is immediately available. Emits
    /// `broccoli.batch_evaluator.fanout.wait.duration` for every acquire (so
    /// the full distribution is visible), and increments
    /// `broccoli.batch_evaluator.fanout.saturated` only when the acquire
    /// blocked beyond `SATURATION_THRESHOLD_MS`.
    pub async fn acquire(&self) -> Result<OwnedSemaphorePermit, tokio::sync::AcquireError> {
        let start = Instant::now();
        let permit = self.permits.clone().acquire_owned().await?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1_000.0;

        if let Some(metrics) = self.metrics.as_ref() {
            let labels = [KeyValue::new("batch.kind", "evaluate")];
            metrics
                .batch_evaluator_fanout_wait_duration
                .record(elapsed_ms, &labels);
            if elapsed_ms >= SATURATION_THRESHOLD_MS {
                metrics
                    .batch_evaluator_fanout_saturated_total
                    .add(1, &labels);
            }
        }

        Ok(permit)
    }

    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::Notify;

    use super::*;

    #[test]
    fn new_with_zero_concurrency_uses_one_permit() {
        let sem = FanoutSemaphore::new(0, None);
        assert_eq!(sem.available_permits(), 1);
    }

    #[tokio::test]
    async fn acquire_returns_permit_when_available() {
        let sem = FanoutSemaphore::new(2, None);
        let permit = sem.acquire().await.expect("permit");
        assert_eq!(sem.available_permits(), 1);
        drop(permit);
        assert_eq!(sem.available_permits(), 2);
    }

    #[tokio::test]
    async fn acquire_blocks_until_permit_released() {
        let sem = FanoutSemaphore::new(1, None);
        let first = sem.acquire().await.expect("first permit");

        let sem2 = sem.clone();
        let notify = Arc::new(Notify::new());
        let notify_clone = notify.clone();

        let handle = tokio::spawn(async move {
            notify_clone.notify_one();
            let _permit = sem2.acquire().await.expect("second permit");
        });

        // Wait until the spawned task has started polling acquire().
        notify.notified().await;
        // Give the spawned task a chance to actually park on the semaphore.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !handle.is_finished(),
            "second acquire should still be blocked"
        );

        drop(first);
        // Now the spawned task should complete.
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("second acquire should resolve after permit drop")
            .expect("task should not panic");
    }

    #[tokio::test]
    async fn acquire_returns_err_when_semaphore_closed() {
        let sem = FanoutSemaphore::new(2, None);
        sem.permits.close();
        let err = sem.acquire().await;
        assert!(err.is_err(), "expected AcquireError after close()");
    }

    #[tokio::test]
    async fn dropping_permit_releases_slot() {
        let sem = FanoutSemaphore::new(3, None);
        let p1 = sem.acquire().await.expect("p1");
        let p2 = sem.acquire().await.expect("p2");
        assert_eq!(sem.available_permits(), 1);
        drop(p1);
        assert_eq!(sem.available_permits(), 2);
        drop(p2);
        assert_eq!(sem.available_permits(), 3);
    }
}
