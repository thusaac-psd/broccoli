use serde::{Deserialize, Serialize};

use super::evaluate::TestCaseBodyRef;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseRow {
    pub id: i32,
    pub score: f64,
    pub is_sample: bool,
    pub position: i32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub input: TestCaseBodyRef,
    #[serde(default)]
    pub expected_output: TestCaseBodyRef,
    #[serde(default)]
    pub is_custom: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCaseData {
    pub input: String,
    pub expected_output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProblemCheckerInfo {
    pub id: i32,
    pub checker_format: Option<String>,
}
