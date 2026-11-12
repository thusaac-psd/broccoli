use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub task_type: String,
    pub executor_name: String,
    pub payload: serde_json::Value,
    pub result_queue: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_queue: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_at_unix_ms: Option<i64>,
}

impl Task {
    pub fn reply_queue_name(&self) -> &str {
        self.reply_queue.as_deref().unwrap_or(&self.result_queue)
    }
}

/// The per-worker private operation queue name.
///
/// A worker subscribes to both the shared operation queue and this private
/// queue so the coordinator can pin a task to a specific worker (target-worker
/// routing). The worker (which consumes it) and the server (which routes pinned
/// tasks onto it) MUST derive the name identically, so the `:worker:`
/// convention lives here as the single source of truth to keep the two sides
/// from drifting.
pub fn worker_private_queue_name(shared_queue: &str, worker_id: &str) -> String {
    format!("{shared_queue}:worker:{worker_id}")
}

/// Redis key prefix for a worker's liveness heartbeat: the full key is
/// `{WORKER_HEARTBEAT_KEY_PREFIX}{worker_id}` (15s TTL, written by the worker;
/// it is also the worker's ID claim, see [`crate::node_identity`]).
///
/// This is the coupling between the worker (which WRITES its heartbeat) and the
/// server (which READS it for the system dashboard, the dead-worker operation
/// reaper, and pinned-task liveness routing) plus the worker-side dedup claim
/// (which checks a prior owner's heartbeat). Every one of those sites must use
/// the identical prefix, so it lives here as the single source of truth rather
/// than as a hand-copied literal in each crate.
pub use crate::node_identity::WORKER_HEARTBEAT_KEY_PREFIX;

/// TTL (seconds) the worker sets on its heartbeat key, refreshing it every few
/// seconds. Readers derive a heartbeat's age from the key's remaining TTL, so
/// liveness is measured on the Redis clock alone and never compares a
/// worker's clock with a server's.
pub const WORKER_HEARTBEAT_TTL_SECS: u64 = 15;

fn default_fairness_mode() -> String {
    "unknown".into()
}

/// The worker liveness heartbeat wire payload, serialized by the worker into
/// `{WORKER_HEARTBEAT_KEY_PREFIX}{id}` and deserialized by the server (system
/// dashboard, dead-worker operation reaper). It lives here as the single source
/// of truth so the producer and consumer cannot drift: previously each crate
/// hand-mirrored this struct and they had already diverged (`fairness_mode` was
/// `String` on one side and `Option<String>` on the other). All the trailing
/// fields are `#[serde(default)]` so heartbeats written by older workers still
/// deserialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    pub id: String,
    pub started_at: chrono::DateTime<Utc>,
    pub last_seen: chrono::DateTime<Utc>,
    pub in_flight: u32,
    pub max_concurrency: Option<u32>,
    #[serde(default = "default_fairness_mode")]
    pub fairness_mode: String,
    pub sandbox_backend: String,
    pub version: String,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub ip_addresses: Vec<String>,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub cpu_count: Option<u32>,
    #[serde(default)]
    pub pid: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::{Task, worker_private_queue_name};

    #[test]
    fn worker_private_queue_name_matches_the_shared_convention() {
        assert_eq!(
            worker_private_queue_name("operation_tasks", "worker-a"),
            "operation_tasks:worker:worker-a"
        );
    }

    #[test]
    fn reply_queue_defaults_to_result_queue_for_legacy_envelopes() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "task-1",
            "task_type": "operation",
            "executor_name": "operation",
            "payload": {},
            "result_queue": "operation_results.legacy"
        }))
        .unwrap();

        assert_eq!(task.enqueued_at_unix_ms, None);
        assert_eq!(task.reply_queue, None);
        assert_eq!(task.reply_queue_name(), "operation_results.legacy");
    }

    #[test]
    fn reply_queue_overrides_result_queue_when_present() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "task-1",
            "task_type": "operation",
            "executor_name": "operation",
            "payload": {},
            "result_queue": "operation_results",
            "reply_queue": "operation_results.replica-a"
        }))
        .unwrap();

        assert_eq!(task.reply_queue_name(), "operation_results.replica-a");
    }

    #[test]
    fn operation_batch_id_round_trips_when_present() {
        let task: Task = serde_json::from_value(serde_json::json!({
            "id": "task-1",
            "task_type": "operation",
            "executor_name": "operation",
            "payload": {},
            "result_queue": "operation_results",
            "operation_batch_id": "batch-1"
        }))
        .unwrap();

        assert_eq!(task.operation_batch_id.as_deref(), Some("batch-1"));

        let json = serde_json::to_value(&task).unwrap();
        assert_eq!(json["operation_batch_id"], "batch-1");
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub success: bool,
    pub output: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_at_unix_ms: Option<i64>,
}

