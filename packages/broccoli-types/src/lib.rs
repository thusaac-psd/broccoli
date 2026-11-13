//! Shared host<->guest wire and domain types for the Broccoli online judge.
//!
//! This crate sits at the bottom of the workspace graph: it carries the serde
//! contracts (verdicts, the operation/step model, checker/evaluate types, hook
//! events, filter IO, the `plugin.toml` manifest) with no heavy dependencies,
//! so base crates (`common`, `worker`, `plugin-core`), the published guest SDK
//! (`broccoli-server-sdk`) and the plugin developer CLI can all depend on it
//! downward instead of the base crates depending upward on the guest SDK.
pub mod error;
pub mod manifest;
pub mod permissions;
pub mod types;
