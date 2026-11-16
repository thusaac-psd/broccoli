//! The error body every failed request returns.

use serde::{Deserialize, Serialize};

/// What went wrong. `code` and `details` are the contract a client reads;
/// `message` is an English diagnostic for logs and for people reading the
/// API, the same whatever `Accept-Language` says. A client shows the user its
/// own text for `code` (the web app's `apiError.<code>` keys) and, for a
/// refused config, for each `details.fields[]` entry (its `kind` and
/// parameters, or the plugin's translation `key`). A code a plugin sent (a
/// plugin route's client error, a hook's rejection) is `<plugin id>.<CODE>`,
/// translated only by `<plugin id>.apiError.<CODE>` in that plugin's pack; a
/// code without a plugin id is the server's own.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ErrorBody {
    /// Machine-readable error code. Any POST sent with an `Idempotency-Key`
    /// can also answer: 422 `IDEMPOTENCY_KEY_MISMATCH` (key reused for a
    /// different method, path, query or body); 409 `IDEMPOTENCY_KEY_IN_PROGRESS`
    /// (the original is still running); 409 `IDEMPOTENCY_RESPONSE_NOT_STORED`
    /// (the original succeeded but its response carried credentials and was not
    /// kept); 409 `IDEMPOTENCY_RESPONSE_TOO_LARGE` (the original succeeded but
    /// its response was too large to keep); 409 `IDEMPOTENCY_OUTCOME_UNKNOWN`
    /// (the original failed or was cut off after it started and may have taken
    /// effect); 413 `PAYLOAD_TOO_LARGE` (body too large for an idempotent
    /// request).
    #[schema(example = "VALIDATION_ERROR")]
    pub code: String,
    /// An English description for logs and developers; never translated.
    #[schema(example = "Title must be 1-256 characters")]
    pub message: String,
    /// What the code is about, in a shape each code documents: for example
    /// `fields` for `VALIDATION_ERROR` on a config, `issues` for
    /// `CONFIG_WOULD_BREAK`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Declares [`HostCode`] from one list: each code's variant, its wire string,
/// and whether a client shows its own text for it.
macro_rules! host_codes {
    ($(
        $(#[$doc:meta])*
        $variant:ident = $code:literal $(, $generic:ident)?;
    )*) => {
        /// Every error code the host itself sends or records: an API error's
        /// `code`, and the error code of a judgement or code run that the
        /// host (not a plugin) ended. A plugin's own codes reach clients as
        /// `<plugin_id>.<CODE>` and are not here.
        ///
        /// Each code has its own text in every language pack
        /// (`apiError.<CODE>`), except a `generic` one, which a client
        /// describes by its kind and the server's detail.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum HostCode {
            $(
                $(#[$doc])*
                #[serde(rename = $code)]
                $variant,
            )*
        }

        impl HostCode {
            /// Every host code.
            pub const ALL: &'static [HostCode] = &[$(Self::$variant),*];

            /// The code as it is sent and stored.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $code,)*
                }
            }

            /// Whether clients show their own text for this code rather than
            /// describing it by its kind and the server's detail.
            pub const fn has_own_text(self) -> bool {
                match self {
                    $(Self::$variant => host_codes!(@own $($generic)?),)*
                }
            }
        }
    };
    (@own generic) => { false };
    (@own) => { true };
}

