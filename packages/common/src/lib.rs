// `#[async_trait]` rewrites each async method to return a boxed future (already
// `#[must_use]`) while also stamping its own `#[must_use]` on the method, so
// clippy's `double_must_use` fires on macro-generated code we don't control.
// Every hit in this crate comes from that expansion (the `Hook`, `GenericHook`,
// `BlobStore`, and `Executor` traits); suppress it crate-wide rather than
// scattering per-trait `#[allow]`s.
#![allow(clippy::double_must_use)]

pub mod cancel;
#[cfg(feature = "sea-orm")]
pub mod clock;
pub mod config;
#[cfg(feature = "sea-orm")]
pub mod db;
pub mod deadline;
pub mod dlq;
pub mod event;
pub mod hook;
pub mod liveness;
pub mod metrics;
pub mod node_identity;
pub mod observability;
pub mod redact;
pub mod redis_conn;
pub mod retry;
pub mod rlimit;
pub mod shutdown;
pub mod storage;
pub mod submission_status;
#[cfg(any(test, feature = "test-util"))]
pub mod test_util;
pub mod warm;
pub mod worker;

pub use config::{DlqConfig, MqAppConfig};
pub use dlq::{DlqEnvelope, DlqErrorCode, DlqMessageType, SubmissionDlqErrorCode};
pub use submission_status::{StatusPhase, SubmissionStatus, Verdict};
