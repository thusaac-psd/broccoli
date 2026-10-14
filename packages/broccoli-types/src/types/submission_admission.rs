use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Optional atomic effects supplied by the contest owner after admission hooks.
/// Storage values and feedback metadata are opaque to the host.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionAdmission {
    pub guard: SubmissionStorageGuard,
    pub feedback_key: String,
    pub metadata: Value,
    pub slot: Option<SubmissionSlot>,
    /// Optional exclusive cutoff for actual arrival, independent of plugin scoring metadata.
    pub latest_arrival_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionStorageGuard {
    pub key: String,
    pub expected_value: String,
}

/// A plugin chooses the namespace and scope of an exclusive submission slot.
/// The generation rejects stale hooks even after a previous slot is released.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionSlot {
    pub key: String,
    pub generation: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionFeedbackResult {
    pub verdict: super::Verdict,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionFeedbackPublication {
    pub withheld: bool,
    pub release_slot: bool,
    pub result: Option<SubmissionFeedbackResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubmissionFeedbackQuery {
    pub key: Option<String>,
    pub slot_key: Option<String>,
    pub submission_ids: Option<Vec<i32>>,
    pub unpublished_only: bool,
    pub after_id: i32,
    /// Maximum records to return; zero lets the SDK read all pages.
    pub limit: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubmissionFeedbackRecord {
    pub submission_id: i32,
    pub contest_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    pub submitted_at_us: i64,
    pub status: String,
    pub verdict: Option<super::Verdict>,
    pub key: String,
    pub metadata: Value,
    pub withheld: bool,
    pub slot_held: bool,
    pub slot_key: Option<String>,
    pub result: Option<SubmissionFeedbackResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSnapshot {
    pub value: String,
    pub revision: i64,
}
