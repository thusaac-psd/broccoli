//! The JSON a guest and the host exchange through the config, timer,
//! storage, registry, operation, evaluate and feedback host functions. The
//! guest SDK serializes these and the host deserializes them (and the
//! reverse for outputs), so the two sides read one definition. The
//! `config_get` output is [`super::ConfigResult`].

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// `config_get` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigGetInput {
    pub scope: String,
    pub ref_id: String,
    pub namespace: String,
}

/// `config_set` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigSetInput {
    pub scope: String,
    pub ref_id: String,
    pub namespace: String,
    pub config: serde_json::Value,
}

/// `timer_schedule` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerScheduleInput {
    /// Unix epoch milliseconds.
    pub fire_at_ms: i64,
    pub key: String,
    pub payload: String,
}

/// `timer_cancel` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimerCancelInput {
    pub key: String,
}

/// `store_get` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreGetInput {
    pub keys: Vec<String>,
}

/// `store_get` output: each key found, with its value and its revision.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreGetOutput {
    pub values: HashMap<String, String>,
    pub versions: HashMap<String, i64>,
}

/// One `store_set` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSetEntry {
    pub key: String,
    pub value: String,
}

/// `store_set` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreSetInput {
    pub entries: Vec<StoreSetEntry>,
}

/// `store_delete` input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreDeleteInput {
    pub keys: Vec<String>,
}

/// `store_compare_and_set` input: replace `key`'s value with `new` if it is
/// `expected` (`None`: absent), and, when `expected_revision` is set, still
/// at that revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCasInput {
    pub key: String,
    pub expected: Option<String>,
    pub new: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<i64>,
}

/// `store_compare_and_set` output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreCasOutput {
    pub swapped: bool,
}

/// `store_scan` input: keys starting with `prefix`, after `after`. Its
/// output is a key-to-value map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreScanInput {
    pub prefix: String,
    pub after: Option<String>,
}

/// `register_contest_type` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterContestTypeInput {
    #[serde(rename = "type")]
    pub contest_type: String,
    pub submission_handler: String,
    pub code_run_handler: String,
    /// The plugin decides admission of each submission of the type.
    #[serde(default)]
    pub submission_admission: bool,
}

/// `register_evaluator` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterEvaluatorInput {
    #[serde(rename = "type")]
    pub problem_type: String,
    #[serde(flatten)]
    pub handlers: EvaluatorHandlers,
}

/// The functions an evaluator registers: one direct `handler`, or a staged
/// `prepare_handler` and `interpret_handler` pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EvaluatorHandlers {
    Staged {
        prepare_handler: String,
        interpret_handler: String,
    },
    Direct {
        handler: String,
    },
}

/// `register_checker_resolver` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterCheckerResolverInput {
    #[serde(rename = "format")]
    pub checker_format: String,
    pub resolve_handler: String,
    pub interpret_handler: String,
}

/// `register_language_resolver` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegisterLanguageResolverInput {
    pub language_id: String,
    pub function_name: String,
    /// Empty: the language id is shown.
    #[serde(default)]
    pub display_name: String,
    #[serde(default = "default_source_filename")]
    pub default_filename: String,
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub template: String,
}

fn default_source_filename() -> String {
    "solution.txt".to_string()
}

/// A batch of operations or evaluations: the `start_operation_batch` and
/// `start_evaluate_batch` output, and the `cancel_operation_batch` and
/// `cancel_evaluate_batch` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchId {
    pub batch_id: String,
}

/// `get_next_operation_result` and `get_next_evaluate_result` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NextResultInput {
    pub batch_id: String,
    /// How long to wait for a result, bounded by the call's deadline.
    pub timeout_ms: u64,
}

/// `get_next_operation_result` and `get_next_evaluate_result` output: the
/// next result, or `None` when none came within the wait. An operation's
/// result is the output the worker reported for it; an evaluation's is its
/// [`super::TestCaseVerdict`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NextResultOutput<T> {
    pub result: Option<T>,
}

/// `cancel_evaluate_test_cases` input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelTestCasesInput {
    pub evaluate_batch_id: String,
    pub test_case_ids: Vec<i32>,
}

/// `cancel_evaluate_test_cases` output: how many cases were cancelled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CancelTestCasesOutput {
    pub count: usize,
}

/// `submission_feedback_update` input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackUpdateInput {
    pub submission_id: i32,
    pub action: super::SubmissionFeedbackAction,
}
