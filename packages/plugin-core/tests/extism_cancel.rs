//! extism stops a guest cancelled at any instant after its call registers with
//! extism's timer thread. `deadline.rs` relies on this: it cancels each call
//! once its deadline passes and repeats the cancel only to cover a call that
//! has not registered yet.
//!
//! The test delivers a cancel at the earliest such instant. A tracing
//! subscriber sees the trace event extism emits on the calling thread right
//! after the call registers, sends the cancel from inside that event, and
//! holds the calling thread until the timer thread has handled it. The guest
//! then must trap instead of spinning forever.

use std::sync::{Arc, Condvar, Mutex};

use extism::{CancelHandle, Manifest, Plugin, Wasm};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// A guest whose only export never returns.
const SPIN: &str = r#"(module (func (export "spin") (result i32) (loop (br 0)) (i32.const 0)))"#;

const REGISTERED: &str = "call registered with the timer";
const CANCEL_HANDLED: &str = "handling cancel event";

#[derive(Default)]
struct Probe {
    target: Mutex<Option<CancelHandle>>,
    /// Cancel events the timer thread has handled for the target.
    handled: Mutex<u32>,
    handled_changed: Condvar,
}

impl Probe {
    fn target(&self) -> Option<CancelHandle> {
        self.target.lock().unwrap().clone()
    }

    /// Cancel the target twice and wait until the timer thread handles the
    /// second: it handles actions in order, so by then the first cancel has
    /// fully taken effect.
    fn cancel_and_wait(&self, cancel: &CancelHandle) {
        cancel.cancel().expect("the timer thread runs");
        cancel.cancel().expect("the timer thread runs");
        let mut handled = self.handled.lock().unwrap();
        while *handled < 2 {
            handled = self.handled_changed.wait(handled).unwrap();
        }
    }
}

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

struct ProbeSubscriber(Arc<Probe>);

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
        let Some(target) = self.0.target() else {
            return;
        };
        let mut fields = Fields::default();
        event.record(&mut fields);
        if fields.plugin != target.id.to_string() {
            return;
        }
        match fields.message.as_str() {
            REGISTERED => self.0.cancel_and_wait(&target),
            CANCEL_HANDLED => {
                *self.0.handled.lock().unwrap() += 1;
                self.0.handled_changed.notify_all();
            }
            _ => {}
        }
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

#[test]
fn a_cancel_handled_as_soon_as_the_call_registers_stops_the_guest() {
    let probe = Arc::new(Probe::default());
    tracing::subscriber::set_global_default(ProbeSubscriber(probe.clone()))
        .expect("no other subscriber in this test binary");

    let mut plugin =
        Plugin::new(Manifest::new([Wasm::data(SPIN)]), [], false).expect("guest builds");
    *probe.target.lock().unwrap() = Some(plugin.cancel_handle());

    let outcome = plugin.call::<_, ()>("spin", "");

    assert_eq!(
        *probe.handled.lock().unwrap(),
        2,
        "the cancels were delivered when the call registered"
    );
    assert!(
        outcome.is_err(),
        "a spinning guest only returns once cancelled"
    );
}
