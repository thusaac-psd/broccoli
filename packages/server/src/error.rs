use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use axum_typed_multipart::TypedMultipartError;
pub use broccoli_api_types::error::{ErrorBody, HostCode};
use common::storage::StorageError;
use plugin_core::error::{AssetError, PluginError};
use sea_orm::DbErr;

/// `Retry-After` for [`AppError::ContestTypeUnavailable`]: about one plugin
/// reconcile interval, after which this replica may have loaded the plugin.
const CONTEST_TYPE_RETRY_AFTER_SECS: u64 = 5;

/// `Retry-After` for [`AppError::PluginUnavailable`]: a plugin call that
/// could not run in time has nothing to wait for but load to drop, which a
/// couple of seconds usually sees.
pub const PLUGIN_RETRY_AFTER_SECS: u64 = 2;

/// The `Retry-After` of a `STORAGE_UNAVAILABLE` answer.
pub const STORAGE_RETRY_AFTER_SECS: u64 = 5;

/// Why the host refused a submission its contest plugin admitted. Each is a
/// 409: the client refreshes what it shows and sends again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionRefused {
    /// The admission's submission slot changed or holds another submission.
    SlotInFlight,
    /// The plugin storage the admission was checked against changed.
    StateChanged,
    /// The admission's arrival window closed before the insert.
    TooLate,
}

impl AdmissionRefused {
    fn code_and_message(self) -> (HostCode, &'static str) {
        match self {
            Self::SlotInFlight => (
                HostCode::SubmissionInFlight,
                "Submission slot changed; refresh and retry.",
            ),
            Self::StateChanged => (
                HostCode::SubmissionStateChanged,
                "Admission state changed; refresh and retry.",
            ),
            Self::TooLate => (
                HostCode::SubmissionTooLate,
                "Submission admission window has closed.",
            ),
        }
    }
}

/// The `Retry-After` of a `DATABASE_UNAVAILABLE` answer. While the database
/// is unreachable a request is refused at once and one probe at a time looks
/// for it, so a client that retries this often costs next to nothing and is
/// served again soon after a probe finds the database back.
pub const DATABASE_RETRY_AFTER_SECS: u64 = 5;

/// The `Retry-After` of a `QUEUE_UNAVAILABLE` answer: about one Redis
/// command timeout, after which a queue that came back serves again.
pub const QUEUE_RETRY_AFTER_SECS: u64 = common::redis_conn::COMMAND_TIMEOUT.as_secs();

