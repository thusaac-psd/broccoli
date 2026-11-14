//! Problem, test case and sample types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateProblemRequest {
    #[schema(example = "Two Sum")]
    pub title: String,
    #[schema(example = "Given an array of integers `nums` and an integer `target`...")]
    pub content: String,
    #[schema(example = 1000)]
    pub time_limit: i32,
    #[schema(example = 262144)]
    pub memory_limit: i32,
    #[schema(example = "batch")]
    pub problem_type: String,
    /// How a solution's output is compared with the expected output.
    /// Defaults to `lines`.
    #[serde(default = "default_checker_format")]
    #[schema(example = "lines")]
    pub checker_format: String,
    /// Contest type that judges submissions made outside any contest.
    /// Required: the server never picks one, because any "first registered"
    /// choice depends on plugin names and can land on a tournament format that
    /// makes no sense for practice.
    #[schema(example = "ioi")]
    pub default_contest_type: String,
    #[schema(example = false)]
    pub show_test_details: Option<bool>,
    #[schema(example = false)]
    pub is_public: Option<bool>,
    #[schema(example = json!({"cpp": ["solution.cpp"], "java": ["Main.java"]}))]
    pub submission_format: Option<std::collections::HashMap<String, Vec<String>>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProblemResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "Two Sum")]
    pub title: String,
    #[schema(example = "Given an array of integers...")]
    pub content: String,
    #[schema(example = 1000)]
    pub time_limit: i32,
    #[schema(example = 262144)]
    pub memory_limit: i32,
    #[schema(example = "batch")]
    pub problem_type: String,
    #[schema(example = "lines")]
    pub checker_format: String,
    #[schema(example = "ioi")]
    pub default_contest_type: String,
    #[schema(example = false)]
    pub show_test_details: bool,
    #[schema(example = false)]
    pub is_public: bool,
    #[schema(example = json!({"cpp": ["solution.cpp"], "java": ["Main.java"]}))]
    pub submission_format: Option<std::collections::HashMap<String, Vec<String>>>,
    pub samples: Vec<SampleTestCaseMeta>,
    #[schema(example = "2025-09-01T08:00:00Z")]
    pub created_at: DateTime<Utc>,
    #[schema(example = "2025-09-01T08:30:00Z")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SampleTestCaseMeta {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = 12)]
    pub input_size: usize,
    #[schema(example = 4)]
    pub output_size: usize,
    #[schema(example = "The sample demonstrates the smallest non-empty input.")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateTestCaseRequest {
    #[schema(example = "4\n2 7 11 15\n9")]
    pub input: String,
    #[schema(example = "0 1")]
    pub expected_output: String,
    #[schema(example = 10)]
    pub score: i32,
    #[schema(example = true)]
    pub is_sample: bool,
    #[schema(example = 0)]
    pub position: Option<i32>,
    #[schema(example = "Basic case")]
    pub description: Option<String>,
    #[schema(value_type = Option<String>, example = "sample_01")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TestCaseResponse {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "4\n2 7 11 15\n9")]
    pub input: String,
    #[schema(example = "0 1")]
    pub expected_output: String,
    #[schema(example = 12)]
    pub input_size: usize,
    #[schema(example = 4)]
    pub output_size: usize,
    #[schema(example = "4\n2 7 11 15\n9")]
    pub input_preview: String,
    #[schema(example = "0 1")]
    pub output_preview: String,
    #[schema(example = 10)]
    pub score: i32,
    #[schema(example = "Basic case")]
    pub description: Option<String>,
    #[schema(example = "sample_01")]
    pub label: String,
    #[schema(example = true)]
    pub is_sample: bool,
    #[schema(example = 0)]
    pub position: i32,
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "2025-09-01T09:00:00Z")]
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[cfg_attr(feature = "sea-orm", derive(sea_orm::FromQueryResult))]
pub struct TestCaseListItem {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = 10)]
    pub score: i32,
    #[schema(example = "Basic case")]
    pub description: Option<String>,
    #[schema(example = "sample_01")]
    pub label: String,
    #[schema(example = true)]
    pub is_sample: bool,
    #[schema(example = 0)]
    pub position: i32,
    #[schema(example = "4\n2 7 11 15\n9")]
    pub input_preview: String,
    #[schema(example = "0 1")]
    pub output_preview: String,
    #[schema(example = 1)]
    pub problem_id: i32,
    #[schema(example = "2025-09-01T09:00:00Z")]
    pub created_at: DateTime<Utc>,
}

/// The checker format of a problem that names none: line by line, ignoring
/// trailing whitespace on each line and trailing blank lines, so an answer
/// typed without a final newline matches `print(3)`. `exact` compares bytes.
pub const DEFAULT_CHECKER_FORMAT: &str = "lines";

fn default_checker_format() -> String {
    DEFAULT_CHECKER_FORMAT.into()
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProblemSamplesResponse {
    pub samples: Vec<SampleCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SampleCase {
    pub input: String,
    pub output: String,
    /// Optional markdown note explaining this sample (author-written).
    pub description: Option<String>,
}
