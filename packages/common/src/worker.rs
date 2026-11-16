use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// An operation task as it travels from a server to a worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub task_type: String,
    pub executor_name: String,
    pub payload: serde_json::Value,
    /// Id of the server waiting for the replies; the queue derives its reply
    /// stream from it.
    pub reply_to: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_batch_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<String>,
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
}

impl TaskResult {
    pub fn attach_worker_metadata(&mut self, task: &Task, worker_id: &str) {
        self.task_type = Some(task.task_type.clone());
        self.operation = Some(task.executor_name.clone());
        self.worker_id = Some(worker_id.to_string());
    }
}

/// Sent when a worker starts executing an operation task. The server times
/// the start by its own clock on receipt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStartedNotification {
    pub task_id: String,
}

/// What a worker tells the waiting server about a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Executor: Send + Sync {
    fn if_accept(&self, _task_type: &str) -> bool;
    async fn execute(&self, task: Task) -> Result<TaskResult>;
}

/// Database connections a worker with `max_concurrency` sandbox slots holds:
/// one per slot plus two for the task cache and heartbeats.
pub fn database_connections(max_concurrency: u32) -> u32 {
    max_concurrency.saturating_add(2)
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
        };

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!({
                "task_id": "op-1",
                "success": true,
                "output": { "ok": true },
                "task_type": "operation",
                "operation": "operation",
                "worker_id": "worker-a"
            })
        );
    }
}
