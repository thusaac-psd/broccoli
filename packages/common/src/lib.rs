// `#[async_trait]` rewrites each async method to return a boxed future (already
// `#[must_use]`) while also stamping its own `#[must_use]` on the method, so
// clippy's `double_must_use` fires on macro-generated code we don't control.
// Every hit in this crate comes from that expansion (the `Hook`, `GenericHook`,
// `BlobStore`, and `Executor` traits); suppress it crate-wide rather than
// scattering per-trait `#[allow]`s.
#![allow(clippy::double_must_use)]

pub mod cancel;
pub mod config;
pub mod dlq;
pub mod event;
pub mod hook;
pub mod metrics;
pub mod observability;
pub mod retry;
pub mod rlimit;
pub mod storage;
pub mod submission_status;
pub mod warm;
pub mod worker;

pub use config::{DlqConfig, MqAppConfig};
pub use dlq::{DlqEnvelope, DlqErrorCode, DlqMessageType, SubmissionDlqErrorCode};
pub use submission_status::{SubmissionStatus, Verdict};