#[derive(Debug)]
pub enum AppError {
    Validation(String),
    /// A plugin config that breaks its namespace's schema, with each broken
    /// field.
    InvalidConfig(Vec<plugin_core::config_schema::FieldError>),
    /// The plugin is not installed, declares no such config namespace, or
    /// does not allow it at the requested scope.
    ConfigNamespaceNotFound(String),
    /// A change to a problem's test data that would leave these stored
    /// configs with something wrong their plugins report; nothing was changed.
    ConfigWouldBreak(Vec<broccoli_api_types::plugin::IncompatibleConfig>),
    PayloadTooLarge(String),
    TokenMissing,
    TokenInvalid,
    InvalidCredentials,
    PermissionDenied,
    /// The caller may see a public contest but has not registered for it, so
    /// may not act in it (submit, run code) until they do.
    ContestNotRegistered,
    NotFound(String),
    MethodNotAllowed,
    Conflict(String),
    /// The usernames an active account already holds (case-insensitively).
    /// Empty when the database refused a write without saying which.
    UsernameTaken(Vec<String>),
    /// `POST /auth/register` while self-registration is closed.
    RegistrationClosed,
    /// An upload meant to add a new plugin, but one with its id exists.
    PluginExists(String),
    /// Enabling a language pack for a locale another enabled pack provides.
    LanguagePackTaken {
        locale: String,
        plugin_id: String,
    },
    PluginNotReady(String),
    /// A valid plugin archive whose plugin failed to load or initialize on
    /// the serving replica; nothing was published.
    PluginLoadFailed(String),
    /// Plugin uploads are switched off on this server
    /// (`plugin.uploads_enabled`).
    PluginUploadDisabled,
    RateLimited {
        retry_after: u64,
    },
    /// The serving replica has no handler for this contest type right now
    /// (its plugin is still loading, or failed to load, here). Retryable: the
    /// load balancer will likely route the retry to a replica that has it.
    ContestTypeUnavailable(String),
    /// The contest already has a submission, so its contest type is fixed.
    ContestTypeLocked,
    /// The plugin providing the problem's checker format declares no checker
    /// source, so the problem `id` has none to set.
    CheckerSourceUnsupported(i32),
    /// The plugin providing this contest type is disabled fleet-wide. Not
    /// retryable: nothing changes until an admin enables it again.
    ContestTypeDisabled(String),
    /// Server is overloaded; client should retry after the given duration.
    /// Distinct from `RateLimited` (429): 429 means "your client is too fast",
    /// 503 means "the system as a whole is past capacity". Used for
    /// durable-queue depth backpressure.
    Overloaded {
        retry_after: u64,
    },
    /// A plugin the request needs could not answer before the request's
    /// deadline: this server is too busy right now. Never a decision about
    /// the request; the client retries after [`PLUGIN_RETRY_AFTER_SECS`].
    PluginUnavailable,
    /// What the request needs is provided by plugin `id`, which an
    /// administrator disabled. Not retryable: nothing changes until it is
    /// enabled again.
    PluginDisabled(String),
    /// Blob storage did not answer through its retries: an outage, retried
    /// by the client after [`STORAGE_RETRY_AFTER_SECS`].
    StorageUnavailable,
    /// A plugin refused the request (a route's client error or a hook's
    /// rejection), confined to the plugin's namespace.
    PluginRefusal(crate::services::plugin_output::PluginRefusal),
    /// The host refused a submission its contest plugin admitted: what the
    /// admission was checked against changed before the insert.
    AdmissionRefused(AdmissionRefused),
    /// The database cannot be reached or cannot serve connections
    /// ([`common::db::is_outage`]): an outage, retried by the client after
    /// [`DATABASE_RETRY_AFTER_SECS`].
    DatabaseUnavailable,
    /// The message queue (Redis) cannot be reached
    /// ([`common::redis_conn::is_outage`]): an outage, retried by the client
    /// after [`QUEUE_RETRY_AFTER_SECS`].
    QueueUnavailable,
    IdempotencyKeyInProgress,
    IdempotencyKeyMismatch(String),
    /// Idempotent response was too large to cache and cannot be replayed.
    IdempotencyResponseTooLarge,
    /// Idempotent response carried credentials (`Cache-Control: no-store`),
    /// so only its completion was recorded and it cannot be replayed.
    IdempotencyResponseNotStored,
    /// The original request's handler ran but failed or was cancelled, so it
    /// may have committed; the key is never reused for a re-execution.
    IdempotencyOutcomeUnknown,
    Internal(String),
}

