use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkerInfo {
    #[schema(example = "worker-1")]
    pub id: String,
    pub started_at: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Seconds since the worker last renewed its liveness key, measured on the
    /// Redis clock. 0 means just now.
    #[schema(example = 3)]
    pub seconds_since_last_seen: u64,
    #[schema(example = 0)]
    pub in_flight: u32,
    pub max_concurrency: Option<u32>,
    /// The worker's fairness mode (e.g. `pinned`, `cooperative`, `unknown`),
    /// reported in its heartbeat. See the worker `fairness` module.
    #[schema(example = "pinned")]
    pub fairness_mode: String,
    #[schema(example = "isolate")]
    pub sandbox_backend: String,
    #[schema(example = "0.1.0")]
    pub version: String,
    /// OS hostname of the machine running this worker. Useful for identifying
    /// physical machines in a lab.
    #[schema(example = "lab-pc-07")]
    pub hostname: Option<String>,
    /// Non-loopback IPv4/IPv6 addresses bound to this worker's interfaces.
    #[serde(default)]
    pub ip_addresses: Vec<String>,
    /// Operating system family (e.g. `linux`, `macos`).
    #[schema(example = "linux")]
    pub os: Option<String>,
    /// CPU architecture (e.g. `x86_64`, `aarch64`).
    #[schema(example = "x86_64")]
    pub arch: Option<String>,
    /// Number of logical CPUs visible to the worker process.
    #[schema(example = 8)]
    pub cpu_count: Option<u32>,
    /// Worker process ID on its host machine.
    #[schema(example = 4242)]
    pub pid: Option<u32>,
    /// Platform tools the worker lacks as of its last heartbeat. Empty on a
    /// healthy worker.
    #[schema(example = json!([]))]
    pub missing_platform_tools: Vec<String>,
    /// Whether the worker takes judging work. A worker that lacks a platform
    /// tool (`missing_platform_tools`) takes none until the tool is in place;
    /// while no live worker takes work, submissions wait `Queued`.
    #[schema(example = true)]
    pub accepting_work: bool,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct WorkersResponse {
    /// Whether the worker list below was read just now.
    pub queue_state: QueueState,
    /// The live workers. While the queue is unreachable: the list this server
    /// last read, and `workers_stale` is true.
    pub workers: Vec<WorkerInfo>,
    /// `workers` is the last list read before the queue became unreachable.
    pub workers_stale: bool,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct QueueInfo {
    #[schema(example = "operation_tasks")]
    pub name: String,
    /// Entries in the stream: unread plus pending.
    #[schema(example = 0)]
    pub depth: u64,
    /// `unread` (no consumer has taken them) and `pending` (taken, not yet
    /// acknowledged), e.g. `{"unread": 0, "pending": 0}`.
    pub breakdown: std::collections::HashMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct QueuesResponse {
    pub queues: Vec<QueueInfo>,
}

/// Whether this server reached the message queue (Redis) for a response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueState {
    Reachable,
    Unreachable,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SystemOverviewResponse {
    /// Whether the worker list and queue depths below were read just now.
    pub queue_state: QueueState,
    /// The live workers. While the queue is unreachable: the list this server
    /// last read, and `workers_stale` is true.
    pub workers: Vec<WorkerInfo>,
    /// `workers` is the last list read before the queue became unreachable.
    pub workers_stale: bool,
    /// Queue depths; empty while the queue is unreachable.
    pub queues: Vec<QueueInfo>,
    /// Submissions currently in a non-terminal state (Pending, Compiling, Running).
    #[schema(example = 0)]
    pub submissions_in_progress: u64,
    /// DLQ messages with `resolved = false`.
    #[schema(example = 0)]
    pub dlq_unresolved_count: u64,
}
