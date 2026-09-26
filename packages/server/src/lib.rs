// `#[async_trait]` stamps its own `#[must_use]` on each rewritten async method
// even though the boxed future it returns is already `#[must_use]`, so clippy's
// `double_must_use` fires on macro output we don't control. The hits are the
// `services` async traits (`OperationTaskPublisher`, `WindowedSession`);
// suppress crate-wide, matching common/plugin-core/worker/stress-test.
#![allow(clippy::double_must_use)]

mod client_ip;
pub mod config;
pub mod consumers;
pub mod database;
pub mod dispatcher;
pub mod dlq;
pub mod entity;
pub mod error;
pub mod extractors;
pub mod handlers;
pub mod healthz_runtime;
pub mod hooks;
pub mod host_funcs;
pub mod manager;
pub mod middleware;
pub mod migration;
pub mod models;
pub mod registry;
pub mod routes;
pub mod runtime;
pub mod seed;
pub mod serve;
pub mod services;
pub mod state;
pub mod upload_limits;
pub mod utils;
pub mod visibility;

use std::path::{Path, PathBuf};
use std::time::Instant;

use axum::body::Body;
use axum::extract::{MatchedPath, State};
use axum::http::{HeaderMap, HeaderValue, Request, Response as HttpResponse, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use opentelemetry::KeyValue;
use tower::Service;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::{compression::CompressionLayer, timeout::TimeoutLayer, trace::TraceLayer};
use tracing::{info, info_span};
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::{Modify, OpenApi};
use utoipa_scalar::{Scalar, Servable as ScalarServable};
use utoipa_swagger_ui::SwaggerUi;
use uuid::Uuid;

use crate::state::AppState;

const X_REQUEST_ID: &str = "x-request-id";
const X_BROCCOLI_STRESS_RUN_ID: &str = "x-broccoli-stress-run-id";

#[cfg(test)]
pub(crate) static METRICS_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(crate) fn metrics_test_lock() -> std::sync::MutexGuard<'static, ()> {
    METRICS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "Broccoli Online Judge API",
        version = "1.0.0",
        description = "API for the Broccoli online judge system"
    ),
    servers(
        (url = "/api/v1", description = "Version 1 API server")
    ),
    tags(
        (name = "Auth", description = "Authentication and user management"),
        (name = "Problems", description = "Problem CRUD operations"),
        (name = "Test Cases", description = "Test case management for problems"),
        (name = "Contests", description = "Contest CRUD operations"),
        (name = "Contest Problems", description = "Managing problems within contests"),
        (name = "Contest Participants", description = "Managing contest participants"),
        (name = "Plugins", description = "WASM plugin management"),
        (name = "Dead Letter Queue", description = "Failed message management and retry"),
        (name = "Admin", description = "Administrative endpoints"),
        (name = "Additional Files", description = "Judge-private supplemental files attached to a problem"),
        (name = "Checker Source", description = "Custom checker source files"),
        (name = "Clarifications", description = "Contest Q&A messages between contestants and admins"),
        (name = "Code Runs", description = "Custom test runs (test-your-code feature)"),
        (name = "Config", description = "Server-side configuration"),
        (name = "I18n", description = "Localization resource bundles"),
        (name = "Meta", description = "Server metadata (version, build info)"),
        (name = "Plugin Config", description = "Per-plugin configuration values keyed by scope"),
        (name = "Problem Attachments", description = "Contestant-visible files attached to a problem"),
        (name = "Roles", description = "Role and permission management"),
        (name = "Submissions", description = "Submitted solutions to problems"),
        (name = "Telemetry", description = "Public client error and web-vitals reporting"),
        (name = "Users", description = "User account management"),
    ),
    modifiers(&SecurityAddon),
)]
struct ApiDoc;

struct SecurityAddon;

impl Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let components = openapi.components.get_or_insert_default();
        components.add_security_scheme(
            "jwt",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}

pub(crate) async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    use prometheus::Encoder;

    let encoder = prometheus::TextEncoder::new();
    let families = state.prometheus_registry.gather();
    let mut buf = Vec::new();
    encoder.encode(&families, &mut buf).unwrap();
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        buf,
    )
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

fn normalize_javascript_content_type<B>(mut response: HttpResponse<B>) -> HttpResponse<B> {
    if response
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value == "text/javascript")
    {
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/javascript"),
        );
    }

    response
}

fn request_id_from_headers(headers: &HeaderMap) -> String {
    headers
        .get(X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string())
}

/// Low-cardinality `http.route` label for metrics: the matched route TEMPLATE
/// (`/problems/{id}`), never the raw URI path. An unmatched request (a 404, or a
/// path with no route) collapses to a single `"<unmatched>"` bucket instead of
/// minting a fresh time series per distinct URI. Falling back to
/// `request.uri().path()` let any scanner hitting `/random/1`, `/random/2`, ...
/// mint unbounded `http.route` series and blow up metric cardinality. The raw
/// path is still logged (a log line's cardinality is free), just not used as a
/// metric attribute.
fn route_label_for_request<B>(request: &Request<B>) -> String {
    request
        .extensions()
        .get::<MatchedPath>()
        .map(|mp| mp.as_str().to_owned())
        .unwrap_or_else(|| "<unmatched>".to_owned())
}

