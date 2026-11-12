use broccoli_server_sdk::types::TestCaseVerdict;
use common::worker::TaskResult;
use dashmap::DashMap;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, oneshot};

use crate::services::batch_table::BatchTable;
use crate::services::evaluate_batch::EvaluateCases;

#[derive(Clone, Debug)]
pub struct PluginHandler {
    pub plugin_id: String,
    pub function_name: String,
}

#[derive(Clone, Debug)]
pub struct ContestTypeHandlers {
    pub plugin_id: String,
    pub submission_fn: String,
    pub code_run_fn: String,
    pub submission_admission: bool,
}

pub type ContestTypeRegistry = Arc<RwLock<HashMap<String, ContestTypeHandlers>>>;

pub type EvaluatorRegistry = Arc<RwLock<HashMap<String, PluginHandler>>>;

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
}

pub type OperationWaiters = Arc<DashMap<String, OperationWaiter>>;
