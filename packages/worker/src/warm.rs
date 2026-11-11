//! Pre-warm subscriber.
//!
//! Each worker subscribes to the [`WARM_CHANNEL`] Redis pub/sub channel. When the
//! server publishes a warm `job_id`, the worker reads the job's blob-hash list
//! from [`warm_job_key`] and fetches every blob into its local file cache, so the
//! first submissions of a contest hit warm caches instead of paying the cold
//! 30 MB fetch during the opening burst. Per-blob progress is reported to
//! [`warm_progress_key`] for the server to aggregate.
//!
//! The warm cacher shares the on-disk `cache_dir` with the executor's cacher;
//! `BlobStoreFileCacher::new` rehydrates its LRU from disk, so blobs warmed here
//! are visible to executor cachers built later.

use std::sync::Arc;
use std::time::Duration;

use common::metrics::Metrics;
use common::storage::config::create_blob_store;
use common::warm::{
    WARM_CHANNEL, WARM_KEY_TTL_SECS, WarmJob, WarmProgress, WarmState, warm_job_key,
    warm_progress_key,
};
use futures::StreamExt;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::config::WorkerAppConfig;
use crate::models::operation::file_cacher::{BlobStoreFileCacher, FileCacher};

/// Report progress to Redis at least every N processed blobs (plus a final
/// terminal update), so the UI advances smoothly without a write per blob.
const PROGRESS_UPDATE_EVERY: u32 = 4;

/// Delay before re-subscribing after the pub/sub stream drops.
const RESUBSCRIBE_DELAY: Duration = Duration::from_secs(5);

/// Backoff between attempts to build the warm cacher (it needs the DB), capped
/// at [`BUILD_RETRY_MAX`]. Warming is optional, so a failed build is retried
/// rather than disabling pre-warm for the life of the process.
const BUILD_RETRY_START: Duration = Duration::from_secs(5);
const BUILD_RETRY_MAX: Duration = Duration::from_secs(60);

/// Liveness check for the idle subscription. A pub/sub connection only reads,
/// so after the Redis host dies (power loss, partition) a half-open socket
/// never errors and the subscriber would silently stop receiving warm jobs.
/// After `interval` without a message the subscriber `PING`s and resubscribes
/// unless the reply arrives within `timeout`.
#[derive(Debug, Clone, Copy)]
struct Keepalive {
    interval: Duration,
    timeout: Duration,
}

const KEEPALIVE: Keepalive = Keepalive {
    interval: Duration::from_secs(30),
    timeout: Duration::from_secs(5),
};

/// Spawn the pre-warm subscriber. Failures never crash the worker: building the
/// cacher is retried with backoff and a dropped subscription is resubscribed,
/// while judging continues regardless. The runtime holds the returned handle
/// the same way it holds `_cleanup_handle`: this is an independent background
/// helper, so shutdown drains the consume loop and heartbeat and lets this
/// detached task die on process exit.
pub fn spawn(config: WorkerAppConfig, metrics: Option<Metrics>) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(e) = run(config, metrics).await {
            warn!(error = %e, "Pre-warm subscriber disabled");
        }
    })
}

