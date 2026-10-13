use serde::{Deserialize, Serialize};

/// Optional `submission_admission` payload on a modified before-submission
/// event. The host commits the state guard, submission and pending feedback
/// together, after all hooks return. No database connection spans a WASM call.
/// Guarded state documents must preserve the host-managed `admission_revision`
/// and `admission_by_user` counters when writing subsequent state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmissionAdmission {
    pub state_key: String,
    pub scope_key: String,
    pub expected_state: String,
    pub delay_seconds: i64,
    pub deadline_ms: i64,
}
