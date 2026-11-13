mod client_ip;
pub use common::clock;
pub mod config;
pub mod consumers;
pub mod database;
pub mod dispatcher;
pub mod dlq;
pub mod entity;
pub mod error;
pub mod extractors;
pub mod handlers;
pub mod hooks;
pub mod host_funcs;
pub mod manager;
pub mod middleware;
pub mod migration;
pub mod models;
pub mod plugin_archive;
pub mod plugin_distribution;
pub mod plugin_fleet;
pub mod plugin_id;
pub mod plugin_lifecycle;
pub mod registry;
pub mod routes;
pub mod runtime;
pub mod seed;
pub mod serve;
pub mod services;
pub mod state;
#[cfg(test)]
#[allow(clippy::disallowed_methods)] // Fixtures are stamped with real time.
mod test_support;
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
use tower_http::set_header::SetResponseHeader;
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

async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
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
        .map_response(cache_hashed_asset_forever)
        .service(ServeDir::new(frontend_dist.as_ref().join("assets")))
}

/// Host assets under `/assets/*` are content-hashed, and the web build's
/// shared-deps plugin writes its import map before hashing, so a file name
/// pins its bytes and a browser may keep it for good. Only successful (or
/// `304`) responses are marked: a 404 for a chunk a redeploy removed must not
/// be cached, or that browser could never load the chunk again. Plugin assets
/// are served by their own route and are not content-hashed, so they never
/// reach this service.
fn cache_hashed_asset_forever<B>(mut response: HttpResponse<B>) -> HttpResponse<B> {
    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    response
}

/// `index.html`, client routes and the other unhashed root files. Served
/// `no-cache` so a browser revalidates them on every load: a stale
/// `index.html` would reference hashed chunks a redeploy no longer has.
fn spa_fallback_service(
    frontend_dist: impl Into<PathBuf>,
) -> SetResponseHeader<ServeDir<ServeFile>, HeaderValue> {
    let frontend_dist = frontend_dist.into();
    SetResponseHeader::overriding(
        ServeDir::new(&frontend_dist).fallback(ServeFile::new(frontend_dist.join("index.html"))),
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    )
}

/// Upper bound on handling one API request.
pub(crate) const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// How long before [`REQUEST_TIMEOUT`] the request's plugin calls give up, so
/// a call that cannot finish answers 503 (retry) before the timeout layer
/// answers 408 (outcome unknown).
pub(crate) const PLUGIN_DEADLINE_MARGIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The deadline of a request's plugin calls, measured from its start.
const PLUGIN_DEADLINE: std::time::Duration = REQUEST_TIMEOUT.saturating_sub(PLUGIN_DEADLINE_MARGIN);

tokio::task_local! {
    /// When the plugin calls of the request being handled reach their deadline.
    static REQUEST_DEADLINE: std::time::Instant;
}

/// The deadline of every interactive plugin call the request this task is
/// handling makes: [`PLUGIN_DEADLINE_MARGIN`] before the request times out.
/// Outside a request (a test driving a handler's logic directly) that long
/// from now.
pub(crate) fn request_deadline() -> std::time::Instant {
    REQUEST_DEADLINE
        .try_with(|deadline| *deadline)
        .unwrap_or_else(|_| std::time::Instant::now() + PLUGIN_DEADLINE)
}

/// Options for an interactive plugin call made by the current request.
pub(crate) fn interactive_call() -> plugin_core::executor::CallOptions {
    plugin_core::executor::CallOptions::interactive(request_deadline())
}

/// Records the request's deadline for [`request_deadline`].
async fn with_request_deadline(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    REQUEST_DEADLINE
        .scope(
            std::time::Instant::now() + PLUGIN_DEADLINE,
            next.run(request),
        )
        .await
}

/// The server's OpenAPI document as pretty-printed JSON: what
/// `/api-docs/openapi.json` serves, and what `packages/web-sdk`'s `schema.ts`
/// is generated from.
pub fn openapi_json(config: &config::AppConfig) -> String {
    let (_, api) = routes::api_routes(config, ApiDoc::openapi());
    api.to_pretty_json()
        .expect("the OpenAPI document serializes to JSON")
}

