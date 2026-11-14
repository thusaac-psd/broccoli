mod v1;

#[cfg(feature = "bundled-stress-test")]
pub mod downloads;

use utoipa::openapi::OpenApi;
use utoipa_axum::router::OpenApiRouter;

use crate::config::{AppConfig, SubmissionConfig};
use crate::state::AppState;

/// The settings the route tree is built from. They shape request limits, not
/// the documented API, so the OpenAPI document is built from the defaults.
#[derive(Debug, Clone, Copy)]
pub struct RouteSettings {
    pub submission_max_size: usize,
}

impl RouteSettings {
    pub fn of(config: &AppConfig) -> Self {
        Self {
            submission_max_size: config.submission.max_size,
        }
    }

    /// The defaults, for building the OpenAPI document without configuration.
    pub fn documented() -> Self {
        Self {
            submission_max_size: SubmissionConfig::default().max_size,
        }
    }
}

pub fn api_routes(
    settings: RouteSettings,
    base_openapi: OpenApi,
) -> (axum::Router<AppState>, OpenApi) {
    let (router, api) = OpenApiRouter::with_openapi(base_openapi)
        .merge(v1::routes(settings))
        .split_for_parts();
    (axum::Router::new().nest("/v1", router), api)
}
