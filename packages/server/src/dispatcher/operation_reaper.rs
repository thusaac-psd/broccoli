//! Dead-worker operation reaper (MQ #2, plus the leftover half of MQ #3).
//!
//! The MQ broker has no visibility timeout and no reaper: when a worker crashes
//! mid-operation, the broker-internal task id it was processing stays wedged in
//! `<queue>_processing` forever, and any queued-but-unstarted tasks strand in the
//! worker's private ZSET `<queue>:worker:<id>`. Nothing redelivers them; today
//! they are recovered only by the operation-waiter timeout (`.max(30 min)`). This
//! reaper requeues that stranded work onto the shared queue within seconds of the
//! owner's heartbeat lapsing.
//!
//! Safety, all reused from proven mechanisms:
//! - Requeue goes through the public `mq.publish` API (operations publish with
//!   `disambiguator = None`, so queues are plain ZSETs with no fairness sub-queues
//!   to re-implement).
//! - Double-execution is gated by the worker's existing heartbeat-aware dedup: a
//!   redelivered task whose original owner is alive returns `HeldByOther` (skip);
//!   only a dead-heartbeat owner returns `Stolen` (re-run). The system already
//!   re-runs operations on suspected death, so this adds no new double-run risk.
//! - Operations are self-contained / blob-portable; `target_worker_id` is a cache
//!   locality hint, never session affinity, so requeuing to the shared queue is
//!   correct.
//!
//! Like the reply-queue [`super::sweeper`], the reaper fails CLOSED when no worker
//! heartbeat exists fleet-wide (an absent liveness signal is not "everyone died")
//! and debounces each dead worker before acting.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use common::worker::Task;
use mq::Mq;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

const WORKER_HEARTBEAT_PREFIX: &str = common::worker::WORKER_HEARTBEAT_KEY_PREFIX;
const WORKER_DEAD_SINCE_PREFIX: &str = "broccoli:worker:dead_since:";
const DEDUP_PREFIX: &str = "broccoli:dedup:";
/// Broker sub-key suffixes for one queue. The empty suffix is the main ZSET and
/// MUST stay last so a `..._processing` key matches `_processing` before `""`.
const BROKER_SUFFIXES: [&str; 4] = ["_processing", "_failed", "_fairness_set", ""];
const DEAD_SINCE_TTL_SECS: u64 = 86_400;

/// How long ago the `dead_since` clock was started, from its key's remaining
/// TTL in milliseconds (`PTTL`). `None` when there is no clock (the key is
/// absent, or has no TTL).
fn dead_for(ttl_ms: i64) -> Option<Duration> {
    if ttl_ms < 0 {
        return None;
    }
    Some(Duration::from_millis(
        (DEAD_SINCE_TTL_SECS * 1000).saturating_sub(ttl_ms as u64),
    ))
}

#[derive(Clone)]
pub struct ReaperConfig {
    /// The shared operation queue (`mq.operation_queue_name`). Private queues are
    /// `<shared>:worker:<id>`; requeued work returns to `<shared>`.
    pub shared_queue: String,
    pub interval_secs: u64,
    /// Debounce a dead worker for this long (on top of the 15s heartbeat TTL)
    /// before requeuing its strands, so a worker briefly between heartbeats is
    /// not reaped out from under itself.
    pub grace_secs: i64,
    /// Per-tick cap on requeues, bounding blast radius. The remainder is logged
    /// and picked up next tick.
    pub max_requeues_per_tick: usize,
    /// Log intended requeues without mutating anything.
    pub dry_run: bool,
}

#[derive(Default, Debug, PartialEq, Eq)]
struct ReapStats {
    requeued: usize,
    dangling: usize,
    /// A valid `Task` payload we could not attribute to an owner (no dedup key):
    /// left in place for the waiter-timeout backstop, never dropped.
    unattributable: usize,
    /// An undeserializable (poison) payload: moved to the `_failed` dead-letter
    /// queue so it stops leaking in `_processing`. See [`dead_letter`].
    dead_lettered: usize,
    failed: usize,
    remaining_over_budget: usize,
}