impl AppError {
    fn status_and_body(self) -> (StatusCode, ErrorBody) {
        let simple = |code: HostCode, message: String| ErrorBody {
            code: code.as_str().into(),
            message,
            details: None,
        };

        match self {
            AppError::Validation(msg) => (
                StatusCode::BAD_REQUEST,
                simple(HostCode::ValidationError, msg),
            ),
            AppError::InvalidConfig(fields) => (
                StatusCode::BAD_REQUEST,
                ErrorBody {
                    code: HostCode::ValidationError.as_str().into(),
                    message: format!(
                        "Config refused: {}",
                        plugin_core::config_schema::describe(&fields)
                    ),
                    details: Some(serde_json::json!({ "fields": fields })),
                },
            ),
            AppError::ConfigNamespaceNotFound(msg) => (
                StatusCode::NOT_FOUND,
                simple(HostCode::ConfigNamespaceNotFound, msg),
            ),
            AppError::ConfigWouldBreak(issues) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: HostCode::ConfigWouldBreak.as_str().into(),
                    message: format!(
                        "The change would leave {} stored config(s) with something wrong their plugins report; \
                         send it again with force=true to make it anyway",
                        issues.len()
                    ),
                    details: Some(serde_json::json!({ "issues": issues })),
                },
            ),
            AppError::PayloadTooLarge(msg) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                simple(HostCode::PayloadTooLarge, msg),
            ),
            AppError::TokenMissing => (
                StatusCode::UNAUTHORIZED,
                simple(HostCode::TokenMissing, "Authentication required".into()),
            ),
            AppError::TokenInvalid => (
                StatusCode::UNAUTHORIZED,
                simple(HostCode::TokenInvalid, "Invalid or expired token".into()),
            ),
            AppError::InvalidCredentials => (
                StatusCode::UNAUTHORIZED,
                simple(
                    HostCode::InvalidCredentials,
                    "Invalid username or password".into(),
                ),
            ),
            AppError::PermissionDenied => (
                StatusCode::FORBIDDEN,
                simple(
                    HostCode::PermissionDenied,
                    "Insufficient permissions".into(),
                ),
            ),
            AppError::ContestNotRegistered => (
                StatusCode::FORBIDDEN,
                simple(
                    HostCode::ContestNotRegistered,
                    "Register for this contest first".into(),
                ),
            ),
            AppError::NotFound(msg) => (StatusCode::NOT_FOUND, simple(HostCode::NotFound, msg)),
            AppError::MethodNotAllowed => (
                StatusCode::METHOD_NOT_ALLOWED,
                simple(
                    HostCode::MethodNotAllowed,
                    "HTTP method not allowed for this endpoint".into(),
                ),
            ),
            AppError::Conflict(msg) => (StatusCode::CONFLICT, simple(HostCode::Conflict, msg)),
            AppError::RegistrationClosed => (
                StatusCode::FORBIDDEN,
                simple(
                    HostCode::RegistrationClosed,
                    "Self-registration is closed; ask the organisers for an account".into(),
                ),
            ),
            AppError::UsernameTaken(usernames) => {
                let message = match usernames.as_slice() {
                    [] => "Username is already taken".to_string(),
                    [one] => format!("Username '{one}' is already taken"),
                    many => format!("Usernames already taken: {}", many.join(", ")),
                };
                let details =
                    (!usernames.is_empty()).then(|| serde_json::json!({ "usernames": usernames }));
                (
                    StatusCode::CONFLICT,
                    ErrorBody {
                        code: HostCode::UsernameTaken.as_str().into(),
                        message,
                        details,
                    },
                )
            }
            AppError::LanguagePackTaken { locale, plugin_id } => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: HostCode::LanguagePackTaken.as_str().into(),
                    message: format!(
                        "The language pack '{plugin_id}' already provides '{locale}'; disable it first"
                    ),
                    details: Some(serde_json::json!({ "locale": locale, "plugin_id": plugin_id })),
                },
            ),
            AppError::PluginExists(id) => (
                StatusCode::CONFLICT,
                ErrorBody {
                    code: HostCode::PluginExists.as_str().into(),
                    message: format!(
                        "Plugin '{id}' already exists; upload it as a replacement instead"
                    ),
                    details: Some(serde_json::json!({ "plugin_id": id })),
                },
            ),
            AppError::PluginNotReady(msg) => (
                StatusCode::BAD_REQUEST,
                simple(HostCode::PluginNotReady, msg),
            ),
            AppError::PluginLoadFailed(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(HostCode::PluginLoadFailed, msg),
            ),
            AppError::PluginUploadDisabled => (
                StatusCode::FORBIDDEN,
                simple(
                    HostCode::PluginUploadDisabled,
                    "Plugin uploads are disabled on this server".into(),
                ),
            ),
            AppError::RateLimited { retry_after } => (
                StatusCode::TOO_MANY_REQUESTS,
                simple(
                    HostCode::RateLimited,
                    format!("Rate limit exceeded. Try again in {} seconds", retry_after),
                ),
            ),
            AppError::CheckerSourceUnsupported(id) => (
                StatusCode::CONFLICT,
                simple(
                    HostCode::CheckerSourceUnsupported,
                    format!("Problem {id}'s checker format takes no checker source"),
                ),
            ),
            AppError::ContestTypeLocked => (
                StatusCode::CONFLICT,
                simple(
                    HostCode::ContestTypeLocked,
                    "The contest type cannot change once the contest has a submission".into(),
                ),
            ),
            AppError::ContestTypeUnavailable(contest_type) => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::ContestTypeUnavailable,
                    format!(
                        "This server cannot accept '{contest_type}' submissions right now; \
                         retry in {CONTEST_TYPE_RETRY_AFTER_SECS} seconds"
                    ),
                ),
            ),
            AppError::ContestTypeDisabled(contest_type) => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::ContestTypeDisabled,
                    format!(
                        "'{contest_type}' submissions are closed: the plugin that judges them \
                         is disabled"
                    ),
                ),
            ),
            AppError::Overloaded { retry_after } => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::QueueOverloaded,
                    format!(
                        "Server is temporarily overloaded; retry in {} seconds",
                        retry_after
                    ),
                ),
            ),
            AppError::PluginUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::PluginUnavailable,
                    format!(
                        "A plugin this request needs is busy; retry in \
                         {PLUGIN_RETRY_AFTER_SECS} seconds"
                    ),
                ),
            ),
            AppError::PluginDisabled(plugin_id) => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::PluginDisabled,
                    format!(
                        "The plugin '{plugin_id}' is disabled, so what it provides is \
                         unavailable until an administrator enables it"
                    ),
                ),
            ),
            AppError::StorageUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::StorageUnavailable,
                    format!(
                        "File storage is not answering; retry in {STORAGE_RETRY_AFTER_SECS} seconds"
                    ),
                ),
            ),
            AppError::DatabaseUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::DatabaseUnavailable,
                    format!(
                        "The server's database is not answering; retry in \
                         {DATABASE_RETRY_AFTER_SECS} seconds"
                    ),
                ),
            ),
            AppError::QueueUnavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                simple(
                    HostCode::QueueUnavailable,
                    format!(
                        "The server's message queue is not answering; retry in \
                         {QUEUE_RETRY_AFTER_SECS} seconds"
                    ),
                ),
            ),
            AppError::PluginRefusal(refusal) => refusal.into_parts(),
            AppError::AdmissionRefused(refused) => {
                let (code, message) = refused.code_and_message();
                (StatusCode::CONFLICT, simple(code, message.into()))
            }
            AppError::IdempotencyKeyInProgress => (
                StatusCode::CONFLICT,
                simple(
                    HostCode::IdempotencyKeyInProgress,
                    "A request with this idempotency key is already being processed".into(),
                ),
            ),
            AppError::IdempotencyKeyMismatch(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                simple(HostCode::IdempotencyKeyMismatch, msg),
            ),
            AppError::IdempotencyResponseTooLarge => (
                StatusCode::CONFLICT,
                simple(
                    HostCode::IdempotencyResponseTooLarge,
                    "Idempotent response too large to replay; original request \
                     succeeded but cannot be safely re-issued"
                        .into(),
                ),
            ),
            AppError::IdempotencyResponseNotStored => (
                StatusCode::CONFLICT,
                simple(
                    HostCode::IdempotencyResponseNotStored,
                    "Idempotent response carries credentials and is never stored; \
                     original request succeeded but cannot be replayed"
                        .into(),
                ),
            ),
            AppError::IdempotencyOutcomeUnknown => (
                StatusCode::CONFLICT,
                simple(
                    HostCode::IdempotencyOutcomeUnknown,
                    "The original request with this idempotency key failed after it started \
                     and may have taken effect; check its result and retry with a new key"
                        .into(),
                ),
            ),
            AppError::Internal(detail) => {
                tracing::error!("Internal error: {}", detail);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    simple(
                        HostCode::InternalError,
                        "An unexpected error occurred".into(),
                    ),
                )
            }
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // Both 429 (RateLimited) and 503 (Overloaded) carry a Retry-After
        // hint. The header semantics are identical (RFC 7231 §7.1.3); only
        // the status code differs in what's "tired" - the client (429) vs
        // the service as a whole (503).
        let retry_after = match &self {
            AppError::RateLimited { retry_after } | AppError::Overloaded { retry_after } => {
                Some(*retry_after)
            }
            AppError::ContestTypeUnavailable(_) => Some(CONTEST_TYPE_RETRY_AFTER_SECS),
            AppError::PluginUnavailable => Some(PLUGIN_RETRY_AFTER_SECS),
            AppError::StorageUnavailable => Some(STORAGE_RETRY_AFTER_SECS),
            AppError::DatabaseUnavailable => Some(DATABASE_RETRY_AFTER_SECS),
            AppError::QueueUnavailable => Some(QUEUE_RETRY_AFTER_SECS),
            _ => None,
        };

        let (status, body) = self.status_and_body();

        if let Some(seconds) = retry_after {
            (status, [("Retry-After", seconds.to_string())], Json(body)).into_response()
        } else {
            (status, Json(body)).into_response()
        }
    }
}