fn stress_run_id_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(X_BROCCOLI_STRESS_RUN_ID)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
}

fn content_length_from_headers(headers: &HeaderMap) -> Option<f64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|n| n as f64)
}

async fn observability_middleware(
    State(state): State<AppState>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let request_id = request_id_from_headers(request.headers());
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        request.headers_mut().insert(X_REQUEST_ID, value);
    }
    let stress_run_id = stress_run_id_from_headers(request.headers());
    let request_body_size = content_length_from_headers(request.headers());

    let method = request.method().to_string();
    let route = route_label_for_request(&request);
    let path = request.uri().path().to_owned();
    let started = Instant::now();
    let in_flight_attrs = [
        KeyValue::new("http.request.method", method.clone()),
        KeyValue::new("http.route", route.clone()),
    ];

    state
        .metrics
        .http_requests_in_flight
        .add(1, &in_flight_attrs);

    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(X_REQUEST_ID, value);
    }

    let latency = started.elapsed();
    let status = response.status().as_u16();
    let response_body_size = content_length_from_headers(response.headers());
    let attrs = [
        KeyValue::new("http.request.method", method.clone()),
        KeyValue::new("http.route", route.clone()),
        KeyValue::new("http.response.status_code", status as i64),
    ];

    state
        .metrics
        .http_requests_in_flight
        .add(-1, &in_flight_attrs);
    state.metrics.http_requests_total.add(1, &attrs);
    state
        .metrics
        .http_request_duration
        .record(latency.as_secs_f64(), &attrs);
    if let Some(size) = request_body_size {
        state.metrics.http_request_body_size.record(size, &attrs);
    }
    if let Some(size) = response_body_size {
        state.metrics.http_response_body_size.record(size, &attrs);
    }

    let trace_id = common::observability::current_trace_id();

    info!(
        method = %method,
        path = %path,
        route = %route,
        request_id = %request_id,
        run_id = stress_run_id.as_deref().unwrap_or(""),
        trace_id = trace_id.as_deref().unwrap_or(""),
        status,
        latency_ms = latency.as_millis() as u64,
        request_body_bytes = request_body_size.unwrap_or(0.0) as u64,
        response_body_bytes = response_body_size.unwrap_or(0.0) as u64,
        "response",
    );

    response
}

fn frontend_assets_service(
    frontend_dist: impl AsRef<Path>,
) -> impl Clone
+ Send
+ Sync
+ 'static
+ Service<
    Request<axum::body::Body>,
    Response: IntoResponse,
    Error = std::convert::Infallible,
    Future: Send + 'static,
> {
    tower::ServiceBuilder::new()
        .map_response(normalize_javascript_content_type)
        .service(ServeDir::new(frontend_dist.as_ref().join("assets")))
}

fn spa_fallback_service(frontend_dist: impl Into<PathBuf>) -> ServeDir<ServeFile> {
    let frontend_dist = frontend_dist.into();
    ServeDir::new(&frontend_dist).fallback(ServeFile::new(frontend_dist.join("index.html")))
}

