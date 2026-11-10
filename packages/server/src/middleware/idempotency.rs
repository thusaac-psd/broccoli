//! `Idempotency-Key` support for authenticated POSTs.
//!
//! A key is bound to the request that first claimed it: method, path and a
//! SHA-256 fingerprint of the body. Reusing the key for a different request
//! answers 422 `IDEMPOTENCY_KEY_MISMATCH`; retrying the same request replays
//! the stored 2xx response (or 409 when the response was deliberately not
//! stored, see [`NoStore`]).
//!
//! Only outcomes that prove nothing was committed free the key: a 4xx (other
//! than 408) releases it, so a corrected request can reuse it. A 5xx, a 408
//! timeout or a request cancelled mid-handler may have committed, so the key
//! is settled as `outcome_unknown` and retries answer 409
//! `IDEMPOTENCY_OUTCOME_UNKNOWN` until the key expires; the client must check
//! what happened and use a fresh key.
//!
//! Only JSON-sized bodies are covered (see [`max_request_body_bytes`]); a keyed
//! request with a larger body is refused with 413 before it is read.
//!
//! Retries must resend a byte-identical body. JSON clients do this naturally;
//! a multipart client must reuse its boundary.
//!
//! Every timestamp comparison runs in SQL against the database clock, so
//! replicas with skewed clocks agree on when a pending claim went stale.