impl From<DbErr> for AppError {
    fn from(err: DbErr) -> Self {
        if common::db::is_outage(&err) {
            tracing::warn!("Database unavailable: {err}");
            AppError::DatabaseUnavailable
        } else {
            AppError::Internal(err.to_string())
        }
    }
}

impl From<redis::RedisError> for AppError {
    fn from(err: redis::RedisError) -> Self {
        if common::redis_conn::is_outage(&err) {
            tracing::warn!("Message queue unavailable: {err}");
            AppError::QueueUnavailable
        } else {
            AppError::Internal(format!("Redis error: {err}"))
        }
    }
}

impl From<StorageError> for AppError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::NotFound(_) => AppError::NotFound(err.to_string()),
            StorageError::SizeLimitExceeded { .. } => AppError::Validation(err.to_string()),
            StorageError::InvalidHash(_) => AppError::Validation(err.to_string()),
            StorageError::Io(_) => AppError::Internal(err.to_string()),
            StorageError::Backend(_) => AppError::Internal(err.to_string()),
            StorageError::Unavailable(_) => {
                tracing::warn!("{err}");
                AppError::StorageUnavailable
            }
        }
    }
}

impl From<PluginError> for AppError {
    fn from(err: PluginError) -> Self {
        match err {
            PluginError::NotFound(detail) => {
                tracing::warn!("Plugin not found: {detail}");
                AppError::NotFound(format!("Plugin '{detail}' not found"))
            }
            PluginError::NotLoaded(detail) => {
                tracing::warn!("Plugin not loaded: {detail}");
                AppError::NotFound(format!("Plugin '{detail}' not found"))
            }
            PluginError::NoRuntime(_) => {
                tracing::warn!("Plugin not ready: {err}");
                AppError::PluginNotReady(err.to_string())
            }
            PluginError::Serialization(_) => AppError::Validation(err.to_string()),
            PluginError::Unavailable { .. }
            | PluginError::ShuttingDown
            | PluginError::InstanceBuild { .. } => {
                tracing::warn!("{err}");
                AppError::PluginUnavailable
            }
            _ => AppError::Internal(err.to_string()),
        }
    }
}

