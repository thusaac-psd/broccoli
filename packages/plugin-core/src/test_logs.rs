//! Every event logged while the crate's tests run, for tests that check what
//! the host logs. One global subscriber collects them, so a test filters for
//! the events it caused.

use parking_lot::Mutex;

/// One captured event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Logged {
    pub level: tracing::Level,
    pub target: String,
    /// Its fields, `name=value` each, the message among them.
    pub text: String,
}

/// The events logged so far; the first call installs the subscriber.
pub(crate) fn captured() -> &'static Mutex<Vec<Logged>> {
    use tracing_subscriber::layer::{Context, SubscriberExt};

    static LOGS: std::sync::OnceLock<Mutex<Vec<Logged>>> = std::sync::OnceLock::new();

    struct Render(String);
    impl tracing::field::Visit for Render {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            use std::fmt::Write as _;
            let _ = write!(self.0, " {}={:?}", field.name(), value);
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            use std::fmt::Write as _;
            let _ = write!(self.0, " {}={}", field.name(), value);
        }
    }
    struct Capture;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            let mut render = Render(String::new());
            event.record(&mut render);
            captured().lock().push(Logged {
                level: *event.metadata().level(),
                target: event.metadata().target().to_string(),
                text: render.0,
            });
        }
    }

    LOGS.get_or_init(|| {
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(Capture))
            .expect("no other test sets a global subscriber");
        Mutex::new(Vec::new())
    })
}