impl ReapStats {
    fn touched(&self) -> bool {
        self.requeued > 0
            || self.dangling > 0
            || self.unattributable > 0
            || self.dead_lettered > 0
            || self.failed > 0
            || self.remaining_over_budget > 0
    }
}

pub async fn run(
    redis_client: redis::Client,
    mq: Arc<Mq>,
    config: ReaperConfig,
    mut cancel: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(config.interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match reap_once(&redis_client, &mq, &config).await {
                    Ok(stats) if stats.touched() => info!(
                        requeued = stats.requeued,
                        dangling = stats.dangling,
                        unattributable = stats.unattributable,
                        dead_lettered = stats.dead_lettered,
                        failed = stats.failed,
                        remaining_over_budget = stats.remaining_over_budget,
                        dry_run = config.dry_run,
                        "Operation reaper tick"
                    ),
                    Ok(_) => {}
                    Err(e) => error!(error = %e, "Operation reaper tick failed"),
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return;
                }
            }
        }
    }
}

async fn reap_once(
    redis_client: &redis::Client,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
) -> Result<ReapStats, redis::RedisError> {
    let mut conn = redis_client.get_multiplexed_async_connection().await?;

    // Fail closed: with zero worker heartbeats fleet-wide the liveness signal is
    // absent (Redis flush, heartbeat writer down), not "every worker died".
    // Reaping then would requeue the in-flight work of running workers.
    let Some(live) = scan_live_workers(&mut conn).await? else {
        warn!(
            "Operation reaper found zero worker heartbeat keys; liveness signal absent, refusing to reap"
        );
        return Ok(ReapStats::default());
    };

    let mut budget = config.max_requeues_per_tick;
    let mut stats = ReapStats::default();
    // Memoize each worker's dead-and-debounced decision within the tick: a worker
    // can own both a private queue and shared-processing entries.
    let mut decisions: HashMap<String, bool> = HashMap::new();

    reap_private_queues(
        &mut conn,
        mq,
        config,
        &live,
        &mut decisions,
        &mut budget,
        &mut stats,
    )
    .await?;

    reap_shared_processing(
        &mut conn,
        mq,
        config,
        &live,
        &mut decisions,
        &mut budget,
        &mut stats,
    )
    .await?;

    Ok(stats)
}

/// SCAN worker heartbeat keys. Returns `None` when NONE exist (fail-closed
/// signal), otherwise the set of live worker ids.
async fn scan_live_workers(
    conn: &mut redis::aio::MultiplexedConnection,
) -> Result<Option<HashSet<String>>, redis::RedisError> {
    let pattern = format!("{WORKER_HEARTBEAT_PREFIX}*");
    let mut cursor = 0_u64;
    let mut live = HashSet::new();

    loop {
        let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(100_u32)
            .query_async(conn)
            .await?;

        for key in keys {
            if let Some(id) = key.strip_prefix(WORKER_HEARTBEAT_PREFIX)
                && !id.is_empty()
            {
                live.insert(id.to_string());
            }
        }

        cursor = next;
        if cursor == 0 {
            break;
        }
    }

    if live.is_empty() {
        Ok(None)
    } else {
        Ok(Some(live))
    }
}

async fn reap_private_queues(
    conn: &mut redis::aio::MultiplexedConnection,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
    live: &HashSet<String>,
    decisions: &mut HashMap<String, bool>,
    budget: &mut usize,
    stats: &mut ReapStats,
) -> Result<(), redis::RedisError> {
    let pattern = format!("{}:worker:*", config.shared_queue);
    let mut cursor = 0_u64;
    let mut owners: HashSet<String> = HashSet::new();

    loop {
        let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(&pattern)
            .arg("COUNT")
            .arg(100_u32)
            .query_async(conn)
            .await?;

        for key in keys {
            if let Some(id) = extract_worker_id(&key, &config.shared_queue) {
                owners.insert(id);
            }
        }

        cursor = next;
        if cursor == 0 {
            break;
        }
    }

    for owner in owners {
        if *budget == 0 {
            break;
        }
        if !reapable_now(conn, config, live, decisions, &owner).await? {
            continue;
        }

        let main_queue = format!("{}:worker:{}", config.shared_queue, owner);
        let processing = format!("{main_queue}_processing");

        // Processing first: those were actually in flight when the worker died.
        let proc_uuids: Vec<String> = redis::cmd("LRANGE")
            .arg(&processing)
            .arg(0_i64)
            .arg(-1_i64)
            .query_async(conn)
            .await?;
        let fully_drained_proc =
            requeue_from_list(conn, mq, config, &processing, &proc_uuids, budget, stats).await?;

        let main_uuids: Vec<String> = redis::cmd("ZRANGE")
            .arg(&main_queue)
            .arg(0_i64)
            .arg(-1_i64)
            .query_async(conn)
            .await?;
        let fully_drained_main =
            requeue_from_zset(conn, mq, config, &main_queue, &main_uuids, budget, stats).await?;

        // Only retire the debounce clock once both sources are fully cleared, so
        // budget-capped or failed remainders are revisited next tick.
        if fully_drained_proc && fully_drained_main && !config.dry_run {
            let _: i64 = redis::cmd("DEL")
                .arg(format!("{WORKER_DEAD_SINCE_PREFIX}{owner}"))
                .query_async(conn)
                .await?;
        }
    }

    Ok(())
}

