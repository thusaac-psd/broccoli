use std::time::Duration;

use anyhow::Context;
use worker::config::WorkerAppConfig;
use worker::runtime::WorkerRuntime;

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
    common::config::warn_unknown_keys(&config.unknown_keys, "BROCCOLI");
    nofile.log();
    let runtime = WorkerRuntime::build(config).await?;
    runtime.run().await
}

/// Prove that the *running* worker process is alive and talking to Redis, not
/// merely that this probe can open a fresh connection: the worker rewrites its
/// heartbeat key every few seconds with a short TTL, so the key exists only
/// while the worker's own Redis connection works. A key written from another
/// host means two workers share this worker id.
async fn check_own_heartbeat(
    redis_url: &str,
    namespace: &str,
    worker_id: &str,
) -> anyhow::Result<()> {
    use common::liveness::NodeRole;
    use common::worker::HeartbeatPayload;

    let client = redis::Client::open(redis_url).context("Failed to initialize Redis client")?;
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .context("Failed to connect to Redis")?;
    let raw: Option<String> = redis::cmd("GET")
        .arg(NodeRole::Worker.key(namespace, worker_id))
        .query_async(&mut conn)
        .await
        .context("Redis heartbeat read failed")?;
    let raw = raw.with_context(|| {
        format!(
            "no live heartbeat for worker `{worker_id}`: the worker process is not running, \
             is still starting, or cannot reach Redis"
        )
    })?;
    let payload: HeartbeatPayload =
        serde_json::from_str(&raw).context("Unparseable worker heartbeat")?;
    let local = hostname::get().ok().and_then(|h| h.into_string().ok());
    if let (Some(theirs), Some(ours)) = (payload.hostname.as_deref(), local.as_deref()) {
        anyhow::ensure!(
            theirs == ours,
            "the heartbeat for worker `{worker_id}` is written by host `{theirs}`, not `{ours}`: \
             two workers share this worker id"
        );
    }
    Ok(())
}

async fn run_healthcheck() -> anyhow::Result<()> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    let config = WorkerAppConfig::load().context("Failed to load config")?;

    let db = common::db::connect(
        &config.database.url,
        &common::db::PgPoolSpec {
            acquire_timeout: Duration::from_secs(5),
            ..common::db::PgPoolSpec::new(1, 0)
        },
        &config.database.session(),
    )
    .await
    .context("Failed to connect to database")?;
    db.execute_raw(Statement::from_string(
        DbBackend::Postgres,
        "SELECT 1".to_string(),
    ))
    .await
    .context("Database ping failed")?;

    if config.mq.enabled {
        tokio::time::timeout(
            Duration::from_secs(5),
            check_own_heartbeat(
                &config.mq.url,
                &config.mq.operation_queue_name,
                &config.worker.id,
            ),
        )
        .await
        .context("Redis heartbeat check timed out")??;
    }

    if worker::sandbox_preflight::uses_isolate(&config.worker.sandbox_backend) {
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
            worker::sandbox_preflight::controllers_present(&controllers)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::runners::AsyncRunner;

    async fn set_heartbeat(url: &str, worker_id: &str, hostname: Option<String>) {
        let payload = serde_json::to_value(common::worker::HeartbeatPayload {
            id: worker_id.to_string(),
            started_at: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            last_seen: chrono::DateTime::from_timestamp(1_700_000_003, 0).unwrap(),
            in_flight: 0,
            max_concurrency: None,
            fairness_mode: "unknown".into(),
            sandbox_backend: "isolate".into(),
            version: "test".into(),
            hostname,
            ip_addresses: Vec::new(),
            os: None,
            arch: None,
            cpu_count: None,
            pid: None,
        })
        .unwrap();
        let client = redis::Client::open(url).unwrap();
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::cmd("SET")
            .arg(common::liveness::NodeRole::Worker.key("ops", worker_id))
            .arg(payload.to_string())
            .arg("EX")
            .arg(15)
            .query_async(&mut conn)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn healthcheck_requires_this_workers_live_heartbeat() {
        let container = common::test_util::redis_image()
            .start()
            .await
            .expect("start redis");
        let port = container.get_host_port_ipv4(6379).await.unwrap();
        let url = format!("redis://127.0.0.1:{port}");
        let local = hostname::get().ok().and_then(|h| h.into_string().ok());

        // Redis is reachable, but no worker process is heartbeating: unhealthy.
        let err = check_own_heartbeat(&url, "ops", "w1").await.unwrap_err();
        assert!(err.to_string().contains("no live heartbeat"), "{err:#}");

        set_heartbeat(&url, "w1", local.clone()).await;
        check_own_heartbeat(&url, "ops", "w1")
            .await
            .expect("live heartbeat from this host");

        // Same worker id heartbeating from another host: a duplicate id.
        set_heartbeat(&url, "w2", Some("some-other-host".into())).await;
        let err = check_own_heartbeat(&url, "ops", "w2").await.unwrap_err();
        assert!(err.to_string().contains("share this worker id"), "{err:#}");
    }
}