/// The whole HTTP app. Fails on a CORS origin that is not a header value.
pub fn build_router(state: AppState) -> anyhow::Result<axum::Router> {
    let trusted_proxies =
        client_ip::parse_trusted_proxy_networks(&state.config.server.trusted_proxies);
    let observability_state = state.clone();
    let clock = state.clock.clone();
    let state_cors = state.config.server.cors.clone();

    let (router, api) = routes::api_routes(&state.config, ApiDoc::openapi());
    let frontend_dist = state.config.server.frontend_dist.clone();

    // The timeout sits INSIDE the idempotency middleware so a timed-out
    // handler surfaces as a 408 the middleware settles (as outcome unknown),
    // rather than cancelling the middleware with its key still pending.
    let router = router
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(axum::middleware::from_fn(with_request_deadline));
    let router = router.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        middleware::idempotency_middleware,
    ));

    #[cfg(feature = "bundled-stress-test")]
    let state_for_downloads = state.clone();

    let assets_router = axum::Router::new()
        .route(
            "/{plugin_id}/{*file_path}",
            axum::routing::get(handlers::assets::serve_plugin_asset),
        )
        .fallback_service(frontend_assets_service(&frontend_dist));

    // `/livez` is the shallow liveness probe for load balancers (no dependency
    // calls); `/healthz` is the deep readiness check (DB + Redis).
    let app = axum::Router::new()
        .nest("/api", router.fallback(api_not_found))
        .route("/metrics", axum::routing::get(metrics_handler))
        .route("/healthz", axum::routing::get(handlers::health::healthz))
        .route("/livez", axum::routing::get(handlers::health::livez))
        .nest("/assets", assets_router)
        .fallback_service(spa_fallback_service(frontend_dist))
        .with_state(state)
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", api.clone()))
        .merge(Scalar::with_url("/scalar", api));

    #[cfg(feature = "bundled-stress-test")]
    let app = app.merge(routes::downloads::router().with_state(state_for_downloads));

    let cors = cors_layer(&state_cors)?;
    Ok(app
        .layer(axum::middleware::from_fn_with_state(
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
        // Inside the `Date` layer, so a preflight it answers itself carries the
        // platform clock too.
        .layer(cors)
        // Outermost: every response, whichever layer produced it.
        .layer(axum::middleware::from_fn_with_state(
            clock,
            date_header_middleware,
        )))
}

/// CORS for a split frontend origin. `expose_headers` lists every response
/// header the web client reads cross-origin: download filenames, rate-limit
/// backoff, the request id for error reports and `Date` for the server clock
/// offset.
fn cors_layer(cors: &config::CorsConfig) -> anyhow::Result<tower_http::cors::CorsLayer> {
    let allow_origins = cors
        .allow_origins
        .iter()
        .map(|origin| {
            origin
                .parse::<HeaderValue>()
                .map_err(|_| anyhow::anyhow!("Invalid CORS origin: {origin}"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(tower_http::cors::CorsLayer::new()
        .allow_origin(allow_origins)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::PUT,
            axum::http::Method::PATCH,
            axum::http::Method::DELETE,
        ])
        .allow_headers([
            axum::http::HeaderName::from_static("content-type"),
            axum::http::HeaderName::from_static("authorization"),
            axum::http::HeaderName::from_static("idempotency-key"),
            axum::http::HeaderName::from_static("x-request-id"),
            axum::http::HeaderName::from_static("x-broccoli-stress-run-id"),
        ])
        .expose_headers([
            axum::http::header::CONTENT_DISPOSITION,
            axum::http::header::DATE,
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderName::from_static("x-request-id"),
        ])
        .allow_credentials(true)
        .max_age(std::time::Duration::from_secs(cors.max_age)))
}

/// Stamps every response's `Date` from the platform clock, so all replicas
/// tell clients the same time (the web client derives its server clock from
/// it). Hyper only adds its own, from this host's clock, when none is set.
async fn date_header_middleware(
    axum::extract::State(clock): axum::extract::State<clock::PlatformClock>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(axum::http::header::DATE, http_date(clock.now()));
    response
}

/// `at` as an HTTP-date (RFC 9110 IMF-fixdate).
fn http_date(at: chrono::DateTime<chrono::Utc>) -> axum::http::HeaderValue {
    let text = at.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
    axum::http::HeaderValue::from_str(&text).expect("an HTTP-date is a valid header value")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;

    #[tokio::test]
    async fn cors_exposes_the_headers_the_web_client_reads() {
        let cors = crate::config::CorsConfig {
            allow_origins: vec!["http://web.example".into()],
            ..Default::default()
        };
        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "ok" }))
            .layer(cors_layer(&cors).unwrap());
        let res = app
            .oneshot(
                Request::get("/")
                    .header(axum::http::header::ORIGIN, "http://web.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let exposed = res
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();
        for name in ["content-disposition", "date", "retry-after", "x-request-id"] {
            assert!(exposed.contains(name), "{name} not exposed: {exposed}");
        }
    }

    #[test]
    fn http_date_is_imf_fixdate() {
        let at = chrono::DateTime::from_timestamp(784_111_777, 0).unwrap();
        assert_eq!(http_date(at), "Sun, 06 Nov 1994 08:49:37 GMT");
    }
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
            assert_eq!(
                response.headers()[axum::http::header::CACHE_CONTROL],
                "no-cache"
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
            assert_eq!(
                response.headers()[axum::http::header::CACHE_CONTROL],
                "public, max-age=31536000, immutable"
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
        assert!(
            response
                .headers()
                .get(axum::http::header::CACHE_CONTROL)
                .is_none(),
            "a missing chunk must not be cached"
        );
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
