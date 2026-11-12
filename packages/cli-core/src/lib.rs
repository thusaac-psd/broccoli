//! Shared CLI plumbing: API client, credentials, config and terminal helpers.

/// The REST API types this crate's client speaks.
pub use broccoli_api_types as api;

pub mod client;
pub mod config;
pub mod credentials;
pub mod fmt;
pub mod model;
pub mod resolve;
pub mod session;
pub mod tls;
pub mod tui;