impl TaskResult {
    pub fn attach_worker_metadata(&mut self, task: &Task, worker_id: &str) {
        self.task_type = Some(task.task_type.clone());
        self.operation = Some(task.executor_name.clone());
        self.worker_id = Some(worker_id.to_string());
        self.enqueued_at_unix_ms = task.enqueued_at_unix_ms;
    }
}

/// Sent when a worker starts executing an operation task. The server times
/// the start by its own clock on receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStartedNotification {
    pub task_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskReply {
    Started {
        #[serde(flatten)]
        notification: TaskStartedNotification,
    },
    Completed {
        result: TaskResult,
    },
}

impl<'de> Deserialize<'de> for TaskReply {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum TaggedTaskReply {
            Started {
                #[serde(flatten)]
                notification: TaskStartedNotification,
            },
            Completed {
                result: TaskResult,
            },
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireTaskReply {
            Tagged(TaggedTaskReply),
            Legacy(TaskResult),
        }

        Ok(match WireTaskReply::deserialize(deserializer)? {
            WireTaskReply::Tagged(TaggedTaskReply::Started { notification }) => {
                TaskReply::Started { notification }
            }
            WireTaskReply::Tagged(TaggedTaskReply::Completed { result }) => {
                TaskReply::Completed { result }
            }
            WireTaskReply::Legacy(result) => TaskReply::Completed { result },
        })
    }
}

#[cfg(test)]
mod reply_tests {
    use super::{TaskReply, TaskResult, TaskStartedNotification};

    #[test]
    fn task_reply_serializes_started_notification() {
        let reply = TaskReply::Started {
            notification: TaskStartedNotification {
                task_id: "op-1".to_string(),
            },
        };

        assert_eq!(
            serde_json::to_value(reply).unwrap(),
            serde_json::json!({
                "type": "started",
                "task_id": "op-1"
            })
        );
    }

    #[test]
    fn task_reply_serializes_completed_result() {
        let reply = TaskReply::Completed {
            result: TaskResult {
                task_id: "op-1".to_string(),
                success: true,
                output: serde_json::json!({ "ok": true }),
                error: None,
                task_type: None,
                operation: None,
                worker_id: None,
                enqueued_at_unix_ms: None,
            },
        };

        assert_eq!(
            serde_json::to_value(reply).unwrap(),
            serde_json::json!({
                "type": "completed",
                "result": {
                    "task_id": "op-1",
                    "success": true,
                    "output": { "ok": true }
                }
            })
        );
    }

    #[test]
    fn task_result_metadata_serializes_when_present() {
        let result = TaskResult {
            task_id: "op-1".to_string(),
            success: true,
            output: serde_json::json!({ "ok": true }),
            error: None,
            task_type: Some("operation".to_string()),
            operation: Some("operation".to_string()),
            worker_id: Some("worker-a".to_string()),
            enqueued_at_unix_ms: Some(1_234),
        };

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "task_id": "op-1",
                "success": true,
                "output": { "ok": true },
                "task_type": "operation",
                "operation": "operation",
                "worker_id": "worker-a",
                "enqueued_at_unix_ms": 1_234
            })
        );
    }

    #[test]
    fn task_reply_deserializes_legacy_bare_result() {
        let reply: TaskReply = serde_json::from_value(serde_json::json!({
            "task_id": "op-1",
            "success": true,
            "output": { "ok": true }
        }))
        .unwrap();

        match reply {
            TaskReply::Completed { result } => {
                assert_eq!(result.task_id, "op-1");
                assert!(result.success);
            }
            TaskReply::Started { .. } => panic!("legacy result should decode as completed"),
        }
    }
}

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Executor: Send + Sync {
    fn if_accept(&self, _task_type: &str) -> bool;
    async fn execute(&self, task: Task) -> Result<TaskResult>;
}
