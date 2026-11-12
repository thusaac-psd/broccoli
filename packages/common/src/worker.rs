use crate::event::Event;
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

/// The payload a worker writes into its liveness key (see
/// [`crate::liveness`]) and the server reads for the system dashboard and
/// capacity planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    pub id: String,
    pub started_at: chrono::DateTime<Utc>,
    pub last_seen: chrono::DateTime<Utc>,
    pub in_flight: u32,
    pub max_concurrency: Option<u32>,
    pub fairness_mode: String,
    pub sandbox_backend: String,
    pub version: String,
    pub hostname: Option<String>,
    pub ip_addresses: Vec<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub cpu_count: Option<u32>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStartedNotification {
    pub task_id: String,
    #[serde(default = "current_unix_ms")]
    pub started_at_unix_ms: i64,
}

fn current_unix_ms() -> i64 {
    Utc::now().timestamp_millis()
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
                started_at_unix_ms: 1_234,
            },
        };

        assert_eq!(
            serde_json::to_value(reply).unwrap(),
            serde_json::json!({
                "type": "started",
                "task_id": "op-1",
                "started_at_unix_ms": 1_234
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TaskEvent {
    Started { task: Task },
    Completed { result: TaskResult },
    Failed { task: Task, error: String },
}

impl Event for TaskEvent {
    fn topic(&self) -> &str {
        match self {
            TaskEvent::Started { .. } => "task_started",
            TaskEvent::Completed { .. } => "task_completed",
            TaskEvent::Failed { .. } => "task_failed",
        }
    }
}

#[async_trait]
pub trait Executor: Send + Sync {
    fn if_accept(&self, _task_type: &str) -> bool;
    async fn execute(&self, task: Task) -> Result<TaskResult>;
}
