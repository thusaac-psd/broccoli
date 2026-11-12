use common::metrics::Metrics;
use opentelemetry::KeyValue;
use tracing::{error, info, warn};

pub struct TaskMetricGuard {
    metrics: Metrics,
    attrs: Vec<KeyValue>,
    worker_attrs: Vec<KeyValue>,
}

pub struct WorkerPermitMetricGuard {
    metrics: Metrics,
    attrs: [KeyValue; 1],
}

impl WorkerPermitMetricGuard {
    pub fn new(metrics: &Metrics, worker_id: &str) -> Self {
        let attrs = [KeyValue::new("worker_id", worker_id.to_string())];
        metrics.worker_permits_in_flight.add(1, &attrs);
        Self {
            metrics: metrics.clone(),
            attrs,
        }
    }
}

impl Drop for WorkerPermitMetricGuard {
    fn drop(&mut self) {
        self.metrics.worker_permits_in_flight.add(-1, &self.attrs);
    }
}

impl TaskMetricGuard {
    pub fn new(metrics: &Metrics, attrs: Vec<KeyValue>, worker_attrs: Vec<KeyValue>) -> Self {
        metrics.task_in_flight.add(1, &attrs);
        metrics.worker_active_tasks.add(1, &worker_attrs);
        Self {
            metrics: metrics.clone(),
            attrs,
            worker_attrs,
        }
    }
}

impl Drop for TaskMetricGuard {
    fn drop(&mut self) {
        self.metrics.task_in_flight.add(-1, &self.attrs);
        self.metrics.worker_active_tasks.add(-1, &self.worker_attrs);
    }
}

pub fn record_mq_publish(
    metrics: &Metrics,
    queue: &str,
    message_type: &'static str,
    outcome: &'static str,
    start: std::time::Instant,
) {
    let attrs = [
        KeyValue::new("queue", queue.to_string()),
        KeyValue::new("message.type", message_type),
        KeyValue::new("outcome", outcome),
    ];
    metrics
        .mq_publish_duration
        .record(start.elapsed().as_secs_f64(), &attrs);
    metrics.mq_publish_messages_total.add(1, &attrs);
}

pub fn record_mq_consume(
    metrics: &Metrics,
    queue: &str,
    message_type: &'static str,
    outcome: &'static str,
    attempts: u8,
    start: std::time::Instant,
) {
    let attrs = [
        KeyValue::new("queue", queue.to_string()),
        KeyValue::new("message.type", message_type),
        KeyValue::new("outcome", outcome),
        KeyValue::new("attempts", attempts as i64),
    ];
    metrics
        .mq_consume_duration
        .record(start.elapsed().as_secs_f64(), &attrs);
    metrics.mq_consume_messages_total.add(1, &attrs);
}

pub fn spawn_metrics_server(registry: prometheus::Registry, addr: String) {
    tokio::spawn(async move {
        let app = axum::Router::new().route(
            "/metrics",
            axum::routing::get(move || {
                let registry = registry.clone();
                async move {
                    use prometheus::Encoder;
                    let encoder = prometheus::TextEncoder::new();
                    let mut buf = Vec::new();
                    if let Err(e) = encoder.encode(&registry.gather(), &mut buf) {
                        error!(error = %e, "Failed to encode Prometheus metrics");
                    }
                    (
                        [(
                            axum::http::header::CONTENT_TYPE,
                            "text/plain; version=0.0.4; charset=utf-8",
                        )],
                        buf,
                    )
                }
            }),
        );

        let addr = addr.as_str();
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => {
                info!(addr, "Worker metrics endpoint listening");
                if let Err(e) = axum::serve(listener, app).await {
                    error!(error = %e, "Metrics server exited with error");
                }
            }
            Err(e) => {
                warn!(error = %e, addr, "Failed to bind worker metrics endpoint");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_permit_guard_records_in_flight_with_worker_label() {
        let (metrics, registry) =
            common::observability::init_metrics("broccoli-worker-permit-test");

        {
            let _guard = WorkerPermitMetricGuard::new(&metrics, "worker-a");
            let families = registry.gather();
            let permits = families
                .iter()
                .find(|family| family.name() == "broccoli_worker_permits_in_flight")
                .expect("worker permits in-flight metric should be exported");
            assert!(permits.get_metric().iter().any(|metric| {
                metric
                    .get_label()
                    .iter()
                    .any(|label| label.name() == "worker_id" && label.value() == "worker-a")
            }));
        }
    }
}
