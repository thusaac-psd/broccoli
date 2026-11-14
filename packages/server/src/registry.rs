use broccoli_server_sdk::types::TestCaseVerdict;
use common::worker::TaskResult;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::{RwLock, oneshot};

use crate::services::batch_table::BatchTable;
use crate::services::evaluate_batch::EvaluateCases;

/// The plugin that evaluates test cases of one problem type, and how.
#[derive(Clone, Debug)]
pub struct EvaluatorHandler {
    pub plugin_id: String,
    pub functions: EvaluatorFunctions,
}

/// The functions an evaluator declared when it registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluatorFunctions {
    /// One call evaluates a test case end to end; the plugin starts and
    /// awaits its own operations.
    Direct { function: String },
    /// The host drives each test case: `prepare` returns its operations, the
    /// host runs them without holding a plugin instance, and `interpret`
    /// turns batches of finished cases into verdicts.
    Staged { prepare: String, interpret: String },
}

impl EvaluatorFunctions {
    /// The function called first for each case: the direct function, or
    /// `prepare`. Used to label logs and metrics.
    pub fn entry(&self) -> &str {
        match self {
            Self::Direct { function } => function,
            Self::Staged { prepare, .. } => prepare,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContestTypeHandlers {
    pub plugin_id: String,
    pub submission_fn: String,
    pub code_run_fn: String,
    pub submission_admission: bool,
}

pub type ContestTypeRegistry = Arc<RwLock<HashMap<String, ContestTypeHandlers>>>;

pub type EvaluatorRegistry = Arc<RwLock<HashMap<String, EvaluatorHandler>>>;

/// The resolve + interpret functions a checker plugin registers for a format
/// under checker fusion. `resolve_fn` returns a `CheckerStage` to splice into the
/// run op; `interpret_fn` turns the check step's small result into a
/// `CheckerVerdict`. Bundled (like [`ContestTypeHandlers`]) so a format registers
/// once and both host fns share one lookup.
#[derive(Clone, Debug)]
pub struct CheckerStageHandlers {
    pub plugin_id: String,
    pub resolve_fn: String,
    pub interpret_fn: String,
}

pub type CheckerStageRegistry = Arc<RwLock<HashMap<String, CheckerStageHandlers>>>;

#[derive(Clone, Debug)]
pub struct LanguageResolverEntry {
    pub plugin_id: String,
    pub function_name: String,
    pub display_name: String,
    pub default_filename: String,
    pub extensions: Vec<String>,
    pub template: String,
}

pub type LanguageResolverRegistry = Arc<RwLock<HashMap<String, LanguageResolverEntry>>>;

/// Live operation batches; each batch's state is its waiters' correlation
/// ids.
pub type OperationBatches = BatchTable<TaskResult, Vec<String>>;

/// Live evaluate batches with their per-test-case progress.
pub type EvaluateBatches = BatchTable<TestCaseVerdict, EvaluateCases>;

/// The evaluate test case an operation runs for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvaluateCaseRef {
    pub batch_id: String,
    pub test_case_id: i32,
}

/// Waits for one operation's result. The result consumer removes the waiter
/// from [`OperationWaiters`] to deliver, so it owns the sender outright.
pub struct OperationWaiter {
    pub result_tx: oneshot::Sender<TaskResult>,
    /// Set when the operation runs for an evaluate test case, so its start
    /// can be recorded on that case.
    pub evaluate_case: Option<EvaluateCaseRef>,
    /// When the operation was registered, just before its publish.
    pub published_at: Instant,
    /// Set once the publish returned: from then on the queue holds the task
    /// (or its reply), so the result consumer may check that it still does
    /// (see `consumers::operation_result`). Before, the task may not be in
    /// the queue yet.
    pub published: bool,
}

impl OperationWaiter {
    pub fn new(
        result_tx: oneshot::Sender<TaskResult>,
        evaluate_case: Option<EvaluateCaseRef>,
    ) -> Self {
        Self {
            result_tx,
            evaluate_case,
            published_at: Instant::now(),
            published: false,
        }
    }
}

pub type OperationWaiters = Arc<DashMap<String, OperationWaiter>>;
