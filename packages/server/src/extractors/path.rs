#![allow(clippy::disallowed_types)]

use axum::extract::rejection::PathRejection;
use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::AppError;

/// A `Path<T>` wrapper that converts deserialization/parse errors into
/// `AppError::Validation`, ensuring clients always receive structured JSON
/// error responses (instead of axum's default `text/plain` body).
pub struct AppPath<T>(pub T);

impl<S, T> FromRequestParts<S> for AppPath<T>
where
    T: DeserializeOwned + Send,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Path::<T>::from_request_parts(parts, state).await {
            Ok(Path(value)) => Ok(AppPath(value)),
            Err(rejection) => Err(map_path_rejection(rejection)),
        }
    }
}

fn map_path_rejection(rejection: PathRejection) -> AppError {
    match rejection {
        // The deserialize error variant is the common case ("notanumber" -> i32).
        PathRejection::FailedToDeserializePathParams(err) => {
            AppError::Validation(format!("Invalid path parameter: {}", err.body_text()))
        }
        PathRejection::MissingPathParams(_) => AppError::Internal("Missing path parameters".into()),
        other => AppError::Validation(other.body_text()),
    }
}
