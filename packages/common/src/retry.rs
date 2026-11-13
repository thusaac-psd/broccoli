use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::Mutex;
use tracing::info;

use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryAttempt {
    pub attempt: u8,
    pub error: String,
    pub timestamp: DateTime<Utc>,
}

impl RetryAttempt {
    /// Attempt `attempt` failed with `error` at platform time `at`.
    pub fn new(attempt: u8, error: impl Into<String>, at: DateTime<Utc>) -> Self {
        Self {
            attempt,
            error: error.into(),
            timestamp: at,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RetryDecision {
    Retry {
        attempt: u8,
        history: Vec<RetryAttempt>,
    },
    Exhausted {
        history: Vec<RetryAttempt>,
    },
}

#[derive(Debug, Clone)]
struct RetryState {
    attempt: u8,
    history: Vec<RetryAttempt>,
    last_updated: Instant,
}

impl RetryState {
    fn new() -> Self {
        Self {
            attempt: 0,
            history: Vec::new(),
            last_updated: Instant::now(),
        }
    }
}

#[derive(Debug, Default)]
pub struct RetryTracker {
    state: HashMap<String, RetryState>,
    max_retries: u8,
}

impl RetryTracker {
    pub fn new(max_retries: u8) -> Self {
        Self {
            state: HashMap::new(),
            max_retries,
        }
    }

    /// Record that `id` failed with `error` at platform time `at`.
    pub fn record_failure(&mut self, id: &str, error: &str, at: DateTime<Utc>) -> RetryDecision {
        let retry_state = self
            .state
            .entry(id.to_string())
            .or_insert_with(RetryState::new);

        retry_state.attempt += 1;
        retry_state.last_updated = Instant::now();
        retry_state
            .history
            .push(RetryAttempt::new(retry_state.attempt, error, at));

        if retry_state.attempt <= self.max_retries {
            RetryDecision::Retry {
                attempt: retry_state.attempt,
                history: retry_state.history.clone(),
            }
        } else {
            let final_history = retry_state.history.clone();
            self.state.remove(id);
            RetryDecision::Exhausted {
                history: final_history,
            }
        }
    }

    pub fn clear(&mut self, id: &str) {
        self.state.remove(id);
    }

    pub fn get_attempt(&self, id: &str) -> u8 {
        self.state.get(id).map(|s| s.attempt).unwrap_or(0)
    }

    pub fn cleanup_stale(&mut self, max_age: Duration) {
        let now = Instant::now();
        self.state
            .retain(|_, state| now.duration_since(state.last_updated) < max_age);
    }

    pub fn len(&self) -> usize {
        self.state.len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.is_empty()
    }
}

pub fn calculate_backoff(attempt: u8, base_ms: u64, max_ms: u64) -> Duration {
    if attempt == 0 {
        return Duration::ZERO;
    }

    let exp_factor = 2u64.saturating_pow((attempt - 1) as u32);
    let delay_ms = base_ms.saturating_mul(exp_factor);

    let jitter = if delay_ms > 0 {
        rand::rng().random_range(0..=delay_ms / 4)
    } else {
        0
    };

    let total_delay = delay_ms.saturating_add(jitter).min(max_ms);
    Duration::from_millis(total_delay)
}

pub struct RetryCleanupGuard<'a> {
    tracker: &'a Arc<Mutex<RetryTracker>>,
    job_id: String,
    defused: bool,
}

impl<'a> RetryCleanupGuard<'a> {
    pub fn new(tracker: &'a Arc<Mutex<RetryTracker>>, job_id: impl Into<String>) -> Self {
        Self {
            tracker,
            job_id: job_id.into(),
            defused: false,
        }
    }

    pub fn defuse(&mut self) {
        self.defused = true;
    }
}

impl Drop for RetryCleanupGuard<'_> {
    fn drop(&mut self) {
        if !self.defused
            && let Ok(mut tracker) = self.tracker.try_lock()
        {
            tracker.clear(&self.job_id);
        }
    }
}

pub fn spawn_cleanup_task(
    tracker: Arc<Mutex<RetryTracker>>,
    cleanup_interval: Duration,
    max_age: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(cleanup_interval);

        loop {
            interval.tick().await;
            let removed = {
                let mut guard = tracker.lock().await;
                let before = guard.len();
                guard.cleanup_stale(max_age);
                before - guard.len()
            };
            if removed > 0 {
                info!(removed, "Cleaned up stale retry tracker entries");
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn backoff_attempt_1_is_base_delay() {
        let d = calculate_backoff(1, 1000, 60000);
        assert!(d.as_millis() >= 1000 && d.as_millis() <= 1250, "d={d:?}");
    }

    #[test]
    fn backoff_attempt_2_doubles_delay() {
        let d = calculate_backoff(2, 1000, 60000);
        assert!(d.as_millis() >= 2000 && d.as_millis() <= 2500, "d={d:?}");
    }

    #[test]
    fn backoff_attempt_3_quadruples_delay() {
        let d = calculate_backoff(3, 1000, 60000);
        assert!(d.as_millis() >= 4000 && d.as_millis() <= 5000, "d={d:?}");
    }

    #[test]
    fn backoff_is_capped_at_max_ms() {
        let d = calculate_backoff(10, 10000, 60000);
        assert!(d.as_millis() <= 60000, "d={d:?}");
    }

    #[test]
    fn backoff_attempt_zero_returns_zero() {
        assert_eq!(calculate_backoff(0, 1000, 60000), Duration::ZERO);
    }

    #[test]
    fn retry_tracker_exhaustion() {
        let mut tracker = RetryTracker::new(3);

        match tracker.record_failure("msg1", "error 1", at()) {
            RetryDecision::Retry { attempt, .. } => assert_eq!(attempt, 1),
            _ => panic!("expected Retry"),
        }

        match tracker.record_failure("msg1", "error 2", at()) {
            RetryDecision::Retry { attempt, .. } => assert_eq!(attempt, 2),
            _ => panic!("expected Retry"),
        }

        match tracker.record_failure("msg1", "error 3", at()) {
            RetryDecision::Retry { attempt, .. } => assert_eq!(attempt, 3),
            _ => panic!("expected Retry on attempt 3 with max_retries=3"),
        }

        match tracker.record_failure("msg1", "error 4", at()) {
            RetryDecision::Exhausted { history } => {
                assert_eq!(history.len(), 4);
                assert_eq!(history[0].attempt, 1);
                assert_eq!(history[3].attempt, 4);
            }
            _ => panic!("expected Exhausted"),
        }

        assert_eq!(tracker.get_attempt("msg1"), 0);
    }

    #[test]
    fn retry_tracker_cleared_after_success() {
        let mut tracker = RetryTracker::new(3);

        tracker.record_failure("msg1", "error", at());
        assert_eq!(tracker.get_attempt("msg1"), 1);

        tracker.clear("msg1");
        assert_eq!(tracker.get_attempt("msg1"), 0);
    }

    #[test]
    fn retry_tracker_tracks_messages_independently() {
        let mut tracker = RetryTracker::new(3);

        tracker.record_failure("msg1", "error", at());
        tracker.record_failure("msg2", "error", at());

        assert_eq!(tracker.get_attempt("msg1"), 1);
        assert_eq!(tracker.get_attempt("msg2"), 1);

        tracker.record_failure("msg1", "error", at());
        assert_eq!(tracker.get_attempt("msg1"), 2);
        assert_eq!(tracker.get_attempt("msg2"), 1);
    }

    #[test]
    fn cleanup_stale_removes_old_entries() {
        let mut tracker = RetryTracker::new(3);

        tracker.record_failure("msg1", "error", at());
        tracker.record_failure("msg2", "error", at());
        assert_eq!(tracker.len(), 2);

        tracker.cleanup_stale(Duration::ZERO);
        assert!(tracker.is_empty());
    }

    #[test]
    fn cleanup_stale_preserves_recent_entries() {
        let mut tracker = RetryTracker::new(3);

        tracker.record_failure("msg1", "error", at());

        tracker.cleanup_stale(Duration::from_secs(3600));
        assert_eq!(tracker.len(), 1);
        assert_eq!(tracker.get_attempt("msg1"), 1);
    }

    #[test]
    fn tracker_len_and_is_empty_reflect_state() {
        let mut tracker = RetryTracker::new(3);
        assert!(tracker.is_empty());
        assert_eq!(tracker.len(), 0);

        tracker.record_failure("msg1", "error", at());
        assert!(!tracker.is_empty());
        assert_eq!(tracker.len(), 1);

        tracker.clear("msg1");
        assert!(tracker.is_empty());
    }
}
