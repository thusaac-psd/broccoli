// `#[async_trait]` stamps its own `#[must_use]` on each rewritten async method
// even though the boxed future it returns is already `#[must_use]`, so clippy's
// `double_must_use` fires on macro output we don't control. Every hit is one of
// the `models::operation` async traits (`SandboxManager`, `FileCacher`,
// `TaskCacheStore`, `CacheLeaderElector`); suppress crate-wide rather than
// scattering per-trait `#[allow]`s.
#![allow(clippy::double_must_use)]

pub mod config;
pub mod consumer;
pub mod error;
pub mod heartbeat;
pub mod metrics;
pub mod models;
pub mod sandbox_preflight;
pub mod system_info;
mod task_runner;
pub mod toolchain_fingerprint;
pub mod warm;

pub use config::{DatabaseConfig, MqAppConfig, StorageConfig, WorkerAppConfig, WorkerConfig};
pub use error::WorkerError;
