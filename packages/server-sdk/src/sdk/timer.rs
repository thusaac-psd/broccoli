use crate::error::SdkError;

pub struct Timer {
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) inner: TimerMock,
}

impl Timer {
    pub fn new() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            inner: TimerMock::new(),
        }
    }
}

impl Default for Timer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_arch = "wasm32")]
impl Timer {
    /// Schedule (or reschedule) a one-shot callback for `key` at
    /// `fire_at_ms` (Unix epoch milliseconds). Rescheduling an existing key
    /// replaces its payload and delivery state rather than adding a second
    /// timer.
    pub fn schedule(&self, fire_at_ms: i64, key: &str, payload: &str) -> Result<(), SdkError> {
        let input = serde_json::json!({
            "fire_at_ms": fire_at_ms,
            "key": key,
            "payload": payload,
        });
        unsafe { crate::host::raw::timer_schedule(serde_json::to_string(&input)?)? };
        Ok(())
    }

    /// Cancel a pending timer. Cancelling a key with nothing pending (already
    /// fired, or never scheduled) is not an error.
    pub fn cancel(&self, key: &str) -> Result<(), SdkError> {
        let input = serde_json::json!({ "key": key });
        unsafe { crate::host::raw::timer_cancel(serde_json::to_string(&input)?)? };
        Ok(())
    }
}

/// Host-target mock for [`Timer`]. Plugin logic (e.g. the afternoon-bracket
/// judging/advancement tests) is unit-tested off wasm and needs to assert
/// "this key was scheduled for this deadline" / "this key was cancelled"
/// without a real timer host to record it.
#[cfg(not(target_arch = "wasm32"))]
pub(super) struct TimerMock {
    /// Currently pending timers: key -> (fire_at_ms, payload). Rescheduling
    /// an existing key overwrites its entry here, matching the real host's
    /// documented semantics ("Scheduling an existing key REPLACES it").
    scheduled: std::cell::RefCell<std::collections::HashMap<String, (i64, String)>>,
    /// Every key ever cancelled, for `was_cancelled` -- kept even after the
    /// key is removed from `scheduled` so a test can assert the cancel
    /// happened at all, not just that the key is currently absent.
    cancelled: std::cell::RefCell<Vec<String>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl TimerMock {
    pub fn new() -> Self {
        Self {
            scheduled: std::cell::RefCell::new(std::collections::HashMap::new()),
            cancelled: std::cell::RefCell::new(Vec::new()),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Timer {
    pub fn schedule(&self, fire_at_ms: i64, key: &str, payload: &str) -> Result<(), SdkError> {
        self.inner
            .scheduled
            .borrow_mut()
            .insert(key.to_string(), (fire_at_ms, payload.to_string()));
        Ok(())
    }

    pub fn cancel(&self, key: &str) -> Result<(), SdkError> {
        // "Cancelling an absent key is a no-op, not an error" -- `remove`
        // already treats an absent key that way, so no presence check here.
        self.inner.scheduled.borrow_mut().remove(key);
        self.inner.cancelled.borrow_mut().push(key.to_string());
        Ok(())
    }

    /// Whether `key` currently has a pending (not yet cancelled) timer.
    pub fn is_scheduled(&self, key: &str) -> bool {
        self.inner.scheduled.borrow().contains_key(key)
    }

    /// The deadline `key` is currently scheduled for, if any.
    pub fn scheduled_at(&self, key: &str) -> Option<i64> {
        self.inner.scheduled.borrow().get(key).map(|(t, _)| *t)
    }

    /// Whether `key` was ever cancelled, regardless of whether it is
    /// currently scheduled again since (a reschedule after a cancel is
    /// legitimate and does not erase the cancel from history).
    pub fn was_cancelled(&self, key: &str) -> bool {
        self.inner.cancelled.borrow().iter().any(|k| k == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_target_schedule_reports_success() {
        // The host-target build exists so plugin logic can be unit-tested off
        // wasm. Scheduling must not panic there, and must not silently look
        // like a failure either - a plugin's error path would then be exercised
        // in tests but never in production.
        let timer = Timer::new();
        assert!(timer.schedule(0, "k", "{}").is_ok());
        assert!(timer.cancel("k").is_ok());
    }

    #[test]
    fn the_host_target_mock_records_a_scheduled_key_and_its_deadline() {
        // Plugin-side tests (e.g. the afternoon-bracket 小局 advancement
        // tests) need to assert "the next deadline was scheduled" without a
        // real timer host - a stateless no-op mock cannot support that.
        let timer = Timer::new();
        timer.schedule(1_600_000, "xiaoju:7:3:1", "{}").unwrap();
        assert!(timer.is_scheduled("xiaoju:7:3:1"));
        assert_eq!(timer.scheduled_at("xiaoju:7:3:1"), Some(1_600_000));
    }

    #[test]
    fn the_host_target_mock_records_a_cancelled_key_and_drops_it_from_scheduled() {
        let timer = Timer::new();
        timer.schedule(1_600_000, "xiaoju:7:3:0", "{}").unwrap();
        timer.cancel("xiaoju:7:3:0").unwrap();
        assert!(timer.was_cancelled("xiaoju:7:3:0"));
        assert!(
            !timer.is_scheduled("xiaoju:7:3:0"),
            "a cancelled key must no longer read back as scheduled"
        );
    }

    #[test]
    fn rescheduling_an_existing_key_replaces_its_deadline() {
        // Matches the real host's documented semantics: "Scheduling an
        // existing key REPLACES it."
        let timer = Timer::new();
        timer.schedule(1_000, "k", "{}").unwrap();
        timer.schedule(2_000, "k", "{}").unwrap();
        assert_eq!(timer.scheduled_at("k"), Some(2_000));
    }

    #[test]
    fn cancelling_an_absent_key_is_not_an_error() {
        // "Cancelling an absent key is a no-op, not an error."
        let timer = Timer::new();
        assert!(timer.cancel("never-scheduled").is_ok());
    }
}