async fn reap_shared_processing(
    conn: &mut redis::aio::MultiplexedConnection,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
    live: &HashSet<String>,
    decisions: &mut HashMap<String, bool>,
    budget: &mut usize,
    stats: &mut ReapStats,
) -> Result<(), redis::RedisError> {
    let processing = format!("{}_processing", config.shared_queue);
    let uuids: Vec<String> = redis::cmd("LRANGE")
        .arg(&processing)
        .arg(0_i64)
        .arg(-1_i64)
        .query_async(conn)
        .await?;

    for uuid in uuids {
        if *budget == 0 {
            stats.remaining_over_budget += 1;
            continue;
        }

        let fields: HashMap<String, String> =
            redis::cmd("HGETALL").arg(&uuid).query_async(conn).await?;
        let Some(task) = parse_task(&fields) else {
            if fields.is_empty() {
                // Dangling: payload hash already gone. Drop the strand pointer.
                remove_from_list(conn, config, &processing, &uuid).await?;
                stats.dangling += 1;
            } else {
                // Poison: a worker popped this into `_processing` and then failed
                // to deserialize it (MQ #6), so it can never be delivered and no
                // live worker holds it. Move it to the `_failed` dead-letter queue
                // instead of leaking here until the waiter times out.
                dead_letter(conn, config, &processing, StrandSource::List, &uuid).await?;
                stats.dead_lettered += 1;
            }
            continue;
        };

        // Attribute via the dedup owner, then check that owner's liveness. An
        // entry with no dedup owner (expired key / dedup disabled) is
        // unattributable: leave it for the waiter-timeout backstop.
        let owner: Option<String> = redis::cmd("GET")
            .arg(format!("{DEDUP_PREFIX}{}", task.id))
            .query_async(conn)
            .await?;
        let Some(owner) = owner.filter(|o| !o.is_empty()) else {
            stats.unattributable += 1;
            continue;
        };
        if !reapable_now(conn, config, live, decisions, &owner).await? {
            continue;
        }

        if requeue_task(conn, mq, config, &uuid, &task, stats).await? {
            remove_from_list(conn, config, &processing, &uuid).await?;
            *budget -= 1;
        }
    }

    Ok(())
}