use axum::{
    body::{Body, Bytes, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use sea_orm::*;
use sha2::{Digest, Sha256};
use tracing::{error, warn};

use crate::entity::idempotency_key;
use crate::error::AppError;
use crate::state::AppState;
use crate::upload_limits::{MIB, source_request_limit_bytes};

const MAX_RESPONSE_BODY_SIZE: usize = 4 * 1_048_576;

/// axum's `DefaultBodyLimit`, which every JSON route without its own limit uses.
const DEFAULT_JSON_BODY_LIMIT: usize = 2 * MIB;

/// Upper bound on a request body buffered for fingerprinting: the largest JSON
/// body a keyed route accepts (submissions and code runs carry source files).
/// The body is buffered before the route's own limit applies, so this bound is
/// what keeps an authenticated client from making the server hold an upload in
/// memory. Uploads are not idempotent and must not send a key: a larger body
/// is refused with 413.
fn max_request_body_bytes(state: &AppState) -> usize {
    DEFAULT_JSON_BODY_LIMIT.max(source_request_limit_bytes(state.config.submission.max_size))
}

/// The declared `Content-Length`, when present and well-formed.
fn declared_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn body_too_large() -> AppError {
    AppError::PayloadTooLarge("Request body too large for an idempotent request".into())
}

const STALE_PENDING_SECS: i64 = 120;

/// Response marker for bodies that must never be persisted: wraps any
/// response and adds `Cache-Control: no-store`. The idempotency middleware
/// honours that standard directive (from this wrapper or any layer that sets
/// it) by recording only that the request completed, never the body, so a
/// replay answers 409 instead of re-sending the secret. It also keeps
/// browsers and intermediaries from caching it. Use it on every handler whose
/// response carries a credential (generated passwords, tokens).
pub struct NoStore<T>(pub T);

impl<T: IntoResponse> IntoResponse for NoStore<T> {
    fn into_response(self) -> Response {
        let mut response = self.0.into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

/// Whether the response forbids storing its body (`Cache-Control: no-store`).
fn forbids_storage(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::CACHE_CONTROL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|directive| directive.trim().eq_ignore_ascii_case("no-store"))
}

/// The `idempotency_key.status` column. SQL literals use the same strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyStatus {
    /// Claimed by an in-flight request.
    Pending,
    /// Finished; `response_body` holds the response to replay.
    Completed,
    /// Finished, but the body was over `MAX_RESPONSE_BODY_SIZE`.
    CompletedTooLarge,
    /// Finished, but the response forbade storing its body (`no-store`).
    CompletedNotStored,
    /// The handler ran but the outcome is unknown (5xx, 408, cancelled): it may
    /// have committed, so the key is never handed to a re-execution.
    OutcomeUnknown,
}

impl KeyStatus {
    const ALL: [KeyStatus; 5] = [
        KeyStatus::Pending,
        KeyStatus::Completed,
        KeyStatus::CompletedTooLarge,
        KeyStatus::CompletedNotStored,
        KeyStatus::OutcomeUnknown,
    ];

    fn as_str(self) -> &'static str {
        match self {
            KeyStatus::Pending => "pending",
            KeyStatus::Completed => "completed",
            KeyStatus::CompletedTooLarge => "completed_too_large",
            KeyStatus::CompletedNotStored => "completed_not_stored",
            KeyStatus::OutcomeUnknown => "outcome_unknown",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|status| status.as_str() == s)
    }
}

/// What a key is bound to. Two requests are "the same" iff all three match.
struct RequestIdentity {
    method: String,
    path: String,
    fingerprint: String,
}

impl RequestIdentity {
    /// Describe how `existing` differs from this request, if it does. Rows
    /// stored before fingerprints existed (`None`) match on method and path.
    fn mismatch(&self, existing: &idempotency_key::Model) -> Option<String> {
        if existing.request_path != self.path || existing.request_method != self.method {
            return Some(format!(
                "This key was used with {} {} but this request is {} {}",
                existing.request_method, existing.request_path, self.method, self.path
            ));
        }
        if existing
            .request_fingerprint
            .as_deref()
            .is_some_and(|fp| fp != self.fingerprint)
        {
            return Some("This key was used with a different request body".into());
        }
        None
    }
}

pub async fn idempotency_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }

    let key = match request.headers().get("idempotency-key") {
        Some(v) => match v.to_str() {
            Ok(s) => s.to_string(),
            Err(_) => {
                return AppError::Validation("Idempotency-Key header is not valid UTF-8".into())
                    .into_response();
            }
        },
        None => return next.run(request).await,
    };

    if key.is_empty() || key.len() > 255 {
        return AppError::Validation("Idempotency-Key must be 1-255 characters".into())
            .into_response();
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return AppError::Validation(
            "Idempotency-Key must contain only alphanumeric characters, hyphens, and underscores"
                .into(),
        )
        .into_response();
    }

    let user_id = match extract_user_id(&request, &state) {
        Some(id) => id,
        None => return next.run(request).await,
    };

    let limit = max_request_body_bytes(&state);
    if declared_length(request.headers()).is_some_and(|len| len > limit as u64) {
        return body_too_large().into_response();
    }

    let (parts, body) = request.into_parts();
    // Buffering now happens before the handler's own timeout starts, so bound
    // it by the same budget: a trickling client cannot hold the claim open.
    let buffered =
        tokio::time::timeout(crate::REQUEST_TIMEOUT, buffer_and_fingerprint(body, limit)).await;
    let (body, fingerprint) = match buffered {
        Ok(Ok(buffered)) => buffered,
        Ok(Err(e)) => return e.into_response(),
        Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
    };
    let identity = RequestIdentity {
        method: parts.method.to_string(),
        path: parts.uri.path().to_string(),
        fingerprint,
    };
    let request = Request::from_parts(parts, Body::from(body));

    match try_claim_key(&state.db, &key, user_id, &identity).await {
        Ok(ClaimResult::Claimed(claim)) => {
            let response = next.run(request).await;
            claim.complete(response).await
        }
        Ok(ClaimResult::AlreadyExists(existing)) => {
            if let Some(msg) = identity.mismatch(&existing) {
                return AppError::IdempotencyKeyMismatch(msg).into_response();
            }
            replay_existing(existing, &key, request, next).await
        }
        Err(e) => {
            warn!(error = %e, key = %key, "Idempotency middleware DB error, passing through");
            next.run(request).await
        }
    }
}

