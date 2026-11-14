use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::CONTENT_LENGTH;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use utoipa_axum::router::OpenApiRouter;

use crate::error::AppError;
use crate::upload_limits::MIB;

/// The most bytes a route reads of a request body. Every body over it is
/// refused with 413 `PAYLOAD_TOO_LARGE` naming the limit: a declared
/// `Content-Length` before the handler runs, a body without one by the
/// extractor that reads past it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BodyLimit(pub usize);

/// axum's limit for a route that sets none.
pub const DEFAULT_BODY_LIMIT: BodyLimit = BodyLimit(2 * MIB);

impl BodyLimit {
    /// The limit a request was routed under.
    pub fn of(extensions: &axum::http::Extensions) -> Self {
        extensions.get().copied().unwrap_or(DEFAULT_BODY_LIMIT)
    }

    pub fn exceeded(self) -> AppError {
        let Self(limit) = self;
        let shown = if limit % MIB == 0 {
            format!("{} MiB", limit / MIB)
        } else {
            format!("{limit} bytes")
        };
        AppError::PayloadTooLarge(format!(
            "The request body is larger than this endpoint's {shown} limit"
        ))
    }
}

/// Limits the request bodies of a router's routes.
pub trait WithBodyLimit {
    /// Limit the request bodies of every route added so far to `bytes`.
    fn body_limit(self, bytes: usize) -> Self;
}

impl<S> WithBodyLimit for OpenApiRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    fn body_limit(self, bytes: usize) -> Self {
        self.layer(axum::middleware::from_fn_with_state(
            BodyLimit(bytes),
            refuse_declared_oversize,
        ))
        .layer(DefaultBodyLimit::max(bytes))
    }
}

async fn refuse_declared_oversize(
    State(limit): State<BodyLimit>,
    mut req: Request,
    next: Next,
) -> Response {
    let declared = req
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > limit.0 as u64) {
        return limit.exceeded().into_response();
    }
    req.extensions_mut().insert(limit);
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_limit() {
        let message = |limit| match BodyLimit(limit).exceeded() {
            AppError::PayloadTooLarge(m) => m,
            other => panic!("{other:?}"),
        };
        assert_eq!(
            message(5 * MIB),
            "The request body is larger than this endpoint's 5 MiB limit"
        );
        assert_eq!(
            message(16 * 1024 + 1),
            "The request body is larger than this endpoint's 16385 bytes limit"
        );
    }
}