/// Whether `owner` is dead AND past its debounce grace. Memoized per tick.
/// Live owners clear their debounce clock; first-seen-dead owners start it.
async fn reapable_now(
    conn: &mut redis::aio::MultiplexedConnection,
    config: &ReaperConfig,
    live: &HashSet<String>,
    decisions: &mut HashMap<String, bool>,
    owner: &str,
) -> Result<bool, redis::RedisError> {
    if let Some(decided) = decisions.get(owner) {
        return Ok(*decided);
    }

    let dead_since_key = format!("{WORKER_DEAD_SINCE_PREFIX}{owner}");

    if live.contains(owner) {
        if !config.dry_run {
            let _: i64 = redis::cmd("DEL")
                .arg(&dead_since_key)
                .query_async(conn)
                .await?;
        }
        decisions.insert(owner.to_string(), false);
        return Ok(false);
    }

    // The debounce clock is the key's own TTL, so how long the worker has
    // been dead is measured by Redis alone. Storing one replica's wall-clock
    // timestamp and subtracting it from another replica's clock let a few
    // seconds of skew between servers defeat or stretch the grace. `NX` starts
    // the clock once, however many replicas observe the death; the `PTTL` in
    // the same round trip reads how much of it is left.
    let (started_now, ttl_ms): (bool, i64) = if config.dry_run {
        let ttl_ms = redis::cmd("PTTL")
            .arg(&dead_since_key)
            .query_async(conn)
            .await?;
        (false, ttl_ms)
    } else {
        let (set, ttl_ms): (Option<String>, i64) = redis::pipe()
            .cmd("SET")
            .arg(&dead_since_key)
            .arg(1)
            .arg("NX")
            .arg("EX")
            .arg(DEAD_SINCE_TTL_SECS)
            .cmd("PTTL")
            .arg(&dead_since_key)
            .query_async(conn)
            .await?;
        (set.is_some(), ttl_ms)
    };
    // The tick that starts the clock always debounces, even with a 0 grace.
    let ready = !started_now
        && dead_for(ttl_ms)
            .is_some_and(|dead_for| dead_for.as_secs() >= config.grace_secs.max(0) as u64);
    if !ready {
        debug!(worker_id = owner, "Worker observed dead; debouncing reaper");
    }

    decisions.insert(owner.to_string(), ready);
    Ok(ready)
}

/// Requeue every uuid drawn from a LIST source (a `_processing` list), removing
/// each on success. Returns whether the whole source was drained (nothing left
/// due to budget or failure), so the caller can retire the debounce clock.
async fn requeue_from_list(
    conn: &mut redis::aio::MultiplexedConnection,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
    source: &str,
    uuids: &[String],
    budget: &mut usize,
    stats: &mut ReapStats,
) -> Result<bool, redis::RedisError> {
    let mut drained = true;
    for uuid in uuids {
        if *budget == 0 {
            stats.remaining_over_budget += 1;
            drained = false;
            continue;
        }
        match requeue_one(conn, mq, config, uuid, stats).await? {
            RequeueOutcome::Requeued => {
                remove_from_list(conn, config, source, uuid).await?;
                *budget -= 1;
            }
            RequeueOutcome::Dangling => {
                remove_from_list(conn, config, source, uuid).await?;
            }
            RequeueOutcome::Poison => {
                dead_letter(conn, config, source, StrandSource::List, uuid).await?;
                stats.dead_lettered += 1;
            }
            RequeueOutcome::LeftInPlace | RequeueOutcome::DryRun => drained = false,
        }
    }
    Ok(drained)
}

