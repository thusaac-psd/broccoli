//! extism bounds a guest by its cancel and its manifest timeout from the
//! instant the call registers with extism's timer thread, which is before
//! any guest code of the call runs, instantiation and initialization
//! included. `deadline.rs` relies on this: it cancels each call once its
//! deadline passes and repeats the cancel only to cover a call that has not
//! registered yet.
//!
//! The cancel tests deliver a cancel at the earliest such instant. A tracing
//! subscriber sees the trace event extism emits on the calling thread right
//! after the call registers, sends the cancel from inside that event, and
//! holds the calling thread until the timer thread has handled it. The guest
//! then must trap instead of spinning forever.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::time::Duration;

use extism::{CancelHandle, Function, Manifest, Plugin, UserData, Val, ValType, Wasm};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// A guest whose `spin` export never returns.
const SPIN: &str = r#"(module (func (export "spin") (result i32) (loop (br 0)) (i32.const 0)))"#;

/// A guest whose start function never returns.
const SPIN_AT_START: &str = r#"(module
    (func $spin (loop (br 0)))
    (start $spin)
    (func (export "ok") (result i32) (i32.const 0)))"#;

/// A reactor whose `_initialize` spins while the host's `spin_init` says so
/// and then marks it initialized; `ready` succeeds only once it is.
const SPIN_IN_INITIALIZE: &str = r#"(module
    (import "extism:host/user" "spin_init" (func $spin_init (result i32)))
    (global $ready (mut i32) (i32.const 0))
    (func (export "_initialize")
        (if (call $spin_init) (then (loop (br 0))))
        (global.set $ready (i32.const 1)))
    (func (export "ready") (result i32)
        (i32.sub (i32.const 1) (global.get $ready))))"#;

/// A guest whose `dirty_then_park` export marks the instance dirty, calls the
/// host's `park` and then spins; `clean` succeeds only on a clean instance.
const DIRTY_THEN_PARK: &str = r#"(module
    (import "extism:host/user" "park" (func $park))
    (global $dirty (mut i32) (i32.const 0))
    (func (export "dirty_then_park") (result i32)
        (global.set $dirty (i32.const 1))
        (call $park)
        (loop (br 0))
        (i32.const 0))
    (func (export "clean") (result i32) (global.get $dirty)))"#;

const REGISTERED: &str = "call registered with the timer";
const CANCEL_HANDLED: &str = "handling cancel event";

/// A plugin whose calls a test cancels.
struct Target {
    cancel: CancelHandle,
    /// Whether to cancel each call as soon as it registers.
    on_register: bool,
    /// Cancel events the timer thread has handled for this plugin.
    handled: Mutex<u32>,
    handled_changed: Condvar,
}

impl Target {
    /// Cancel twice and wait until the timer thread handles the second: it
    /// handles actions in order, so by then the first cancel has fully taken
    /// effect.
    fn cancel_and_wait(&self) {
        *self.handled.lock().unwrap() = 0;
        self.cancel.cancel().expect("the timer thread runs");
        self.cancel.cancel().expect("the timer thread runs");
        let mut handled = self.handled.lock().unwrap();
        while *handled < 2 {
            handled = self.handled_changed.wait(handled).unwrap();
        }
    }

    fn cancel_handled(&self) {
        *self.handled.lock().unwrap() += 1;
        self.handled_changed.notify_all();
    }
}

/// The calls to cancel, by plugin id. One subscriber serves every test in
/// this binary, so each test registers its own plugin here.
#[derive(Default)]
struct Probe(Mutex<HashMap<String, Arc<Target>>>);

impl Probe {
    /// Cancel every call `plugin` makes, as soon as it registers.
    fn cancel_on_register(&self, plugin: &Plugin) {
        self.track(plugin, true);
    }

    /// Track `plugin` so a test can cancel its call with
    /// [`Target::cancel_and_wait`] at a point of its choosing.
    fn track(&self, plugin: &Plugin, on_register: bool) -> Arc<Target> {
        let cancel = plugin.cancel_handle();
        let target = Arc::new(Target {
            cancel: cancel.clone(),
            on_register,
            handled: Mutex::new(0),
            handled_changed: Condvar::new(),
        });
        self.0
            .lock()
            .unwrap()
            .insert(cancel.id.to_string(), target.clone());
        target
    }

    fn stop_cancelling(&self, plugin: &Plugin) {
        self.0
            .lock()
            .unwrap()
            .remove(&plugin.cancel_handle().id.to_string());
    }

    fn target(&self, plugin: &str) -> Option<Arc<Target>> {
        self.0.lock().unwrap().get(plugin).cloned()
    }
}

