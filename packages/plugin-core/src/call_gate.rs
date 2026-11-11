//! Tracks plugin calls in flight so a server can shut down cleanly.
//!
//! A plugin call runs on a blocking thread, and its host functions bridge
//! back into the async runtime with `Handle::block_on`. If the runtime is torn
//! down while such a call is still running, the bridge panics ("A Tokio 1.x
//! context was found, but it is being shutdown") and a normal stop looks
//! like a crash. The server therefore [`CallGate::close`]s the gate first, so
//! new calls fail cleanly, and then [`CallGate::drain`]s the calls already
//! running before it lets the runtime go.
//!
//! The gate is re-entrant: a call made from inside a call it already admitted
//! (a host function that calls back into a plugin, on the same blocking
//! thread) is let through after close, or the drain would wait on calls it
//! is itself refusing.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::Notify;

#[derive(Default)]
struct Inner {
    in_flight: AtomicUsize,
    closed: AtomicBool,
    idle: Notify,
}

thread_local! {
    /// How many admitted calls this thread is currently running.
    static IN_CALL: Cell<u32> = const { Cell::new(0) };
}

/// Marks the current thread as running an admitted call while alive. Held
/// inside the blocking closure that runs the guest.
pub struct ThreadInCall(());

impl ThreadInCall {
    pub fn enter() -> Self {
        IN_CALL.with(|c| c.set(c.get() + 1));
        Self(())
    }
}

impl Drop for ThreadInCall {
    fn drop(&mut self) {
        IN_CALL.with(|c| c.set(c.get().saturating_sub(1)));
    }
}

/// Cheap to clone; clones share one gate.
#[derive(Clone, Default)]
pub struct CallGate(Arc<Inner>);

/// Held for the duration of one plugin call; the call counts as in flight
/// until it is dropped.
pub struct CallGuard(Arc<Inner>);

impl CallGate {
    /// Admit one call, or `None` once the gate is closed, unless this thread
    /// is already running an admitted call (a nested call).
    pub fn enter(&self) -> Option<CallGuard> {
        self.0.in_flight.fetch_add(1, Ordering::SeqCst);
        let nested = IN_CALL.with(|c| c.get() > 0);
        if self.0.closed.load(Ordering::SeqCst) && !nested {
            drop(CallGuard(self.0.clone()));
            return None;
        }
        Some(CallGuard(self.0.clone()))
    }

    /// Refuse every call from now on.
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
    }

    /// Wait until no call is in flight, for at most `timeout`. Returns
    /// whether the gate drained.
    pub async fn drain(&self, timeout: Duration) -> bool {
        let idle = async {
            loop {
                let notified = self.0.idle.notified();
                if self.0.in_flight.load(Ordering::SeqCst) == 0 {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(timeout, idle).await.is_ok()
    }

    /// Calls currently in flight.
    pub fn in_flight(&self) -> usize {
        self.0.in_flight.load(Ordering::SeqCst)
    }
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_closed_gate_refuses_calls_and_drains_once_running_ones_end() {
        let gate = CallGate::default();
        let running = gate.enter().expect("open gate admits");
        gate.close();
        assert!(gate.enter().is_none(), "a closed gate refuses new calls");
        assert_eq!(gate.in_flight(), 1, "a refused call does not count");
        assert!(
            !gate.drain(Duration::from_millis(50)).await,
            "still draining while a call runs"
        );
        let waiter = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.drain(Duration::from_secs(5)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(running);
        assert!(waiter.await.unwrap(), "drained when the last call ended");
    }

    #[test]
    fn a_nested_call_is_admitted_after_close() {
        let gate = CallGate::default();
        let outer = gate.enter().expect("open gate admits");
        let in_call = ThreadInCall::enter();
        gate.close();
        let nested = gate.enter();
        assert!(
            nested.is_some(),
            "a call made from inside an admitted call gets in"
        );
        drop(nested);
        drop(in_call);
        assert!(gate.enter().is_none(), "but not a fresh one");
        drop(outer);
        assert_eq!(gate.in_flight(), 0);
    }
}
