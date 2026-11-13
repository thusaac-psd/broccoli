//! Rejects a request whose path or query string carries a percent-encoded NUL
//! (`%00`) with a 400 `VALIDATION_ERROR`, before any handler binds it to a
//! query. Postgres stores no NUL in text, and no route takes one.

use axum::extract::Request;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

pub async fn reject_nul_in_uri(req: Request, next: Next) -> Response {
    let uri = req.uri();
    if uri.path().contains("%00") || uri.query().is_some_and(|q| q.contains("%00")) {
        return crate::utils::text::nul_rejected("The request URL").into_response();
    }
    next.run(req).await
}
