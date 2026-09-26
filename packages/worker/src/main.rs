// The binary re-declares `mod models`, so the same `#[async_trait]` traits that
// trip `double_must_use` in the library (see lib.rs) are compiled again in this
// crate graph. Same false positive from macro output, same crate-wide suppression.
#![allow(clippy::double_must_use)]

mod config;
mod consumer;
mod dedup;
mod error;
mod fairness;
mod heartbeat;
mod metrics;
mod models;
mod runtime;
mod sandbox_preflight;
mod system_info;
mod task_runner;
mod toolchain_fingerprint;
mod warm;

use std::time::Duration;

use anyhow::Context;
use config::WorkerAppConfig;
use runtime::WorkerRuntime;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("broccoli-worker {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if std::env::args().any(|a| a == "--healthcheck") {
        run_healthcheck().await?;
        println!("ok");
        return Ok(());
    }

    // Before anything opens sockets or files; see `common::rlimit`.
    let nofile = common::rlimit::raise_nofile_limit();
    let config = WorkerAppConfig::load().context("Failed to load config")?;
    let _telemetry_guard = common::observability::init_tracing(&config.observability);
    nofile.log();
    let runtime = WorkerRuntime::build(config).await?;
    runtime.run().await
}

async fn run_healthcheck() -> anyhow::Result<()> {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    let config = WorkerAppConfig::load().context("Failed to load config")?;

    let mut db_options = sea_orm::ConnectOptions::new(config.database.url.clone());
    db_options
        .max_connections(1)
        .min_connections(0)
        .connect_timeout(Duration::from_secs(5))
        .acquire_timeout(Duration::from_secs(5))
        .idle_timeout(Duration::from_secs(30))
        .max_lifetime(Duration::from_secs(60))
        .sqlx_logging(false);
    let db = Database::connect(db_options)
        .await
        .context("Failed to connect to database")?;
    db.execute_raw(Statement::from_string(
        DbBackend::Postgres,
        "SELECT 1".to_string(),
    ))
    .await
    .context("Database ping failed")?;

    if config.mq.enabled {
        let client = redis::Client::open(config.mq.url.as_str())
            .context("Failed to initialize Redis health client")?;
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .context("Failed to connect to Redis")?;
        let _pong: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .context("Redis ping failed")?;
    }

    if crate::sandbox_preflight::uses_isolate(&config.worker.sandbox_backend) {
        let status = tokio::process::Command::new(&config.worker.isolate_bin)
            .arg("--version")
            .status()
            .await
            .with_context(|| {
                format!(
                    "Failed to execute isolate binary `{}`",
                    config.worker.isolate_bin
                )
            })?;
        anyhow::ensure!(status.success(), "isolate --version exited with {status}");

        if config.worker.enable_cgroups {
            let controllers = std::fs::read_to_string("/sys/fs/cgroup/cgroup.controllers")
                .context("cgroup v2 controllers are not visible")?;
            crate::sandbox_preflight::controllers_present(&controllers)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }

    Ok(())
}
