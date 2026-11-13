//! Request and response types of the Broccoli REST API.
//!
//! The server serializes these types and documents them in its OpenAPI
//! document; the Rust clients (`broccoli-cli-core`, the stress-test harness)
//! deserialize the same definitions, so a client cannot drift from the wire
//! contract. Server-only conversions (from database models) live in the server.

pub mod attachment;
pub mod auth;
pub mod clarification;
pub mod code_run;
pub mod contest;
pub mod dlq;
pub mod error;
pub mod plugin;
pub mod problem;
pub mod shared;
pub mod status;
pub mod submission;

pub use status::{StatusPhase, SubmissionStatus, Verdict};