/// Requeue every uuid drawn from a ZSET source (a private main queue), removing
/// each on success. Returns whether the whole source was drained.
async fn requeue_from_zset(
    conn: &mut redis::aio::MultiplexedConnection,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
    source: &str,
    uuids: &[String],
    budget: &mut usize,
    stats: &mut ReapStats,
) -> Result<bool, redis::RedisError> {
    let mut drained = true;
    for uuid in uuids {
        if *budget == 0 {
            stats.remaining_over_budget += 1;
            drained = false;
            continue;
        }
        let outcome = requeue_one(conn, mq, config, uuid, stats).await?;
        match outcome {
            // Requeued: hash already deleted by requeue_task. Dangling: hash was
            // already gone. Either way just drop the ZSET pointer.
            RequeueOutcome::Requeued | RequeueOutcome::Dangling => {
                if !config.dry_run {
                    let _: i64 = redis::cmd("ZREM")
                        .arg(source)
                        .arg(uuid)
                        .query_async(conn)
                        .await?;
                }
                if matches!(outcome, RequeueOutcome::Requeued) {
                    *budget -= 1;
                }
            }
            RequeueOutcome::Poison => {
                dead_letter(conn, config, source, StrandSource::Zset, uuid).await?;
                stats.dead_lettered += 1;
            }
            RequeueOutcome::LeftInPlace | RequeueOutcome::DryRun => drained = false,
        }
    }
    Ok(drained)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RequeueOutcome {
    Requeued,
    Dangling,
    /// Undeserializable payload (MQ #6): the caller dead-letters it.
    Poison,
    LeftInPlace,
    DryRun,
}

/// How a stranded uuid is anchored in its queue family, so [`dead_letter`] knows
/// which removal command to issue.
#[derive(Clone, Copy)]
enum StrandSource {
    List,
    Zset,
}

/// Read a stranded uuid's payload hash, republish the `Task` to the shared queue,
/// and delete the orphaned hash. Does NOT remove the uuid from its source list /
/// zset - the caller does that only on a removable outcome.
async fn requeue_one(
    conn: &mut redis::aio::MultiplexedConnection,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
    uuid: &str,
    stats: &mut ReapStats,
) -> Result<RequeueOutcome, redis::RedisError> {
    let fields: HashMap<String, String> = redis::cmd("HGETALL").arg(uuid).query_async(conn).await?;

    if fields.is_empty() {
        stats.dangling += 1;
        return Ok(RequeueOutcome::Dangling);
    }

    let Some(task) = parse_task(&fields) else {
        // Poison (MQ #6): undeserializable payload. The caller dead-letters it.
        return Ok(RequeueOutcome::Poison);
    };

    if requeue_task(conn, mq, config, uuid, &task, stats).await? {
        Ok(RequeueOutcome::Requeued)
    } else if config.dry_run {
        Ok(RequeueOutcome::DryRun)
    } else {
        Ok(RequeueOutcome::LeftInPlace)
    }
}

/// Publish `task` to the shared queue and delete the old payload hash `uuid`.
/// Returns true when the task was really requeued (false = dry-run or publish
/// error, in which case the strand is left untouched for a later retry).
async fn requeue_task(
    conn: &mut redis::aio::MultiplexedConnection,
    mq: &Arc<Mq>,
    config: &ReaperConfig,
    uuid: &str,
    task: &Task,
    stats: &mut ReapStats,
) -> Result<bool, redis::RedisError> {
    if config.dry_run {
        info!(task_id = %task.id, uuid, queue = %config.shared_queue, "Reaper dry-run: would requeue");
        return Ok(false);
    }

    match mq
        .publish::<Task>(&config.shared_queue, None, task, None)
        .await
    {
        Ok(_) => {
            let _: i64 = redis::cmd("DEL").arg(uuid).query_async(conn).await?;
            stats.requeued += 1;
            Ok(true)
        }
        Err(e) => {
            warn!(task_id = %task.id, uuid, error = %e, "Reaper requeue publish failed; leaving strand");
            stats.failed += 1;
            Ok(false)
        }
    }
}

async fn remove_from_list(
    conn: &mut redis::aio::MultiplexedConnection,
    config: &ReaperConfig,
    source: &str,
    uuid: &str,
) -> Result<(), redis::RedisError> {
    if config.dry_run {
        return Ok(());
    }
    let _: i64 = redis::cmd("LREM")
        .arg(source)
        .arg(1_i64)
        .arg(uuid)
        .query_async(conn)
        .await?;
    Ok(())
}

/// Dead-letter a poison (undeserializable) strand: remove it from its source and
/// push its task_id onto the shared `<queue>_failed` list, matching the broker's
/// own reject-at-max-attempts convention (LPUSH the task_id, keep the payload
/// hash for inspection). A poison message can never be delivered and no live
/// worker holds it, so this is unconditional - no owner attribution or liveness
/// check needed. It stops the message leaking in `_processing` forever.
async fn dead_letter(
    conn: &mut redis::aio::MultiplexedConnection,
    config: &ReaperConfig,
    source: &str,
    kind: StrandSource,
    uuid: &str,
) -> Result<(), redis::RedisError> {
    if config.dry_run {
        warn!(
            uuid,
            source, "Reaper dry-run: would dead-letter poison (undeserializable) message"
        );
        return Ok(());
    }

    match kind {
        StrandSource::List => {
            let _: i64 = redis::cmd("LREM")
                .arg(source)
                .arg(1_i64)
                .arg(uuid)
                .query_async(conn)
                .await?;
        }
        StrandSource::Zset => {
            let _: i64 = redis::cmd("ZREM")
                .arg(source)
                .arg(uuid)
                .query_async(conn)
                .await?;
        }
    }

    let failed_queue = format!("{}_failed", config.shared_queue);
    let _: i64 = redis::cmd("LPUSH")
        .arg(&failed_queue)
        .arg(uuid)
        .query_async(conn)
        .await?;
    warn!(
        uuid,
        failed_queue, "Reaper dead-lettered a poison (undeserializable) operation message"
    );
    Ok(())
}

fn parse_task(fields: &HashMap<String, String>) -> Option<Task> {
    let payload = fields.get("payload")?;
    serde_json::from_str::<Task>(payload).ok()
}

/// Extract the worker id from a private-queue family key
/// (`<shared>:worker:<id>[suffix]`), guarding against false matches by
/// reconstructing the canonical key. `_processing` is tried before the empty
/// suffix, so a `..._processing` key yields the base id, not `<id>_processing`.
fn extract_worker_id(key: &str, shared_queue: &str) -> Option<String> {
    let prefix = format!("{shared_queue}:worker:");
    let suffix = BROKER_SUFFIXES.iter().find(|s| key.ends_with(**s))?;
    let base = key.strip_suffix(*suffix)?;
    let id = base.strip_prefix(&prefix)?;
    if id.is_empty() {
        return None;
    }
    let canonical = format!("{prefix}{id}{suffix}");
    (canonical == key).then(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> ReaperConfig {
        ReaperConfig {
            shared_queue: "operation_tasks".to_string(),
            interval_secs: 30,
            grace_secs: 0,
            max_requeues_per_tick: 1000,
            dry_run: false,
        }
    }

    fn task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            task_type: "operation".to_string(),
            executor_name: "operation".to_string(),
            payload: serde_json::json!({}),
            result_queue: "results".to_string(),
            operation_batch_id: None,
            reply_queue: None,
            priority: None,
            trace_context: None,
            enqueued_at_unix_ms: None,
        }
    }

    #[test]
    fn extracts_worker_id_from_private_queue_family_keys() {
        assert_eq!(
            extract_worker_id("operation_tasks:worker:w1", "operation_tasks").as_deref(),
            Some("w1")
        );
        assert_eq!(
            extract_worker_id("operation_tasks:worker:w1_processing", "operation_tasks").as_deref(),
            Some("w1")
        );
        assert_eq!(
            extract_worker_id("operation_tasks:worker:w1_failed", "operation_tasks").as_deref(),
            Some("w1")
        );
        // Wrong shared prefix, or the shared queue's own keys, are rejected.
        assert_eq!(
            extract_worker_id("operation_tasks_processing", "operation_tasks"),
            None
        );
        assert_eq!(
            extract_worker_id("other:worker:w1", "operation_tasks"),
            None
        );
        assert_eq!(
            extract_worker_id("operation_tasks:worker:", "operation_tasks"),
            None
        );
    }

    #[test]
    fn parse_task_reads_the_payload_field() {
        let json = serde_json::to_string(&task("t1")).unwrap();
        let mut fields = HashMap::new();
        fields.insert("payload".to_string(), json);
        fields.insert("attempts".to_string(), "0".to_string());
        assert_eq!(parse_task(&fields).unwrap().id, "t1");

        assert!(parse_task(&HashMap::new()).is_none());
        let mut bad = HashMap::new();
        bad.insert("payload".to_string(), "not json".to_string());
        assert!(parse_task(&bad).is_none());
    }

    // ---- Redis integration tests (testcontainers) ----

    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    async fn redis_and_mq() -> (
        redis::Client,
        Arc<Mq>,
        testcontainers::ContainerAsync<Redis>,
    ) {
        let container = Redis::default().start().await.expect("start redis");
        let port = container
            .get_host_port_ipv4(6379)
            .await
            .expect("redis port");
        let url = format!("redis://127.0.0.1:{port}");
        let client = redis::Client::open(url.as_str()).expect("client");
        let mq = Arc::new(
            mq::init_mq(mq::MqConfig { url, pool_size: 4 })
                .await
                .expect("init mq"),
        );
        (client, mq, container)
    }

    async fn drain_queue(mq: &Arc<Mq>, queue: &str) -> Vec<String> {
        let mut ids = Vec::new();
        while let Some(msg) = mq.try_consume::<Task>(queue, None).await.expect("consume") {
            ids.push(msg.payload.id.clone());
            mq.acknowledge::<Task>(queue, msg).await.expect("ack");
        }
        ids
    }

    /// A dead worker's private processing + private queue are requeued onto the
    /// shared queue, and the strands are cleaned up.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn requeues_dead_worker_private_strands_to_shared() {
        use redis::AsyncCommands;
        let (client, mq, _c) = redis_and_mq().await;
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let cfg = cfg();

        // A live worker must exist so the reaper does not fail closed.
        conn.set::<_, _, ()>("broccoli:worker:heartbeat:alive", "1")
            .await
            .unwrap();

        // Publish two tasks to the dead worker's private queue, then simulate one
        // moving into processing (consume without ack leaves it in _processing).
        let private = format!("{}:worker:dead", cfg.shared_queue);
        mq.publish::<Task>(&private, None, &task("in-flight"), None)
            .await
            .unwrap();
        mq.publish::<Task>(&private, None, &task("still-queued"), None)
            .await
            .unwrap();
        let taken = mq
            .try_consume::<Task>(&private, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(taken.payload.id, "in-flight");
        // `taken` is now in `<private>_processing`, never acked.

        // First tick starts the debounce clock (grace 0 means the SECOND tick acts).
        let stats = reap_once(&client, &mq, &cfg).await.unwrap();
        assert_eq!(stats.requeued, 0, "first sighting only debounces");

        let stats = reap_once(&client, &mq, &cfg).await.unwrap();
        assert_eq!(
            stats.requeued, 2,
            "both strands requeued on the second tick"
        );

        let mut ids = drain_queue(&mq, &cfg.shared_queue).await;
        ids.sort();
        assert_eq!(
            ids,
            vec!["in-flight".to_string(), "still-queued".to_string()]
        );

        // Strands cleaned.
        let proc_len: i64 = conn.llen(format!("{private}_processing")).await.unwrap();
        let main_len: i64 = conn.zcard(&private).await.unwrap();
        assert_eq!(proc_len, 0);
        assert_eq!(main_len, 0);
    }

    /// A live worker's in-flight private entry is never touched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn spares_live_worker_strands() {
        use redis::AsyncCommands;
        let (client, mq, _c) = redis_and_mq().await;
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let cfg = cfg();

        conn.set::<_, _, ()>("broccoli:worker:heartbeat:busy", "1")
            .await
            .unwrap();
        let private = format!("{}:worker:busy", cfg.shared_queue);
        mq.publish::<Task>(&private, None, &task("running"), None)
            .await
            .unwrap();
        let _taken = mq
            .try_consume::<Task>(&private, None)
            .await
            .unwrap()
            .unwrap();

        for _ in 0..3 {
            let stats = reap_once(&client, &mq, &cfg).await.unwrap();
            assert_eq!(stats.requeued, 0, "a live worker's work is never reaped");
        }
        let proc_len: i64 = conn.llen(format!("{private}_processing")).await.unwrap();
        assert_eq!(proc_len, 1, "the live worker keeps its in-flight entry");
    }

    /// A shared-queue processing strand owned (via dedup) by a dead worker is
    /// requeued; one owned by a live worker is spared.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attributes_shared_processing_via_dedup_owner() {
        use redis::AsyncCommands;
        let (client, mq, _c) = redis_and_mq().await;
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let cfg = cfg();

        conn.set::<_, _, ()>("broccoli:worker:heartbeat:live-owner", "1")
            .await
            .unwrap();
        // dead-owner has no heartbeat key.

        // Two shared-queue tasks, consumed (moved to _processing) without ack.
        mq.publish::<Task>(&cfg.shared_queue, None, &task("orphaned"), None)
            .await
            .unwrap();
        mq.publish::<Task>(&cfg.shared_queue, None, &task("healthy"), None)
            .await
            .unwrap();
        let a = mq
            .try_consume::<Task>(&cfg.shared_queue, None)
            .await
            .unwrap()
            .unwrap();
        let b = mq
            .try_consume::<Task>(&cfg.shared_queue, None)
            .await
            .unwrap()
            .unwrap();

        // Attribute each via a dedup key -> owner.
        conn.set::<_, _, ()>(format!("{DEDUP_PREFIX}{}", a.payload.id), "dead-owner")
            .await
            .unwrap();
        conn.set::<_, _, ()>(format!("{DEDUP_PREFIX}{}", b.payload.id), "live-owner")
            .await
            .unwrap();

        // Debounce, then act.
        reap_once(&client, &mq, &cfg).await.unwrap();
        let stats = reap_once(&client, &mq, &cfg).await.unwrap();
        assert_eq!(
            stats.requeued, 1,
            "only the dead owner's strand is requeued"
        );

        let ids = drain_queue(&mq, &cfg.shared_queue).await;
        assert_eq!(ids, vec!["orphaned".to_string()]);

        let proc_len: i64 = conn
            .llen(format!("{}_processing", cfg.shared_queue))
            .await
            .unwrap();
        assert_eq!(proc_len, 1, "the live owner's strand stays in processing");
    }

    /// With zero worker heartbeats the reaper fails closed: nothing is touched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fails_closed_when_no_worker_heartbeat_exists() {
        use redis::AsyncCommands;
        let (client, mq, _c) = redis_and_mq().await;
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let cfg = cfg();

        let private = format!("{}:worker:dead", cfg.shared_queue);
        mq.publish::<Task>(&private, None, &task("stranded"), None)
            .await
            .unwrap();
        let _taken = mq
            .try_consume::<Task>(&private, None)
            .await
            .unwrap()
            .unwrap();

        let stats = reap_once(&client, &mq, &cfg).await.unwrap();
        assert_eq!(stats, ReapStats::default(), "no heartbeats -> no action");
        let proc_len: i64 = conn.llen(format!("{private}_processing")).await.unwrap();
        assert_eq!(proc_len, 1, "the strand survives a fail-closed tick");
        let dead_since: bool = conn
            .exists("broccoli:worker:dead_since:dead")
            .await
            .unwrap();
        assert!(
            !dead_since,
            "debounce clock must not start while liveness is absent"
        );
    }

    /// MQ #6: a poison (undeserializable) message stranded in shared
    /// `_processing` is moved to the `_failed` dead-letter queue instead of
    /// leaking, without any owner attribution or debounce.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dead_letters_poison_shared_processing_message() {
        use redis::AsyncCommands;
        let (client, mq, _c) = redis_and_mq().await;
        let mut conn = client.get_multiplexed_async_connection().await.unwrap();
        let cfg = cfg();

        conn.set::<_, _, ()>("broccoli:worker:heartbeat:alive", "1")
            .await
            .unwrap();

        // Seed a poison strand by hand: a uuid wedged in `_processing` whose
        // payload hash cannot deserialize into `Task` (schema drift). This is
        // exactly the state `try_consume::<Task>` leaves behind when it pops a
        // message then fails `into_message()`.
        let processing = format!("{}_processing", cfg.shared_queue);
        let poison = "poison-uuid";
        conn.rpush::<_, _, ()>(&processing, poison).await.unwrap();
        conn.hset_multiple::<_, _, _, ()>(
            poison,
            &[
                ("task_id", poison),
                ("payload", "{\"totally\":\"not a task\"}"),
                ("attempts", "0"),
            ],
        )
        .await
        .unwrap();

        // No debounce required: one tick dead-letters it.
        let stats = reap_once(&client, &mq, &cfg).await.unwrap();
        assert_eq!(
            stats.dead_lettered, 1,
            "the poison message is dead-lettered"
        );
        assert_eq!(stats.requeued, 0);

        let proc_len: i64 = conn.llen(&processing).await.unwrap();
        assert_eq!(proc_len, 0, "poison removed from processing");
        let failed: Vec<String> = conn
            .lrange(format!("{}_failed", cfg.shared_queue), 0, -1)
            .await
            .unwrap();
        assert_eq!(
            failed,
            vec![poison.to_string()],
            "poison lands in the dead-letter queue"
        );
        // The payload hash is kept for inspection, matching broker reject semantics.
        let hash_exists: bool = conn.exists(poison).await.unwrap();
        assert!(
            hash_exists,
            "the poison payload hash is retained for inspection"
        );
    }
}
