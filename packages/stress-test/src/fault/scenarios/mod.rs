pub mod burst;
pub mod cancel_storm;
pub mod kill_server_recovery;
pub mod rolling_worker_restart;

use async_trait::async_trait;

use super::transcript::Transcript;

pub struct ScenarioContext {
    pub redis_url: String,
    pub db_url: Option<String>,
    pub server_url: Option<String>,
    pub admin_token: Option<String>,
    pub kill_command: Option<String>,
}

#[derive(Debug)]
pub struct ScenarioOutcome {
    pub passed: bool,
    pub transcript: Transcript,
}

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Scenario {
    fn name(&self) -> &'static str;
    async fn run(&self, ctx: &ScenarioContext) -> anyhow::Result<ScenarioOutcome>;
}
