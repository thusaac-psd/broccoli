//! Fault-injection harness scenarios.
//!
//! They live in stress-test as `fault::*` so operators need only one binary
//! for stack validation. The CLI is wired in under the `fault` subcommand (see `crate::cli`); the dispatcher
//! lives in `crate::fault::runner`.

pub mod runner;
pub mod scenarios;
pub mod transcript;

pub use scenarios::{Scenario, ScenarioContext, ScenarioOutcome};
pub use transcript::{Event, EventKind, Severity, Transcript};
