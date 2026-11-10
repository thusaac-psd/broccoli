use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::Utc;
use common::node_identity::{NodeClaim, NodeHeartbeat};

// The heartbeat wire payload is defined once in `common` so the worker (producer)
// and the server (consumer) cannot drift. See [`common::worker::HeartbeatPayload`].
use common::worker::HeartbeatPayload;

use crate::system_info::SystemInfo;

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
    pub worker_id: String,
    pub sandbox_backend: String,
    pub max_concurrency: Option<u32>,
    pub fairness_mode: String,
    pub system_info: SystemInfo,
}

pub struct HeartbeatHandle(NodeHeartbeat);

impl HeartbeatHandle {
    /// Stop heartbeating and release the worker's ID claim.
    pub async fn shutdown(&mut self) {
        self.0.shutdown().await;
    }
}

impl HeartbeatConfig {
    /// The heartbeat payload as of now.
    pub fn payload(
        &self,
        started_at: chrono::DateTime<Utc>,
        in_flight: &InFlightCounter,
    ) -> serde_json::Value {
        let payload = HeartbeatPayload {
            id: self.worker_id.clone(),
            started_at,
            last_seen: Utc::now(),
            in_flight: in_flight.current(),
            max_concurrency: self.max_concurrency,
            fairness_mode: self.fairness_mode.clone(),
            sandbox_backend: self.sandbox_backend.clone(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            hostname: self.system_info.hostname.clone(),
            ip_addresses: self.system_info.ip_addresses.clone(),
            os: Some(self.system_info.os.clone()),
            arch: Some(self.system_info.arch.clone()),
            cpu_count: Some(self.system_info.cpu_count),
            pid: Some(self.system_info.pid),
        };
        serde_json::to_value(payload).unwrap_or_default()
    }
}

/// Keep the worker's heartbeat key (which is also its ID claim, taken at boot
/// with [`NodeClaim::claim_at_boot`]) alive until shutdown.
pub fn spawn(
    claim: NodeClaim,
    config: HeartbeatConfig,
    started_at: chrono::DateTime<Utc>,
    in_flight: InFlightCounter,
) -> HeartbeatHandle {
    HeartbeatHandle(claim.spawn_heartbeat(move || config.payload(started_at, &in_flight)))
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