impl From<broccoli_api_types::visibility::PluginOutage> for AppError {
    fn from(outage: broccoli_api_types::visibility::PluginOutage) -> Self {
        use broccoli_api_types::visibility::PluginOutage;
        match outage {
            PluginOutage::Disabled { plugin_id } => AppError::PluginDisabled(plugin_id),
            PluginOutage::Unavailable => AppError::PluginUnavailable,
        }
    }
}

impl From<AssetError> for AppError {
    fn from(err: AssetError) -> Self {
        match err {
            AssetError::NotFound => AppError::NotFound("Asset not found".into()),
            AssetError::NoWebConfig => AppError::NotFound("Plugin does not have web assets".into()),
            AssetError::PathTraversal => AppError::PermissionDenied,
            AssetError::Io(_) | AssetError::Internal(_) => AppError::Internal(err.to_string()),
        }
    }
}

/// A multipart body that could not be read: over the route's body limit (413),
/// or malformed (400).
impl From<axum::extract::multipart::MultipartError> for AppError {
    fn from(err: axum::extract::multipart::MultipartError) -> Self {
        if err.status() == StatusCode::PAYLOAD_TOO_LARGE {
            AppError::PayloadTooLarge(
                "The request body is larger than this endpoint accepts".into(),
            )
        } else {
            AppError::Validation(format!("Malformed multipart body: {err}"))
        }
    }
}

