//! Code-run (custom test) types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::submission::SubmissionFileDto;
use crate::{SubmissionStatus, Verdict};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CustomTestCaseInput {
    pub input: String,
    pub expected_output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunCodeRequest {
    pub files: Vec<SubmissionFileDto>,
    #[schema(example = "cpp")]
    pub language: String,
    pub custom_test_cases: Vec<CustomTestCaseInput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CodeRunResponse {
    #[schema(example = 1)]
    pub id: i32,
    pub files: Vec<SubmissionFileDto>,
    #[schema(example = "cpp")]
    pub language: String,
    pub status: SubmissionStatus,
    #[schema(example = 1)]
    pub user_id: i32,
    #[schema(example = "alice")]
    pub username: String,
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "Two Sum")]
    pub problem_title: String,
    #[schema(example = 1)]
    pub contest_id: Option<i32>,
    #[schema(example = "ioi")]
    pub contest_type: String,
    pub custom_test_cases: Vec<CustomTestCaseInput>,
    #[schema(example = "2025-10-01T14:30:00Z")]
    pub created_at: DateTime<Utc>,
    pub result: Option<CodeRunJudgeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CodeRunJudgeResult {
    #[schema(value_type = Option<String>, example = "Accepted")]
    pub verdict: Option<Verdict>,
    #[schema(example = 2.0)]
    pub score: Option<f64>,
    #[schema(example = 50)]
    pub time_used: Option<i32>,
    #[schema(example = 1024)]
    pub memory_used: Option<i32>,
    pub compile_output: Option<String>,
    pub error_message: Option<String>,
    pub judged_at: Option<DateTime<Utc>>,
    pub test_case_results: Vec<CodeRunResultResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CodeRunResultResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(value_type = String, example = "Accepted")]
    pub verdict: Verdict,
    #[schema(example = 1.0)]
    pub score: f64,
    #[schema(example = 5)]
    pub time_used: Option<i32>,
    #[schema(example = 256)]
    pub memory_used: Option<i32>,
    pub run_index: i32,
    pub input: Option<String>,
    pub expected_output: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub checker_output: Option<String>,
}