fn extract_user_id(request: &Request<Body>, state: &AppState) -> Option<i32> {
    let auth_header = request.headers().get("Authorization")?.to_str().ok()?;
    let token = auth_header.strip_prefix("Bearer ")?;
    let claims = crate::utils::jwt::verify(token, &state.config.auth.jwt_secret).ok()?;
    Some(claims.uid)
}

/// Buffer the request body, hashing each frame as it arrives (one pass).
async fn buffer_and_fingerprint(body: Body, limit: usize) -> Result<(Bytes, String), AppError> {
    let mut hasher = Sha256::new();
    let mut buffered = Vec::new();
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|e| AppError::Validation(format!("Failed to read request body: {e}")))?;
        if buffered.len() + chunk.len() > limit {
            return Err(body_too_large());
        }
        hasher.update(&chunk);
        buffered.extend_from_slice(&chunk);
    }
    Ok((Bytes::from(buffered), hex::encode(hasher.finalize())))
}

enum ClaimResult {
    Claimed(PendingClaim),
    AlreadyExists(idempotency_key::Model),
}

/// Claim the key: insert it, or take over a `pending` row that went stale
/// (its request died without settling it). Both happen in one statement
/// judged by the database clock.
async fn try_claim_key(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    identity: &RequestIdentity,
) -> Result<ClaimResult, DbErr> {
    let claimed = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO idempotency_key
                   (key, user_id, request_path, request_method, request_fingerprint, status, created_at)
               VALUES ($1, $2, $3, $4, $5, 'pending', NOW())
               ON CONFLICT (key, user_id) DO UPDATE
                   SET request_path = EXCLUDED.request_path,
                       request_method = EXCLUDED.request_method,
                       request_fingerprint = EXCLUDED.request_fingerprint,
                       created_at = NOW(), completed_at = NULL,
                       response_status = NULL, response_body = NULL
                   WHERE idempotency_key.status = 'pending'
                     AND idempotency_key.created_at < NOW() - make_interval(secs => $6)
               RETURNING created_at, (xmax <> 0) AS reclaimed"#,
            [
                key.into(),
                user_id.into(),
                identity.path.as_str().into(),
                identity.method.as_str().into(),
                identity.fingerprint.as_str().into(),
                (STALE_PENDING_SECS as f64).into(),
            ],
        ))
        .await?;

    if let Some(row) = claimed {
        if row.try_get::<bool>("", "reclaimed")? {
            warn!(key = %key, "Reclaimed stale pending idempotency key");
        }
        return Ok(ClaimResult::Claimed(PendingClaim {
            db: db.clone(),
            key: key.to_string(),
            user_id,
            epoch: row.try_get("", "created_at")?,
            settled: false,
        }));
    }

    let existing = idempotency_key::Entity::find_by_id((key.to_string(), user_id))
        .one(db)
        .await?
        .ok_or_else(|| {
            DbErr::Custom("Idempotency key vanished between INSERT and SELECT".into())
        })?;

    Ok(ClaimResult::AlreadyExists(existing))
}

