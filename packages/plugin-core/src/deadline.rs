//! Cancelling guest calls at their deadlines.
//!
//! An Extism call runs on its thread until the guest returns or extism
//! interrupts it, so a deadline cannot be enforced by dropping the awaiting
//! future: the guest keeps running. One process-wide thread instead keeps a
//! heap of the deadlines of every running call and cancels a guest through its
//! [`CancelHandle`] when its deadline passes. A guest parked in a host function
//! traps as soon as it returns to guest code.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use extism::CancelHandle;
use parking_lot::{Condvar, Mutex};

/// The first repeat of a cancel past the deadline. `watch` runs just before
/// `Plugin::call`, and extism registers the call with its timer thread a
/// little later, once it holds the instance and has replaced a store the
/// previous call left unusable; no guest code runs in that window, but a
/// cancel handled in it finds no call to stop. Any cancel handled after
/// registration traps the guest, instantiation and initialization included,
/// so the deadline thread keeps cancelling until the call returns, doubling
/// the gap up to [`CANCEL_RETRY_MAX`] so a guest parked in a long host
/// function after its cancel sees only a few more.
const CANCEL_RETRY: Duration = Duration::from_millis(10);
/// The longest gap between repeated cancels.
const CANCEL_RETRY_MAX: Duration = Duration::from_secs(1);

struct Entry {
    cancel: CancelHandle,
    fired: Arc<AtomicBool>,
    /// The gap before the next repeat of the cancel.
    retry: Duration,
}

#[derive(Default)]
struct State {
    heap: BinaryHeap<Reverse<(Instant, u64)>>,
    live: HashMap<u64, Entry>,
    next_id: u64,
}

struct Deadlines {
    state: Mutex<State>,
    wake: Condvar,
}

static DEADLINES: LazyLock<&'static Deadlines> = LazyLock::new(|| {
    let deadlines: &'static Deadlines = Box::leak(Box::new(Deadlines {
        state: Mutex::new(State::default()),
        wake: Condvar::new(),
    }));
    std::thread::Builder::new()
        .name("plugin-deadlines".into())
        .spawn(move || deadlines.run())
        .expect("spawn the plugin deadline thread");
    deadlines
});

impl Deadlines {
    fn run(&self) {
        let mut state = self.state.lock();
        loop {
            let Some(&Reverse((at, id))) = state.heap.peek() else {
                self.wake.wait(&mut state);
                continue;
            };
            if !state.live.contains_key(&id) {
                state.heap.pop();
                continue;
            }
            let now = Instant::now();
            if at > now {
                self.wake.wait_until(&mut state, at);
                continue;
            }
            state.heap.pop();
            if let Some(entry) = state.live.get_mut(&id) {
                entry.fired.store(true, Ordering::Release);
                // Fails only once extism's timer thread is gone, and then
                // there is no running guest left to cancel.
                let _ = entry.cancel.cancel();
                let retry = entry.retry;
                entry.retry = (retry * 2).min(CANCEL_RETRY_MAX);
                state.heap.push(Reverse((now + retry, id)));
            }
        }
    }
}

/// Cancels the guest behind `cancel` at `deadline` while alive. Dropped once
/// the call returns; no cancel for this call is sent after that, and one
/// queued earlier reaches extism's timer thread before the instance's next
/// call registers, so it finds nothing to cancel.
#[must_use = "the deadline is only watched while the guard lives"]
pub(crate) struct DeadlineGuard {
    id: u64,
    fired: Arc<AtomicBool>,
}

/// Watch a call that is about to start on the instance behind `cancel`.
pub(crate) fn watch(cancel: CancelHandle, deadline: Instant) -> DeadlineGuard {
    let deadlines = *DEADLINES;
    let fired = Arc::new(AtomicBool::new(false));
    let mut state = deadlines.state.lock();
    let id = state.next_id;
    state.next_id += 1;
    state.live.insert(
        id,
        Entry {
            cancel,
            fired: fired.clone(),
            retry: CANCEL_RETRY,
        },
    );
    let earliest = state
        .heap
        .peek()
        .is_none_or(|Reverse((at, _))| deadline < *at);
    state.heap.push(Reverse((deadline, id)));
    drop(state);
    if earliest {
        deadlines.wake.notify_one();
    }
    DeadlineGuard { id, fired }
}

impl DeadlineGuard {
    /// Whether the deadline passed while the call ran.
    pub(crate) fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        DEADLINES.state.lock().live.remove(&self.id);
    }
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

    fn call_until(plugin: &mut Plugin, func: &str, deadline: Instant) -> (bool, bool) {
        let guard = watch(plugin.cancel_handle(), deadline);
        let ok = plugin.call::<_, ()>(func, "").is_ok();
        (ok, guard.fired())
    }

    #[test]
    fn a_guest_running_at_the_deadline_is_cancelled() {
        let mut plugin = guest();
        let (ok, fired) = call_until(
            &mut plugin,
            "spin",
            Instant::now() + Duration::from_millis(50),
        );
        assert!(fired);
        assert!(!ok, "a spinning guest only returns once cancelled");
    }

    #[test]
    fn a_guest_started_after_the_deadline_is_still_cancelled() {
        let mut plugin = guest();
        let (ok, fired) = call_until(&mut plugin, "spin", Instant::now());
        assert!(fired);
        assert!(!ok);
    }

    #[test]
    fn a_guest_finishing_before_the_deadline_is_not_cancelled() {
        let mut plugin = guest();
        let (ok, fired) = call_until(
            &mut plugin,
            "ok",
            Instant::now() + Duration::from_secs(3600),
        );
        assert!(!fired);
        assert!(ok);
    }

    #[test]
    fn an_instance_runs_its_next_call_after_a_cancel() {
        let mut plugin = guest();
        let (ok, _) = call_until(&mut plugin, "spin", Instant::now());
        assert!(!ok);
        plugin
            .call::<_, ()>("ok", "")
            .expect("the next call on the instance runs to completion");
    }

    #[test]
    fn one_thread_watches_many_calls_with_the_earliest_first() {
        let mut late = guest();
        let mut early = guest();
        let late_guard = watch(
            late.cancel_handle(),
            Instant::now() + Duration::from_secs(3600),
        );
        let (ok, fired) = call_until(
            &mut early,
            "spin",
            Instant::now() + Duration::from_millis(20),
        );
        assert!(fired && !ok);
        assert!(!late_guard.fired());
        late.call::<_, ()>("ok", "").expect("not cancelled");
        drop(late_guard);
    }
}
