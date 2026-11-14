//! The JSON log line every binary writes when `observability.log_format` is
//! JSON: one object per event with `timestamp`, `level`, `fields` and, inside a
//! span, `span` (the innermost span) and `spans` (root to innermost).
//!
//! An event may name the same field twice. OpenTelemetry's internal log macros
//! do this: they pass `message = "..."` and an empty format string, which
//! tracing also records as `message`. A JSON object with a repeated key is
//! ambiguous (most parsers keep the last, empty value), so each field name
//! appears once and keeps its first non-empty value.

use std::fmt;

use serde_json::{Map, Value};
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::{FormatTime, SystemTime};
use tracing_subscriber::fmt::{FmtContext, FormatEvent, FormatFields, FormattedFields};
use tracing_subscriber::registry::{LookupSpan, SpanRef};

/// The event formatter of the JSON log layer. Pair it with
/// `fmt::layer().json()`, whose `JsonFields` records span fields as JSON.
pub(super) struct JsonLines;

impl<S, N> FormatEvent<S, N> for JsonLines
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'w> FormatFields<'w> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: Writer<'_>,
        event: &Event<'_>,
    ) -> fmt::Result {
        let mut timestamp = String::new();
        SystemTime.format_time(&mut Writer::new(&mut timestamp))?;

        let mut fields = FieldMap::default();
        event.record(&mut fields);

        let mut line = Map::new();
        line.insert("timestamp".into(), Value::String(timestamp));
        line.insert(
            "level".into(),
            Value::String(event.metadata().level().to_string()),
        );
        line.insert("fields".into(), Value::Object(fields.0));

        if let Some(scope) = ctx.event_scope() {
            let spans: Vec<Value> = scope
                .from_root()
                .map(|span| span_json::<S, N>(&span))
                .collect();
            if let Some(innermost) = spans.last() {
                line.insert("span".into(), innermost.clone());
            }
            line.insert("spans".into(), Value::Array(spans));
        }

        let text = serde_json::to_string(&Value::Object(line)).map_err(|_| fmt::Error)?;
        writeln!(writer, "{text}")
    }
}

/// A span as `{ ...its fields, "name": <span name> }`.
fn span_json<S, N>(span: &SpanRef<'_, S>) -> Value
where
    S: for<'a> LookupSpan<'a>,
    N: 'static,
{
    // `JsonFields` stores each span's fields as a JSON object. A span whose
    // fields another formatter recorded keeps only its name.
    let mut object = span
        .extensions()
        .get::<FormattedFields<N>>()
        .and_then(|recorded| match serde_json::from_str(&recorded.fields) {
            Ok(Value::Object(fields)) => Some(fields),
            _ => None,
        })
        .unwrap_or_default();
    object.insert("name".into(), Value::String(span.name().to_string()));
    Value::Object(object)
}

/// An event's fields, each name once (see the module docs).
#[derive(Default)]
struct FieldMap(Map<String, Value>);

impl FieldMap {
    fn insert(&mut self, field: &Field, value: Value) {
        match self.0.get(field.name()) {
            Some(existing) if !is_empty(existing) => {}
            _ => {
                self.0.insert(field.name().to_string(), value);
            }
        }
    }
}

fn is_empty(value: &Value) -> bool {
    matches!(value, Value::String(s) if s.is_empty())
}

impl Visit for FieldMap {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.insert(field, Value::from(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.insert(field, Value::from(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.insert(field, Value::from(value));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.insert(field, Value::from(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.insert(field, Value::from(value));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.insert(field, Value::String(format!("{value:?}")));
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use tracing_subscriber::layer::SubscriberExt;

    use super::JsonLines;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn capture(emit: impl FnOnce()) -> String {
        let out = Capture::default();
        let writer = out.clone();
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .event_format(JsonLines)
                .with_writer(move || writer.clone()),
        );
        tracing::subscriber::with_default(subscriber, emit);
        let bytes = out.0.lock().unwrap().clone();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_field_named_twice_appears_once_with_its_non_empty_value() {
        let line = capture(|| {
            tracing::info!(name: "Provider.Set", name = "Provider.Set", message = "provider is set", "");
        });
        assert_eq!(line.matches("\"message\"").count(), 1, "{line}");
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["fields"]["message"], "provider is set");
        assert_eq!(parsed["fields"]["name"], "Provider.Set");
        assert_eq!(parsed["level"], "INFO");
    }

    #[test]
    fn an_event_inside_spans_lists_them_root_first() {
        let line = capture(|| {
            let outer = tracing::info_span!("outer", job = 7);
            let _outer = outer.enter();
            let inner = tracing::info_span!("inner", step = "load");
            let _inner = inner.enter();
            tracing::warn!(attempt = 2, "retrying");
        });
        let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(parsed["fields"]["message"], "retrying");
        assert_eq!(parsed["fields"]["attempt"], 2);
        assert_eq!(parsed["span"]["name"], "inner");
        assert_eq!(parsed["span"]["step"], "load");
        assert_eq!(parsed["spans"][0]["name"], "outer");
        assert_eq!(parsed["spans"][0]["job"], 7);
        assert_eq!(parsed["spans"].as_array().unwrap().len(), 2);
    }
}
