use std::collections::VecDeque;
use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{
    IntoResponse, Response, Sse,
    sse::{Event, KeepAlive},
};
use serde::Deserialize;
use tracing::instrument;
use utoipa::IntoParams;

use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{FreshAuthUser, bearer_claims};
use crate::extractors::path::AppPath;
use crate::services::notifications;
use crate::state::AppState;
use crate::visibility::{Action, Resource, Subject, VisibilityKernel};

#[derive(Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct NotificationQuery {
    pub plugin_id: String,
    /// Omit on first connection to start at the present, without historical alerts.
    #[param(value_type = Option<String>)]
    pub after: Option<i64>,
}

#[utoipa::path(
    get, path = "/{id}/notifications", tag = "Contests",
    operation_id = "streamContestNotifications",
    summary = "Stream the current user's plugin notifications",
    description = "SSE with a ready cursor followed by notification events. Reconnect with after to replay. Connections close at token expiry or after one minute for reauthorization.",
    params(("id" = i32, Path, description = "Contest ID"), NotificationQuery),
    responses(
        (status = 200, description = "SSE ready and notification events", body = String, content_type = "text/event-stream"),
        (status = 400, description = "Invalid cursor or plugin", body = ErrorBody),
        (status = 401, description = "Authentication required", body = ErrorBody),
        (status = 404, description = "Contest not found", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth, headers))]
pub async fn stream_notifications(
    auth: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(contest_id): AppPath<i32>,
    Query(query): Query<NotificationQuery>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if query.plugin_id.is_empty()
        || query.plugin_id.len() > 128
        || query.after.is_some_and(|n| n < 0)
    {
        return Err(AppError::Validation(
            "Invalid notification plugin or cursor".into(),
        ));
    }
    let kernel = VisibilityKernel::new(&state, Subject::from_auth_user(&auth));
    if kernel
        .decide(Action::Read, Resource::Contest(contest_id))
        .await?
        .is_denied()
    {
        return Err(AppError::NotFound("Contest not found".into()));
    }
    let claims = bearer_claims(&headers, &state.config.auth.jwt_secret)?;
    let remaining = (claims.exp as i64 - chrono::Utc::now().timestamp()).clamp(0, 60) as u64;
    if remaining == 0 {
        return Err(AppError::TokenInvalid);
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(remaining);
    let latest =
        notifications::watermark(&state.db, contest_id, auth.user_id, &query.plugin_id).await?;
    // A cursor from an older database may be ahead after a restore. A ready
    // event explicitly resets it rather than leaving that browser stuck forever.
    let cursor = query.after.unwrap_or(latest).min(latest);
    let ready = Event::default()
        .event("ready")
        .id(cursor.to_string())
        .data("{}");
    let events = futures::stream::unfold(
        (
            cursor,
            VecDeque::from([ready]),
            state.db,
            query.plugin_id,
            auth.user_id,
        ),
        move |(mut cursor, mut pending, db, plugin_id, user_id)| async move {
            loop {
                if tokio::time::Instant::now() >= until {
                    return None;
                }
                if let Some(event) = pending.pop_front() {
                    return Some((
                        Ok::<_, Infallible>(event),
                        (cursor, pending, db, plugin_id, user_id),
                    ));
                }
                let rows = match notifications::after(&db, contest_id, user_id, &plugin_id, cursor)
                    .await
                {
                    Ok(rows) => rows,
                    Err(error) => {
                        tracing::warn!(%error, contest_id, user_id, "Notification stream interrupted");
                        return None;
                    }
                };
                for row in rows {
                    cursor = row.id.parse().expect("database notification id");
                    pending.push_back(
                        Event::default()
                            .event("notification")
                            .id(&row.id)
                            .json_data(&row)
                            .expect("serializable notification"),
                    );
                }
                if pending.is_empty() {
                    // Poll on the SERVER, not in background browser timers. The
                    // durable log also works across replicas without an in-memory bus.
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
        },
    );
    let mut response = Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response();
    response
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    response.headers_mut().insert(
        "cache-control",
        HeaderValue::from_static("no-cache, no-transform"),
    );
    Ok(response)
}
