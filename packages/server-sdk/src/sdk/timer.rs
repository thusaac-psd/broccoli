use crate::error::SdkError;

// No mock-state field: like `Checker`, the host-target mock below has
// nothing to record or replay - it always reports success - so there is no
// wasm32-or-otherwise consumer to cfg-gate a state field to. See
// `sdk/checker.rs`'s doc comment and commit `fe8ae48e` for the "field is
// never read" warning this shape avoids.
pub struct Timer {}

impl Timer {
    pub fn new() -> Self {
        Self {}
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

#[cfg(not(target_arch = "wasm32"))]
impl Timer {
    pub fn schedule(&self, _fire_at_ms: i64, _key: &str, _payload: &str) -> Result<(), SdkError> {
        Ok(())
    }

    pub fn cancel(&self, _key: &str) -> Result<(), SdkError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_target_schedule_is_a_no_op_that_reports_success() {
        // The host-target build exists so plugin logic can be unit-tested off
        // wasm. Scheduling must not panic there, and must not silently look
        // like a failure either - a plugin's error path would then be exercised
        // in tests but never in production.
        let timer = Timer::new();
        assert!(timer.schedule(0, "k", "{}").is_ok());
        assert!(timer.cancel("k").is_ok());
    }
}