host_codes! {
    // API errors.
    /// A request field is invalid; the detail names it.
    ValidationError = "VALIDATION_ERROR", generic;
    ConfigNamespaceNotFound = "CONFIG_NAMESPACE_NOT_FOUND";
    ConfigWouldBreak = "CONFIG_WOULD_BREAK";
    PayloadTooLarge = "PAYLOAD_TOO_LARGE";
    TokenMissing = "TOKEN_MISSING";
    TokenInvalid = "TOKEN_INVALID";
    InvalidCredentials = "INVALID_CREDENTIALS";
    PermissionDenied = "PERMISSION_DENIED";
    ContestNotRegistered = "CONTEST_NOT_REGISTERED";
    NotFound = "NOT_FOUND", generic;
    MethodNotAllowed = "METHOD_NOT_ALLOWED";
    Conflict = "CONFLICT", generic;
    RegistrationClosed = "REGISTRATION_CLOSED";
    UsernameTaken = "USERNAME_TAKEN";
    PluginExists = "PLUGIN_EXISTS";
    PluginNotReady = "PLUGIN_NOT_READY";
    PluginLoadFailed = "PLUGIN_LOAD_FAILED";
    PluginUploadDisabled = "PLUGIN_UPLOAD_DISABLED";
    RateLimited = "RATE_LIMITED";
    CheckerSourceUnsupported = "CHECKER_SOURCE_UNSUPPORTED";
    ContestTypeLocked = "CONTEST_TYPE_LOCKED";
    ContestTypeUnavailable = "CONTEST_TYPE_UNAVAILABLE";
    ContestTypeDisabled = "CONTEST_TYPE_DISABLED";
    QueueOverloaded = "QUEUE_OVERLOADED";
    PluginUnavailable = "PLUGIN_UNAVAILABLE";
    PluginDisabled = "PLUGIN_DISABLED";
    StorageUnavailable = "STORAGE_UNAVAILABLE";
    /// The server's database cannot be reached or cannot serve connections.
    DatabaseUnavailable = "DATABASE_UNAVAILABLE";
    /// The server's message queue (Redis) cannot be reached.
    QueueUnavailable = "QUEUE_UNAVAILABLE";
    /// A plugin hook refused the request without a code of its own.
    PluginRejected = "PLUGIN_REJECTED";
    IdempotencyKeyInProgress = "IDEMPOTENCY_KEY_IN_PROGRESS";
    IdempotencyKeyMismatch = "IDEMPOTENCY_KEY_MISMATCH";
    IdempotencyResponseTooLarge = "IDEMPOTENCY_RESPONSE_TOO_LARGE";
    IdempotencyResponseNotStored = "IDEMPOTENCY_RESPONSE_NOT_STORED";
    IdempotencyOutcomeUnknown = "IDEMPOTENCY_OUTCOME_UNKNOWN";
    InternalError = "INTERNAL_ERROR";
    // Submission admission.
    /// The plugin's submission slot changed while the submission was admitted.
    SubmissionInFlight = "SUBMISSION_IN_FLIGHT";
    /// The admission state the plugin checked changed meanwhile.
    SubmissionStateChanged = "SUBMISSION_STATE_CHANGED";
    /// The submission arrived after the plugin's admission window closed.
    SubmissionTooLate = "SUBMISSION_TOO_LATE";
    // Judgement and code run errors.
    NoHandlerRegistered = "NO_HANDLER_REGISTERED";
    ProblemNotFound = "PROBLEM_NOT_FOUND";
    DatabaseError = "DATABASE_ERROR";
    InvalidFiles = "INVALID_FILES";
    BlobStoreError = "BLOB_STORE_ERROR";
    SerializationError = "SERIALIZATION_ERROR";
    PluginError = "PLUGIN_ERROR";
    PluginInvalidOutput = "PLUGIN_INVALID_OUTPUT";
    PluginExecutionError = "PLUGIN_EXECUTION_ERROR";
    DispatchTimeout = "DISPATCH_TIMEOUT";
    PluginSessionAborted = "PLUGIN_SESSION_ABORTED";
    PluginNoVerdict = "PLUGIN_NO_VERDICT";
    /// The queue was unreachable for one of the judgement's operations.
    QueueUnreachable = "QUEUE_UNREACHABLE";
    /// The queue lost one of the judgement's operations or its reply.
    OperationLost = "OPERATION_LOST";
    /// The worker that took an operation lacks a platform tool it needs.
    PlatformToolMissing = "PLATFORM_TOOL_MISSING";
    /// No live worker takes work.
    NoCapableWorker = "NO_CAPABLE_WORKER";
    /// The problem's checker program failed.
    CheckerFailed = "CHECKER_FAILED";
    /// A program the problem supplies besides its checker (an interactor or
    /// a manager) failed: every attempt fails the same way.
    ProblemFailed = "PROBLEM_FAILED";
    /// A contest-type plugin refused the submission deterministically
    /// (`OnSubmissionOutput::refused`). Terminal: never re-judged
    /// automatically.
    PluginRefused = "PLUGIN_REFUSED";
    StuckJob = "STUCK_JOB";
    DispatchRetryExhausted = "DISPATCH_RETRY_EXHAUSTED";
    /// A judgement still in flight when a newer judgement replaced it as
    /// current (rejudge, DLQ retry, applying another judgement).
    Superseded = "SUPERSEDED";
    // Dead letters: why a message or a stuck unit was dead-lettered.
    /// An operation task failed past its retry budget.
    MaxRetriesExceeded = "MAX_RETRIES_EXCEEDED";
    /// The queue handed a task out past its delivery limit: every worker it
    /// reached died or wedged while holding it.
    RedeliveryExhausted = "REDELIVERY_EXHAUSTED";
    /// A queued message could not be read.
    DeserializationError = "DESERIALIZATION_ERROR";
}

impl std::fmt::Display for HostCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