impl From<TypedMultipartError> for AppError {
    fn from(err: TypedMultipartError) -> Self {
        match err {
            TypedMultipartError::MissingField { .. }
            | TypedMultipartError::WrongFieldType { .. }
            | TypedMultipartError::DuplicateField { .. }
            | TypedMultipartError::UnknownField { .. }
            | TypedMultipartError::InvalidEnumValue { .. }
            | TypedMultipartError::NamelessField => AppError::Validation(err.to_string()),
            TypedMultipartError::FieldTooLarge { .. } => AppError::PayloadTooLarge(err.to_string()),
            TypedMultipartError::InvalidRequestBody { source } => source.into(),
            TypedMultipartError::InvalidRequest { .. } => AppError::Validation(err.to_string()),
            _ => AppError::Internal(err.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn storage_that_does_not_answer_is_a_retryable_503() {
        let err = AppError::from(StorageError::Unavailable("refused".into()));
        let response = err.into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()["Retry-After"],
            STORAGE_RETRY_AFTER_SECS.to_string()
        );
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "STORAGE_UNAVAILABLE");
    }

    #[tokio::test]
    async fn a_queue_that_does_not_answer_is_a_retryable_503() {
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let response = AppError::from(redis::RedisError::from(refused)).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response.headers()["Retry-After"],
            QUEUE_RETRY_AFTER_SECS.to_string()
        );
        let body: serde_json::Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], "QUEUE_UNAVAILABLE");

        let reply = redis::RedisError::from((redis::ErrorKind::UnexpectedReturnType, "WRONGTYPE"));
        assert!(matches!(AppError::from(reply), AppError::Internal(_)));
    }

    /// Durable-queue backpressure must surface as
    /// `503 Service Unavailable` with code `QUEUE_OVERLOADED` and the
    /// `Retry-After` header populated from the variant payload. Re-tagging
    /// the variant or dropping the header branch fails here.
    /// The idempotency replay refusals are API contracts the web client
    /// branches on; each has its own code (all 409).
    #[tokio::test]
    async fn idempotency_replay_refusals_have_distinct_codes() {
        for (err, code) in [
            (
                AppError::IdempotencyKeyInProgress,
                "IDEMPOTENCY_KEY_IN_PROGRESS",
            ),
            (
                AppError::IdempotencyResponseTooLarge,
                "IDEMPOTENCY_RESPONSE_TOO_LARGE",
            ),
            (
                AppError::IdempotencyResponseNotStored,
                "IDEMPOTENCY_RESPONSE_NOT_STORED",
            ),
            (
                AppError::IdempotencyOutcomeUnknown,
                "IDEMPOTENCY_OUTCOME_UNKNOWN",
            ),
        ] {
            let response = err.into_response();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["code"], code);
        }
    }

    #[tokio::test]
    async fn overloaded_maps_to_503_with_retry_after() {
        let err = AppError::Overloaded { retry_after: 7 };
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .expect("Retry-After header"),
            "7"
        );

        let body_bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let body: serde_json::Value = serde_json::from_slice(&body_bytes).expect("json");
        assert_eq!(body["code"], "QUEUE_OVERLOADED");
    }

    #[tokio::test]
    async fn a_plugin_that_cannot_answer_now_is_a_503_with_retry_after() {
        for err in [
            PluginError::Unavailable {
                plugin_id: "p".into(),
                func_name: "f".into(),
            },
            PluginError::ShuttingDown,
            PluginError::InstanceBuild {
                plugin_id: "p".into(),
                message: "out of memory".into(),
            },
        ] {
            let response = AppError::from(err).into_response();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                response.headers().get("Retry-After").unwrap(),
                &PLUGIN_RETRY_AFTER_SECS.to_string()
            );
            let body = to_bytes(response.into_body(), 4096).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["code"], "PLUGIN_UNAVAILABLE");
        }
    }

    #[tokio::test]
    async fn a_disabled_plugin_is_a_503_naming_it_without_retry_after() {
        use broccoli_api_types::visibility::PluginOutage;
        let response = AppError::from(PluginOutage::Disabled {
            plugin_id: "ioi".into(),
        })
        .into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(response.headers().get("Retry-After").is_none());
        let body = to_bytes(response.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["code"], "PLUGIN_DISABLED");
        assert!(
            body["message"].as_str().unwrap().contains("'ioi'"),
            "{body}"
        );

        let response = AppError::from(PluginOutage::Unavailable).into_response();
        assert_eq!(
            response.headers().get("Retry-After").unwrap(),
            &PLUGIN_RETRY_AFTER_SECS.to_string()
        );
    }

    /// Sanity check that 429 RateLimited and 503 Overloaded both
    /// flow Retry-After through `IntoResponse` - the implementation
    /// shares the branch, so a regression in either path would also
    /// drop the header for the other.
    #[tokio::test]
    async fn rate_limited_still_maps_to_429_with_retry_after() {
        let err = AppError::RateLimited { retry_after: 1 };
        let response = err.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(
            response
                .headers()
                .get("Retry-After")
                .expect("Retry-After header"),
            "1"
        );
    }
}