static PROBE: LazyLock<&'static Probe> = LazyLock::new(|| {
    let probe: &'static Probe = Box::leak(Box::default());
    tracing::subscriber::set_global_default(ProbeSubscriber(probe))
        .expect("no other subscriber in this test binary");
    probe
});

#[derive(Default)]
struct Fields {
    message: String,
    plugin: String,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = value.to_owned(),
            "plugin" => self.plugin = value.to_owned(),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let value = format!("{value:?}");
        self.record_str(field, value.trim_matches('"'));
    }
}

struct ProbeSubscriber(&'static Probe);

impl Subscriber for ProbeSubscriber {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let Some(target) = self.0.target(&fields.plugin) else {
            return;
        };
        match fields.message.as_str() {
            REGISTERED if target.on_register => target.cancel_and_wait(),
            CANCEL_HANDLED => target.cancel_handled(),
            _ => {}
        }
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

fn plugin(manifest: Manifest, functions: impl IntoIterator<Item = Function>) -> Plugin {
    Plugin::new(manifest, functions, false).expect("guest builds")
}

#[test]
fn a_cancel_handled_as_soon_as_the_call_registers_stops_the_guest() {
    let mut plugin = plugin(Manifest::new([Wasm::data(SPIN)]), []);
    PROBE.cancel_on_register(&plugin);

    let outcome = plugin.call::<_, ()>("spin", "");
    assert!(
        outcome.is_err(),
        "a spinning guest only returns once cancelled"
    );
}

#[test]
fn a_cancel_stops_a_guest_spinning_in_its_start_function() {
    let mut plugin = plugin(Manifest::new([Wasm::data(SPIN_AT_START)]), []);
    PROBE.cancel_on_register(&plugin);

    let outcome = plugin.call::<_, ()>("ok", "");
    assert!(
        outcome.is_err(),
        "the start function only ends once cancelled"
    );
}

#[test]
fn the_manifest_timeout_stops_a_guest_spinning_in_its_start_function() {
    // A zero timeout has passed by the time the timer thread registers the
    // call, so the timer stops the guest whenever it gets to it.
    let manifest = Manifest::new([Wasm::data(SPIN_AT_START)]).with_timeout(Duration::ZERO);
    let mut plugin = plugin(manifest, []);

    let outcome = plugin.call::<_, ()>("ok", "");
    assert!(
        outcome.is_err(),
        "the start function only ends once timed out"
    );
}

#[test]
fn a_call_after_a_cancelled_initialization_initializes_the_guest_again() {
    let spin = Arc::new(AtomicBool::new(true));
    let spin_init = Function::new(
        "spin_init",
        [],
        [ValType::I32],
        UserData::new(spin.clone()),
        |_, _, outputs, spin| {
            let spin = spin.get()?;
            outputs[0] = Val::I32(spin.lock().unwrap().load(Ordering::SeqCst).into());
            Ok(())
        },
    );
    let mut plugin = plugin(Manifest::new([Wasm::data(SPIN_IN_INITIALIZE)]), [spin_init]);

    PROBE.cancel_on_register(&plugin);
    let outcome = plugin.call::<_, ()>("ready", "");
    assert!(outcome.is_err(), "`_initialize` only ends once cancelled");

    PROBE.stop_cancelling(&plugin);
    spin.store(false, Ordering::SeqCst);
    plugin
        .call::<_, ()>("ready", "")
        .expect("the next call runs `_initialize` to completion first");
}

#[test]
fn the_call_after_a_guest_is_cancelled_mid_export_runs_on_a_fresh_instance() {
    // `park` cancels the call while the guest is inside it, after the guest
    // has changed its state; the guest traps at its next epoch check, the
    // loop after `park`.
    let target: UserData<Option<Arc<Target>>> = UserData::new(None);
    let park = Function::new("park", [], [], target.clone(), |_, _, _, target| {
        let target = target.get()?.lock().unwrap().clone();
        target
            .expect("the test tracks the plugin")
            .cancel_and_wait();
        Ok(())
    });
    let mut plugin = plugin(Manifest::new([Wasm::data(DIRTY_THEN_PARK)]), [park]);
    *target.get().unwrap().lock().unwrap() = Some(PROBE.track(&plugin, false));

    let outcome = plugin.call::<_, ()>("dirty_then_park", "");
    assert!(outcome.is_err(), "the guest traps after `park` returns");

    plugin
        .call::<_, ()>("clean", "")
        .expect("the next call does not see the cancelled call's state");
}
