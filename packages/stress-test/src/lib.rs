// `#[async_trait]` stamps its own `#[must_use]` on each rewritten async method
// even though the boxed future it returns is already `#[must_use]`, so clippy's
// `double_must_use` fires on macro output we don't control. The only hit is the
// `Scenario` trait in `fault::scenarios`; suppress crate-wide.
#![allow(clippy::double_must_use)]

pub mod bootstrap;
pub mod cleanup;
pub mod cli;
pub mod client;
pub mod correctness;
pub mod dto;
pub mod error;
pub mod events;
pub mod fault;
pub mod fixtures;
pub mod load;
pub mod mixed;
pub mod passthrough;
pub mod perf;
pub mod report;
pub mod runner;
pub mod scenarios;
pub mod ui;
pub mod version_check;
