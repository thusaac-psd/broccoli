//! Dead-letter queue statistics.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct MessageTypeCounts {
    #[schema(example = 1)]
    pub operation_task: u64,
    #[schema(example = 3)]
    pub stuck_submission: u64,
    #[schema(example = 1)]
    pub stuck_code_run: u64,
    #[schema(example = 1)]
    pub stuck_submission_judgement: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DlqStatsResponse {
    #[schema(example = 5)]
    pub total_unresolved: u64,
    #[schema(example = 42)]
    pub total_resolved: u64,
    pub unresolved_by_message_type: MessageTypeCounts,
    pub unresolved_by_error_code: HashMap<String, u64>,
}
