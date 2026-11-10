use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use chrono::Utc;
use redis::AsyncCommands;
use redis::aio::MultiplexedConnection;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{info, warn};

// The heartbeat wire payload is defined once in `common` so the worker (producer)
// and the server (consumer) cannot drift. See [`common::worker::HeartbeatPayload`].
use common::worker::HeartbeatPayload;

use crate::system_info::SystemInfo;

const KEY_PREFIX: &str = common::worker::WORKER_HEARTBEAT_KEY_PREFIX;
const TICK_INTERVAL: Duration = Duration::from_secs(5);
const KEY_TTL_SECS: u64 = common::worker::WORKER_HEARTBEAT_TTL_SECS;

#[derive(Clone)]
pub struct InFlightCounter(Arc<AtomicU32>);

impl InFlightCounter {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU32::new(0)))
    }

    pub fn current(&self) -> u32 {
        self.0.load(Ordering::Relaxed)
    }

    pub fn guard(&self) -> InFlightGuard {
        self.0.fetch_add(1, Ordering::Relaxed);
        InFlightGuard(self.0.clone())
    }
}

impl Default for InFlightCounter {
    fn default() -> Self {
        Self::new()
    }
}

pub struct InFlightGuard(Arc<AtomicU32>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct HeartbeatConfig {
    pub redis_url: String,
    pub worker_id: String,
    pub sandbox_backend: String,
    pub max_concurrency: Option<u32>,
    pub fairness_mode: String,
    pub system_info: SystemInfo,
}

pub struct HeartbeatHandle {
    cancel: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl HeartbeatHandle {
    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.await;
        }
    }
}

pub fn spawn(config: HeartbeatConfig, in_flight: InFlightCounter) -> HeartbeatHandle {
    let (cancel_tx, mut cancel_rx) = oneshot::channel::<()>();

    let join = tokio::spawn(async move {
        let started_at = Utc::now();
        let key = format!("{KEY_PREFIX}{}", config.worker_id);
        let conn = Mutex::new(None::<MultiplexedConnection>);
        let client = match redis::Client::open(config.redis_url.as_str()) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, "Heartbeat: failed to open Redis client, disabling heartbeat");
                return;
            }
        };

        let mut interval = tokio::time::interval(TICK_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let payload = HeartbeatPayload {
                        id: config.worker_id.clone(),
                        started_at,
                        last_seen: Utc::now(),
                        in_flight: in_flight.current(),
                        max_concurrency: config.max_concurrency,
                        fairness_mode: config.fairness_mode.clone(),
                        sandbox_backend: config.sandbox_backend.clone(),
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        hostname: config.system_info.hostname.clone(),
                        ip_addresses: config.system_info.ip_addresses.clone(),
                        os: Some(config.system_info.os.clone()),
                        arch: Some(config.system_info.arch.clone()),
                        cpu_count: Some(config.system_info.cpu_count),
                        pid: Some(config.system_info.pid),
                    };
                    if let Err(e) = write_heartbeat(&client, &conn, &key, &payload).await {
                        warn!(error = %e, "Heartbeat write failed");
                    }
                }
                _ = &mut cancel_rx => {
                    if let Err(e) = clear_heartbeat(&client, &conn, &key).await {
                        warn!(error = %e, "Heartbeat clear on shutdown failed");
                    } else {
                        info!(worker_id = %config.worker_id, "Heartbeat cleared on shutdown");
                    }
                    return;
                }
            }
        }
    });

    HeartbeatHandle {
        cancel: Some(cancel_tx),
        join: Some(join),
    }
}

async fn get_conn(
    client: &redis::Client,
    cell: &Mutex<Option<MultiplexedConnection>>,
) -> Result<MultiplexedConnection, redis::RedisError> {
    let mut guard = cell.lock().await;
    if let Some(ref conn) = *guard {
        return Ok(conn.clone());
    }
    let conn = client.get_multiplexed_async_connection().await?;
    *guard = Some(conn.clone());
    Ok(conn)
}

async fn invalidate(cell: &Mutex<Option<MultiplexedConnection>>) {
    let mut guard = cell.lock().await;
    *guard = None;
}

async fn write_heartbeat(
    client: &redis::Client,
    cell: &Mutex<Option<MultiplexedConnection>>,
    key: &str,
    payload: &HeartbeatPayload,
) -> Result<(), redis::RedisError> {
    let body = serde_json::to_string(payload)
        .map_err(|e| redis::RedisError::from((redis::ErrorKind::Client, "json", e.to_string())))?;
    let mut conn = match get_conn(client, cell).await {
        Ok(c) => c,
        Err(e) => {
            invalidate(cell).await;
            return Err(e);
        }
    };
    let result: Result<(), redis::RedisError> = redis::cmd("SET")
        .arg(key)
        .arg(body)
        .arg("EX")
        .arg(KEY_TTL_SECS)
        .query_async(&mut conn)
        .await;
    if result.is_err() {
        invalidate(cell).await;
    }
    result
}

async fn clear_heartbeat(
    client: &redis::Client,
    cell: &Mutex<Option<MultiplexedConnection>>,
    key: &str,
) -> Result<(), redis::RedisError> {
    let mut conn = get_conn(client, cell).await?;
    let _: i64 = conn.del(key).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_flight_guard_increments_and_decrements() {
        let counter = InFlightCounter::new();
        assert_eq!(counter.current(), 0);
        let g1 = counter.guard();
        assert_eq!(counter.current(), 1);
        let g2 = counter.guard();
        assert_eq!(counter.current(), 2);
        drop(g1);
        assert_eq!(counter.current(), 1);
        drop(g2);
        assert_eq!(counter.current(), 0);
    }

    #[test]
    fn payload_serializes_with_iso_timestamps() {
        let payload = HeartbeatPayload {
            id: "worker-test".into(),
            started_at: Utc::now(),
            last_seen: Utc::now(),
            in_flight: 3,
            max_concurrency: Some(8),
            fairness_mode: "pinned".into(),
            sandbox_backend: "isolate".into(),
            version: "0.1.0".into(),
            hostname: Some("lab-pc-01".into()),
            ip_addresses: vec!["192.168.1.10".into()],
            os: Some("linux".into()),
            arch: Some("x86_64".into()),
            cpu_count: Some(8),
            pid: Some(4242),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("\"id\":\"worker-test\""));
        assert!(json.contains("\"in_flight\":3"));
        assert!(json.contains("\"max_concurrency\":8"));
        assert!(json.contains("\"fairness_mode\":\"pinned\""));
        assert!(json.contains("\"sandbox_backend\":\"isolate\""));
        assert!(json.contains("\"hostname\":\"lab-pc-01\""));
        assert!(json.contains("\"ip_addresses\":[\"192.168.1.10\"]"));
        assert!(json.contains("\"os\":\"linux\""));
    }

    #[test]
    fn payload_deserializes_legacy_without_system_fields() {
        let legacy = r#"{
            "id":"old-worker",
            "started_at":"2026-05-01T00:00:00Z",
            "last_seen":"2026-05-01T00:00:05Z",
            "in_flight":0,
            "max_concurrency":null,
            "sandbox_backend":"isolate",
            "version":"0.1.0"
        }"#;
        let p: HeartbeatPayload = serde_json::from_str(legacy).expect("legacy payload");
        assert_eq!(p.id, "old-worker");
        assert!(p.hostname.is_none());
        assert!(p.ip_addresses.is_empty());
        assert!(p.os.is_none());
        assert!(p.cpu_count.is_none());
        assert_eq!(p.fairness_mode, "unknown");
    }
}