pub fn build_router(state: AppState) -> axum::Router {
    let trusted_proxies =
        client_ip::parse_trusted_proxy_networks(&state.config.server.trusted_proxies);
    let observability_state = state.clone();

    let (router, api) = routes::api_routes(&state.config, ApiDoc::openapi());
    let frontend_dist = state.config.server.frontend_dist.clone();

    let router = router.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        middleware::idempotency_middleware,
    ));
    let router = router.layer(TimeoutLayer::with_status_code(
        StatusCode::REQUEST_TIMEOUT,
        std::time::Duration::from_secs(60),
    ));

    #[cfg(feature = "bundled-stress-test")]
    let state_for_downloads = state.clone();

    let assets_router = axum::Router::new()
        .route(
            "/{plugin_id}/{*file_path}",
            axum::routing::get(handlers::assets::serve_plugin_asset),
        )
        .fallback_service(frontend_assets_service(&frontend_dist));

    // `/metrics` and `/healthz` are also served on a dedicated tokio runtime
    // + listener when `server.healthz_listen` is configured - see UP#14e in
    // `healthz_runtime`. Operators are encouraged to point load-balancer
    // probes and Prometheus scrapes at the dedicated listener so main-runtime
    // saturation (submission bursts, scheduler thrash) does not stall
    // liveness signals. The endpoints remain on the main router unchanged so
    // legacy probe configurations keep working.
    let app = axum::Router::new()
        .nest("/api", router.fallback(api_not_found))
        .route("/metrics", axum::routing::get(metrics_handler))
        .route("/healthz", axum::routing::get(handlers::health::healthz))
        .nest("/assets", assets_router)
        .fallback_service(spa_fallback_service(frontend_dist))
        .with_state(state)
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api.clone()))
        .merge(Scalar::with_url("/scalar", api));

    #[cfg(feature = "bundled-stress-test")]
    let app = app.merge(routes::downloads::router().with_state(state_for_downloads));

    app.layer(axum::middleware::from_fn_with_state(
        trusted_proxies,
        client_ip::client_ip_source_middleware,
    ))
    .layer(CompressionLayer::new())
    .layer(
        TraceLayer::new_for_http()
            .make_span_with(|request: &axum::http::Request<_>| {
                let path = request
                    .extensions()
                    .get::<MatchedPath>()
                    .map(|mp| mp.as_str().to_owned())
                    .unwrap_or_else(|| request.uri().path().to_owned());

                let request_id = request_id_from_headers(request.headers());
                let stress_run_id = stress_run_id_from_headers(request.headers());

                let span = info_span!(
                    "http_request",
                    method = %request.method(),
                    path,
                    request_id,
                    stress_run_id = stress_run_id.as_deref().unwrap_or(""),
                    status = tracing::field::Empty,
                    latency_ms = tracing::field::Empty,
                );

                if request.headers().contains_key("traceparent") {
                    use opentelemetry_http::HeaderExtractor;
                    use tracing_opentelemetry::OpenTelemetrySpanExt;

                    let parent_cx = opentelemetry::global::get_text_map_propagator(|prop| {
                        prop.extract(&HeaderExtractor(request.headers()))
                    });
                    let _ = span.set_parent(parent_cx);
                }

                span
            })
            .on_response(
                move |response: &axum::http::Response<_>,
                      latency: std::time::Duration,
                      span: &tracing::Span| {
                    span.record("status", response.status().as_u16());
                    span.record("latency_ms", latency.as_millis() as u64);
                },
            ),
    )
    .layer(axum::middleware::from_fn_with_state(
        observability_state,
        observability_middleware,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use axum::routing::get;
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn request_id_from_headers_preserves_valid_value() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static("stress-run-1-42"));

        assert_eq!(
            request_id_from_headers(&headers),
            "stress-run-1-42".to_string()
        );
    }

    #[tokio::test]
    async fn request_id_from_headers_generates_when_missing_or_invalid() {
        let headers = axum::http::HeaderMap::new();
        let generated = request_id_from_headers(&headers);
        assert!(Uuid::parse_str(&generated).is_ok());

        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-request-id", HeaderValue::from_static(""));
        let generated = request_id_from_headers(&headers);
        assert!(Uuid::parse_str(&generated).is_ok());
    }

    #[tokio::test]
    async fn spa_fallback_serves_index_for_root_and_client_routes() {
        let dist = tempfile::tempdir().unwrap();
        std::fs::write(dist.path().join("index.html"), "<html>app</html>").unwrap();
        let service = spa_fallback_service(dist.path().to_path_buf());

        for (method, path) in [(Method::GET, "/"), (Method::HEAD, "/contests/42")] {
            let response = service
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[axum::http::header::CONTENT_TYPE],
                "text/html"
            );
        }
    }

    #[tokio::test]
    async fn frontend_assets_service_serves_assets_without_spa_fallback() {
        let dist = tempfile::tempdir().unwrap();
        let assets = dist.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(assets.join("main.js"), "console.log('ok');").unwrap();
        let service = frontend_assets_service(dist.path());

        for method in [Method::GET, Method::HEAD] {
            let response = service
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri("/main.js")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let response = response.into_response();

            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                response.headers()[axum::http::header::CONTENT_TYPE],
                "application/javascript"
            );
        }

        let response = service
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/typo.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let response = response.into_response();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn frontend_asset_mount_keeps_plugin_asset_route_precedence() {
        let dist = tempfile::tempdir().unwrap();
        let assets = dist.path().join("assets");
        std::fs::create_dir(&assets).unwrap();
        std::fs::write(dist.path().join("index.html"), "<html>app</html>").unwrap();
        std::fs::write(dist.path().join("robots.txt"), "User-agent: *").unwrap();
        std::fs::write(assets.join("main.js"), "console.log('ok');").unwrap();

        let assets_router = axum::Router::new()
            .route(
                "/{plugin_id}/{*file_path}",
                get(|| async { "plugin asset" }),
            )
            .fallback_service(frontend_assets_service(dist.path()));
        let api_router = axum::Router::new()
            .route(
                "/v1/health",
                get(|| async { Json(json!({ "status": "ok" })) }),
            )
            .fallback(api_not_found);

        let app = axum::Router::new()
            .nest("/api", api_router)
            .nest("/assets", assets_router)
            .fallback_service(spa_fallback_service(dist.path().to_path_buf()));

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/assets/main.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/javascript"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/assets/typo.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/assets/plugin/foo.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/contests/42")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/html"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/robots.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/plain"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "application/json"
        );

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/v1/missing")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
