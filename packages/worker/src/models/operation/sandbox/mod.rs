mod capture;
pub mod error;
pub mod isolate;
pub mod mock;

use async_trait::async_trait;
pub use broccoli_types::types::{
    DirectoryOptions, DirectoryRule, EnvRule, ExecutionResult, ResourceLimits, RunOptions,
    SandboxStatus,
};
use error::SandboxError;
use std::path::PathBuf;

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SandboxManager {
    async fn create_sandbox(&self, id: Option<&str>) -> Result<PathBuf, SandboxError>;
    async fn remove_sandbox(&self, id: &str) -> Result<(), SandboxError>;
    async fn execute(
        &self,
        box_id: &str,
        argv: Vec<String>,
        run_options: &RunOptions,
    ) -> Result<ExecutionResult, SandboxError>;
}