/// Answer a retry of a request that already holds the key.
async fn replay_existing(
    existing: idempotency_key::Model,
    key: &str,
    request: Request<Body>,
    next: Next,
) -> Response {
    match KeyStatus::parse(&existing.status) {
        Some(KeyStatus::Completed) => {
            let status = existing
                .response_status
                .and_then(|s| StatusCode::from_u16(s as u16).ok())
                .unwrap_or(StatusCode::OK);
            let body = existing.response_body.unwrap_or_default();

            Response::builder()
                .status(status)
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Some(KeyStatus::CompletedTooLarge) => AppError::IdempotencyResponseTooLarge.into_response(),
        Some(KeyStatus::CompletedNotStored) => {
            AppError::IdempotencyResponseNotStored.into_response()
        }
        Some(KeyStatus::OutcomeUnknown) => AppError::IdempotencyOutcomeUnknown.into_response(),
        // Not stale, or `try_claim_key` would have reclaimed it.
        Some(KeyStatus::Pending) => AppError::IdempotencyKeyInProgress.into_response(),
        None => {
            warn!(key = %key, status = %existing.status, "Unknown idempotency key status");
            next.run(request).await
        }
    }
}

/// How a claimed key is settled once the handler has produced `status`.
fn settlement_for(status: StatusCode) -> Settlement {
    if status.is_success() {
        Settlement::Store
    } else if status == StatusCode::REQUEST_TIMEOUT || status.is_server_error() {
        Settlement::Unknown
    } else {
        Settlement::Release
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Settlement {
    /// The request succeeded: keep (or note) its response for replays.
    Store,
    /// The request was refused before committing anything: free the key.
    Release,
    /// The request may or may not have committed.
    Unknown,
}

/// A `pending` claim owned by the in-flight request. Every write is fenced on
/// `epoch` (the claim's `created_at`), so a request whose stale claim was
/// reclaimed by a retry can no longer settle or delete the retry's row.
///
/// If the request future is dropped before the claim settles (the client
/// disconnected, or a layer cancelled it), `Drop` settles the key as
/// `outcome_unknown` rather than leaving it `pending` until the stale window
/// lets the request run again.
struct PendingClaim {
    db: DatabaseConnection,
    key: String,
    user_id: i32,
    epoch: DateTime<Utc>,
    settled: bool,
}

impl PendingClaim {
    /// Settle the key for the handler's response and hand the response back.
    async fn complete(mut self, response: Response) -> Response {
        let status = response.status();
        let response = match settlement_for(status) {
            Settlement::Release => {
                if let Err(e) = self.release().await {
                    warn!(error = %e, "Failed to delete idempotency key after error response");
                }
                response
            }
            Settlement::Unknown => {
                self.mark(KeyStatus::OutcomeUnknown, Some(status), None)
                    .await;
                response
            }
            // The body carries a secret: record completion (so a retry is not
            // executed twice) but never the body itself.
            Settlement::Store if forbids_storage(response.headers()) => {
                self.mark(KeyStatus::CompletedNotStored, Some(status), None)
                    .await;
                response
            }
            Settlement::Store => self.store(response).await,
        };
        self.settled = true;
        response
    }

    async fn store(&self, response: Response) -> Response {
        let status = response.status();
        let (parts, body) = response.into_parts();
        match to_bytes(body, usize::MAX).await {
            Ok(bytes) if bytes.len() > MAX_RESPONSE_BODY_SIZE => {
                // Too large to cache. Pass the FULL body through to the
                // current client, then mark the key so retries get 409.
                warn!(
                    body_len = bytes.len(),
                    cap = MAX_RESPONSE_BODY_SIZE,
                    "Response body too large to cache for idempotency; passing original body through, retries will be rejected"
                );
                self.mark(KeyStatus::CompletedTooLarge, Some(status), None)
                    .await;
                Response::from_parts(parts, Body::from(bytes))
            }
            Ok(bytes) => {
                match std::str::from_utf8(&bytes) {
                    Ok(body_str) => {
                        self.mark(KeyStatus::Completed, Some(status), Some(body_str))
                            .await
                    }
                    Err(_) => {
                        warn!("Response body is not valid UTF-8, not storing it for replay");
                        self.mark(KeyStatus::CompletedNotStored, Some(status), None)
                            .await
                    }
                }
                Response::from_parts(parts, Body::from(bytes))
            }
            Err(e) => {
                error!(error = %e, "Failed to read response body for idempotency cache");
                self.mark(KeyStatus::OutcomeUnknown, Some(status), None)
                    .await;
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }

    async fn mark(
        &self,
        new_status: KeyStatus,
        response_status: Option<StatusCode>,
        response_body: Option<&str>,
    ) {
        if let Err(e) = settle_claim(
            &self.db,
            &self.key,
            self.user_id,
            self.epoch,
            new_status,
            response_status,
            response_body,
        )
        .await
        {
            error!(error = %e, status = new_status.as_str(), "Failed to settle idempotency key");
        }
    }

    async fn release(&self) -> Result<(), DbErr> {
        release_claim(&self.db, &self.key, self.user_id, self.epoch).await
    }
}

impl Drop for PendingClaim {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (db, key, user_id, epoch) =
            (self.db.clone(), self.key.clone(), self.user_id, self.epoch);
        handle.spawn(async move {
            let unknown = KeyStatus::OutcomeUnknown;
            if let Err(e) = settle_claim(&db, &key, user_id, epoch, unknown, None, None).await {
                warn!(error = %e, key = %key, "Failed to settle abandoned idempotency key");
            }
        });
    }
}

async fn settle_claim(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    epoch: DateTime<Utc>,
    new_status: KeyStatus,
    response_status: Option<StatusCode>,
    response_body: Option<&str>,
) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"UPDATE idempotency_key
           SET status = $1, response_status = $2, response_body = $3, completed_at = NOW()
           WHERE key = $4 AND user_id = $5 AND status = 'pending' AND created_at = $6"#,
        [
            new_status.as_str().into(),
            response_status.map(|s| s.as_u16() as i16).into(),
            response_body.into(),
            key.into(),
            user_id.into(),
            epoch.into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn release_claim(
    db: &DatabaseConnection,
    key: &str,
    user_id: i32,
    epoch: DateTime<Utc>,
) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"DELETE FROM idempotency_key
           WHERE key = $1 AND user_id = $2 AND status = 'pending' AND created_at = $3"#,
        [key.into(), user_id.into(), epoch.into()],
    ))
    .await?;
    Ok(())
}

pub async fn cleanup_expired_keys(db: &DatabaseConnection) {
    match db
        .execute_unprepared(
            "DELETE FROM idempotency_key WHERE created_at < NOW() - INTERVAL '24 hours'",
        )
        .await
    {
        Ok(result) => {
            let deleted = result.rows_affected();
            if deleted > 0 {
                tracing::info!(deleted, "Cleaned up expired idempotency keys");
            }
        }
        Err(e) => {
            error!(error = %e, "Failed to cleanup expired idempotency keys");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_status_round_trips_through_its_column_value() {
        for status in KeyStatus::ALL {
            assert_eq!(KeyStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(KeyStatus::parse("bogus"), None);
    }

    #[tokio::test]
    async fn streamed_body_over_the_bound_is_refused_mid_stream() {
        let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
            Ok(Bytes::from(vec![b'a'; 6])),
            Ok(Bytes::from(vec![b'b'; 6])),
        ];
        let body = Body::from_stream(futures::stream::iter(chunks));
        assert!(matches!(
            buffer_and_fingerprint(body, 10).await,
            Err(AppError::PayloadTooLarge(_))
        ));

        let (bytes, fingerprint) = buffer_and_fingerprint(Body::from("abc"), 10).await.unwrap();
        assert_eq!(&bytes[..], b"abc");
        assert_eq!(fingerprint, hex::encode(Sha256::digest(b"abc")));
    }

    #[test]
    fn only_outcomes_that_prove_nothing_committed_release_the_key() {
        assert_eq!(settlement_for(StatusCode::CREATED), Settlement::Store);
        assert_eq!(settlement_for(StatusCode::BAD_REQUEST), Settlement::Release);
        assert_eq!(settlement_for(StatusCode::CONFLICT), Settlement::Release);
        assert_eq!(
            settlement_for(StatusCode::REQUEST_TIMEOUT),
            Settlement::Unknown
        );
        assert_eq!(
            settlement_for(StatusCode::INTERNAL_SERVER_ERROR),
            Settlement::Unknown
        );
        assert_eq!(settlement_for(StatusCode::BAD_GATEWAY), Settlement::Unknown);
    }
}