async fn run(config: WorkerAppConfig, metrics: Option<Metrics>) -> anyhow::Result<()> {
    let redis_url = config.mq.url.clone();
    let worker_id = config.worker.id.clone();

    // An invalid URL is a configuration error that no retry fixes.
    let client = redis::Client::open(redis_url.as_str())?;

    let mut backoff = BUILD_RETRY_START;
    let cacher = loop {
        match build_cacher(&config, metrics.clone()).await {
            Ok(cacher) => break cacher,
            Err(e) => {
                warn!(error = %e, retry_in_secs = backoff.as_secs(), "Pre-warm cacher unavailable; retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BUILD_RETRY_MAX);
            }
        }
    };

    info!(worker_id = %worker_id, channel = %WARM_CHANNEL, "Pre-warm subscriber started");

    loop {
        if let Err(e) = subscribe_and_serve(&client, &worker_id, cacher.as_ref(), KEEPALIVE).await {
            warn!(error = %e, "Pre-warm subscription dropped; retrying");
        }
        tokio::time::sleep(RESUBSCRIBE_DELAY).await;
    }
}

/// Build a [`FileCacher`] over the same blob store + cache dir the executor uses.
///
/// KNOWN LIMITATION (low severity): this is a SEPARATE cacher instance from the
/// executor's, each with its own in-memory LRU enforcing `max_cache_size` against
/// the SAME on-disk `cache_dir`. Because neither knows about the other's LRU
/// accounting, total on-disk usage can reach ~2x the configured cap before either
/// evicts. The correct fix is to SHARE a single `Arc<dyn FileCacher>` between the
/// warm subscriber and the executor; it is deferred because that requires
/// threading the executor's cacher through worker startup.
async fn build_cacher(
    config: &WorkerAppConfig,
    metrics: Option<Metrics>,
) -> anyhow::Result<Arc<dyn FileCacher>> {
    let db = common::db::connect(
        &config.database.url,
        &common::db::PgPoolSpec::new(2, 1),
        &config.database.session(),
    )
    .await?;

    let blob_store = create_blob_store(&config.storage.blob_store, db, metrics.clone()).await?;
    let cacher = BlobStoreFileCacher::new(
        blob_store,
        config.storage.cache_dir.clone().into(),
        config.storage.max_cache_size,
        metrics,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    Ok(Arc::new(cacher))
}

async fn subscribe_and_serve(
    client: &redis::Client,
    worker_id: &str,
    cacher: &dyn FileCacher,
    keepalive: Keepalive,
) -> anyhow::Result<()> {
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.subscribe(WARM_CHANNEL).await?;
    // A separate connection for GET/SET - the pub/sub connection can't run cmds.
    let mut cmd = client.get_multiplexed_async_connection().await?;

    let (mut sink, mut stream) = pubsub.split();
    loop {
        let msg = match tokio::time::timeout(keepalive.interval, stream.next()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => return Ok(()),
            Err(_idle) => match tokio::time::timeout(keepalive.timeout, sink.ping::<()>()).await {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => anyhow::bail!("pub/sub keepalive ping failed: {e}"),
                Err(_) => anyhow::bail!(
                    "pub/sub keepalive ping got no reply within {:?}",
                    keepalive.timeout
                ),
            },
        };
        let job_id: String = match msg.get_payload() {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Ignoring malformed warm message");
                continue;
            }
        };
        if let Err(e) = handle_job(&mut cmd, worker_id, cacher, &job_id).await {
            warn!(error = %e, job_id = %job_id, "Warm job failed");
        }
    }
}

async fn handle_job(
    cmd: &mut MultiplexedConnection,
    worker_id: &str,
    cacher: &dyn FileCacher,
    job_id: &str,
) -> anyhow::Result<()> {
    let raw: Option<String> = cmd.get(warm_job_key(job_id)).await?;
    let Some(raw) = raw else {
        // Job key expired or never existed (stale pub/sub message). Ignore.
        return Ok(());
    };
    let job: WarmJob = serde_json::from_str(&raw)?;
    let total = job.total();

    info!(worker_id = %worker_id, job_id = %job_id, total, "Pre-warm started");
    write_progress(
        cmd,
        &progress(&job, worker_id, 0, 0, WarmState::Warming, None),
    )
    .await;

    let mut warmed: u32 = 0;
    let mut processed: u32 = 0;
    let mut fetched_bytes: u64 = 0;
    let mut last_error: Option<String> = None;

    for hash in &job.hashes {
        match cacher.warm(hash).await {
            Ok(bytes) => {
                warmed += 1;
                fetched_bytes += bytes;
            }
            Err(e) => {
                // Continue warming the rest - a single bad blob shouldn't abort
                // the whole contest's warm. Surface the last error + Error state.
                error!(worker_id = %worker_id, job_id = %job_id, hash = %hash, error = %e, "Warm blob failed");
                last_error = Some(e);
            }
        }
        processed += 1;
        if processed.is_multiple_of(PROGRESS_UPDATE_EVERY) && processed < total {
            write_progress(
                cmd,
                &progress(
                    &job,
                    worker_id,
                    warmed,
                    fetched_bytes,
                    WarmState::Warming,
                    None,
                ),
            )
            .await;
        }
    }

    let state = if last_error.is_some() {
        WarmState::Error
    } else {
        WarmState::Complete
    };
    write_progress(
        cmd,
        &progress(&job, worker_id, warmed, fetched_bytes, state, last_error),
    )
    .await;
    info!(worker_id = %worker_id, job_id = %job_id, warmed, total, ?state, "Pre-warm finished");
    Ok(())
}

fn progress(
    job: &WarmJob,
    worker_id: &str,
    warmed: u32,
    fetched_bytes: u64,
    state: WarmState,
    error: Option<String>,
) -> WarmProgress {
    WarmProgress {
        job_id: job.job_id.clone(),
        worker_id: worker_id.to_string(),
        warmed,
        total: job.total(),
        fetched_bytes,
        state,
        error,
        updated_at_unix_ms: chrono::Utc::now().timestamp_millis(),
    }
}

async fn write_progress(cmd: &mut MultiplexedConnection, p: &WarmProgress) {
    let Ok(body) = serde_json::to_string(p) else {
        return;
    };
    let res: Result<(), redis::RedisError> = redis::cmd("SET")
        .arg(warm_progress_key(&p.job_id, &p.worker_id))
        .arg(body)
        .arg("EX")
        .arg(WARM_KEY_TTL_SECS)
        .query_async(cmd)
        .await;
    if let Err(e) = res {
        warn!(error = %e, "Failed to write warm progress");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::operation::file_cacher::NoopFileCacher;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    /// Regression: a subscription whose server stops answering (a half-open
    /// socket after the Redis host dies, simulated with `CLIENT PAUSE`) used to
    /// wait on the stream forever. The keepalive must notice and return so the
    /// caller resubscribes.
    #[tokio::test]
    async fn unresponsive_server_ends_the_subscription_so_it_is_resubscribed() {
        let redis = Redis::default().start().await.expect("redis");
        let port = redis.get_host_port_ipv4(6379).await.expect("port");
        let client = redis::Client::open(format!("redis://127.0.0.1:{port}")).unwrap();
        let keepalive = Keepalive {
            interval: Duration::from_millis(300),
            timeout: Duration::from_millis(300),
        };
        let serve = tokio::spawn({
            let client = client.clone();
            async move { subscribe_and_serve(&client, "w", &NoopFileCacher, keepalive).await }
        });

        // Healthy and idle: keepalive pings succeed, the subscription stays up.
        tokio::time::sleep(Duration::from_millis(1000)).await;
        assert!(
            !serve.is_finished(),
            "an idle healthy subscription stays up"
        );

        let mut admin = client.get_multiplexed_async_connection().await.unwrap();
        let _: () = redis::cmd("CLIENT")
            .arg("PAUSE")
            .arg(5000)
            .query_async(&mut admin)
            .await
            .unwrap();

        let ended = tokio::time::timeout(Duration::from_secs(3), serve)
            .await
            .expect("the keepalive must end a subscription whose server stopped answering")
            .unwrap();
        assert!(
            ended.is_err(),
            "it ends with an error so the caller resubscribes"
        );
    }
}
