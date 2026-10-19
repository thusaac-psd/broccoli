use serde::{Deserialize, Serialize};

/// A durable, idempotent message addressed to contestants by its owning plugin.
/// The host supplies the plugin namespace; it never interprets `payload`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublishNotification {
    pub contest_id: i32,
    pub user_ids: Vec<i32>,
    pub key: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

impl PublishNotification {
    pub fn validate(&self) -> Result<(), String> {
        if self.contest_id <= 0
            || self.user_ids.is_empty()
            || self.user_ids.len() > 32
            || self.user_ids.iter().any(|id| *id <= 0)
        {
            return Err("Notifications require a contest and 1-32 valid recipients".into());
        }
        if self.key.is_empty()
            || self.key.len() > 256
            || self.kind.is_empty()
            || self.kind.len() > 128
        {
            return Err("Notification key or kind is empty or too long".into());
        }
        if self.payload.to_string().len() > 16 * 1024 {
            return Err("Notification payload exceeds 16 KiB".into());
        }
        Ok(())
    }
}
