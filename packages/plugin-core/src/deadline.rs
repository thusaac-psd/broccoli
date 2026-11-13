//! Cancelling a guest call at a deadline.
//!
//! An Extism call runs on the calling thread until the guest returns or
//! extism interrupts it, so a deadline cannot be enforced by dropping the
//! awaiting future: the guest keeps running. [`cancel_at`] instead watches
//! the deadline from a second thread and cancels the guest through its
//! [`CancelHandle`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use extism::CancelHandle;

/// How often the watchdog repeats a cancel past the deadline. A cancel only
/// interrupts a guest extism has already registered as running, and the
/// call may register just after the first cancel, so the watchdog keeps
/// cancelling until the call returns.
const CANCEL_RETRY: Duration = Duration::from_millis(10);

/// Run `call`, cancelling the guest it runs through `cancel` if it is still
/// running at `deadline`. Returns the call's result and whether the deadline
/// fired before it returned.
///
/// The watchdog thread is joined before this returns, so no cancel for this
/// instance is sent after it: one queued earlier reaches extism's timer
/// thread before the instance's next call registers, and finds nothing to
/// cancel.
pub(crate) fn cancel_at<R>(
    cancel: &CancelHandle,
    deadline: Instant,
    call: impl FnOnce() -> R,
) -> (R, bool) {
    let finished = Mutex::new(false);
    let wake = Condvar::new();
    let fired = AtomicBool::new(false);

    let result = std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut done = finished.lock().unwrap_or_else(|e| e.into_inner());
            while !*done {
                let now = Instant::now();
                let wait = if now < deadline {
                    deadline - now
                } else {
                    fired.store(true, Ordering::Release);
                    // Fails only once extism's timer thread is gone, and then
                    // there is no running guest left to cancel.
                    let _ = cancel.cancel();
                    CANCEL_RETRY
                };
                done = wake
                    .wait_timeout(done, wait)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
        });
        let result = call();
        *finished.lock().unwrap_or_else(|e| e.into_inner()) = true;
        wake.notify_one();
        result
    });
    (result, fired.load(Ordering::Acquire))
}

#[cfg(test)]
mod tests {
    use super::*;
    use extism::{Manifest, Plugin, Wasm};

    /// A guest with one export that never returns and one that returns at once.
    const GUEST: &str = r#"(module
        (func (export "spin") (result i32) (loop (br 0)) (i32.const 0))
        (func (export "ok") (result i32) (i32.const 0)))"#;

    fn guest() -> Plugin {
        Plugin::new(Manifest::new([Wasm::data(GUEST)]), [], false).expect("guest builds")
    }

    #[test]
    fn a_guest_running_at_the_deadline_is_cancelled() {
        let mut plugin = guest();
        let cancel = plugin.cancel_handle();
        let deadline = Instant::now() + Duration::from_millis(50);
        let (result, fired) = cancel_at(&cancel, deadline, || plugin.call::<_, ()>("spin", ""));
        assert!(fired);
        assert!(
            result.is_err(),
            "a spinning guest only returns once cancelled"
        );
    }

    #[test]
    fn a_guest_started_after_the_deadline_is_still_cancelled() {
        // The watchdog's first cancel goes out before the guest registers
        // with extism's timer; only a repeated cancel can stop it.
        let mut plugin = guest();
        let cancel = plugin.cancel_handle();
        let (result, fired) =
            cancel_at(&cancel, Instant::now(), || plugin.call::<_, ()>("spin", ""));
        assert!(fired);
        assert!(result.is_err());
    }

    #[test]
    fn a_guest_finishing_before_the_deadline_is_not_cancelled() {
        let mut plugin = guest();
        let cancel = plugin.cancel_handle();
        let deadline = Instant::now() + Duration::from_secs(3600);
        let (result, fired) = cancel_at(&cancel, deadline, || plugin.call::<_, ()>("ok", ""));
        assert!(!fired);
        result.expect("the guest returns on its own");
    }

    #[test]
    fn an_instance_runs_its_next_call_after_a_cancel() {
        let mut plugin = guest();
        let cancel = plugin.cancel_handle();
        let (first, _) = cancel_at(&cancel, Instant::now(), || plugin.call::<_, ()>("spin", ""));
        assert!(first.is_err());
        plugin
            .call::<_, ()>("ok", "")
            .expect("the next call on the instance runs to completion");
    }
}
