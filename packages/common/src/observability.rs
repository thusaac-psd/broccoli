use opentelemetry::metrics::MeterProvider as _;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::config::ObservabilityConfig;

pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take()
            && let Err(e) = provider.shutdown()
        {
            eprintln!("OpenTelemetry shutdown error: {e}");
        }
    }
}

pub fn init_tracing(config: &ObservabilityConfig) -> TelemetryGuard {
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    let is_json = config.json_logs();

    let filter_directive = std::env::var("RUST_LOG").unwrap_or_else(|_| config.log_filter.clone());

    let provider = if !config.otlp.endpoint.is_empty() {
        match build_otel_provider(config) {
            Ok(prov) => Some(prov),
            Err(e) => {
                eprintln!("Failed to initialize OpenTelemetry, continuing without: {e}");
                None
            }
        }
    } else {
        None
    };

    match (is_json, &provider) {
        (true, Some(p)) => {
            let otel = tracing_opentelemetry::layer().with_tracer(p.tracer("broccoli"));
            tracing_subscriber::registry()
                .with(EnvFilter::new(&filter_directive))
                .with(tracing_subscriber::fmt::layer().json().with_target(false))
                .with(otel)
                .init();
        }
        (true, None) => {
            tracing_subscriber::registry()
                .with(EnvFilter::new(&filter_directive))
                .with(tracing_subscriber::fmt::layer().json().with_target(false))
                .init();
        }
        (false, Some(p)) => {
            let otel = tracing_opentelemetry::layer().with_tracer(p.tracer("broccoli"));
            tracing_subscriber::registry()
                .with(EnvFilter::new(&filter_directive))
                .with(tracing_subscriber::fmt::layer().with_target(false))
                .with(otel)
                .init();
        }
        (false, None) => {
            tracing_subscriber::registry()
                .with(EnvFilter::new(&filter_directive))
                .with(tracing_subscriber::fmt::layer().with_target(false))
                .init();
        }
    }

    TelemetryGuard { provider }
}

pub fn inject_trace_context() -> Option<String> {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let span = tracing::Span::current();
    let otel_cx = span.context();
    let span_ref = otel_cx.span();
    let sc = span_ref.span_context();

    if !sc.is_valid() {
        return None;
    }

    let flags = sc.trace_flags().to_u8();
    Some(format!(
        "00-{}-{}-{:02x}",
        sc.trace_id(),
        sc.span_id(),
        flags
    ))
}

pub fn current_trace_id() -> Option<String> {
    use opentelemetry::trace::TraceContextExt;
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let span = tracing::Span::current();
    let otel_cx = span.context();
    let span_ref = otel_cx.span();
    let sc = span_ref.span_context();

    if !sc.is_valid() {
        return None;
    }

    Some(sc.trace_id().to_string())
}

pub fn extract_trace_context(traceparent: &str) -> Option<opentelemetry::Context> {
    use opentelemetry::trace::{
        SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState,
    };

    let parts: Vec<&str> = traceparent.split('-').collect();
    if parts.len() != 4 || parts[0] != "00" {
        return None;
    }

    let trace_id = TraceId::from_hex(parts[1]).ok()?;
    let span_id = SpanId::from_hex(parts[2]).ok()?;
    let flags = u8::from_str_radix(parts[3], 16).ok()?;

    let span_context = SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::new(flags),
        true,
        TraceState::default(),
    );

    if !span_context.is_valid() {
        return None;
    }

    let cx = opentelemetry::Context::new().with_remote_span_context(span_context);
    Some(cx)
}

pub fn init_metrics(service_name: &str) -> (crate::metrics::Metrics, prometheus::Registry) {
    let registry = prometheus::Registry::new();

    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry.clone())
        .build()
        .expect("Failed to build Prometheus exporter");

    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service_name.to_string())
        .build();

    let provider = SdkMeterProvider::builder()
        .with_reader(exporter)
        .with_resource(resource)
        .build();

    let scope = opentelemetry::InstrumentationScope::builder(service_name.to_string()).build();
    let meter = provider.meter_with_scope(scope);
    let metrics = crate::metrics::Metrics::new(&meter);

    opentelemetry::global::set_meter_provider(provider);

    (metrics, registry)
}

/// Build a [`Metrics`](crate::metrics::Metrics) backed by a LOCAL meter provider
/// that is NOT installed as the global provider. Intended for tests: calling
/// [`init_metrics`] from multiple tests races on `global::set_meter_provider`,
/// and the last install drops the provider the earlier test's meter depends on,
/// making metric assertions flaky. The returned provider must be kept alive by
/// the caller for the duration of the test so its meter keeps recording.
pub fn init_metrics_local(service_name: &str) -> (crate::metrics::Metrics, SdkMeterProvider) {
    let registry = prometheus::Registry::new();
    let exporter = opentelemetry_prometheus::exporter()
        .with_registry(registry)
        .build()
        .expect("Failed to build Prometheus exporter");
    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(service_name.to_string())
        .build();
    let provider = SdkMeterProvider::builder()
        .with_reader(exporter)
        .with_resource(resource)
        .build();
    let scope = opentelemetry::InstrumentationScope::builder(service_name.to_string()).build();
    let meter = provider.meter_with_scope(scope);
    let metrics = crate::metrics::Metrics::new(&meter);
    (metrics, provider)
}

fn build_otel_provider(
    config: &ObservabilityConfig,
) -> Result<SdkTracerProvider, Box<dyn std::error::Error + Send + Sync>> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&config.otlp.endpoint)
        .build()?;

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(config.otlp.service_name.clone())
                .build(),
        )
        .build();

    Ok(provider)
}
