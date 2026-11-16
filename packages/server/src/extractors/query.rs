#![allow(clippy::disallowed_types)]

use axum::extract::rejection::QueryRejection;
use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::AppError;

/// A query string, deserialized into `T`. A query that does not fit `T` is
/// a 400 `VALIDATION_ERROR` naming the parameter, in the JSON error shape
/// every endpoint answers with.
pub struct AppQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for AppQuery<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(AppQuery(value)),
            Err(QueryRejection::FailedToDeserializeQueryString(e)) => Err(AppError::Validation(
                format!("Invalid query string: {}", e.body_text()),
            )),
            Err(other) => Err(AppError::Validation(other.body_text())),
        }
    }
}
