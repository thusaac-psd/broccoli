use broccoli_server_sdk::types::TestCaseVerdict;
use common::worker::TaskResult;
use dashmap::DashMap;
use std::collections::{BTreeMap, HashMap};
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

pub type ContestTypeRegistry = Arc<RwLock<Providers<ContestTypeHandlers>>>;

pub type EvaluatorRegistry = Arc<RwLock<Providers<EvaluatorHandler>>>;

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

pub type CheckerStageRegistry = Arc<RwLock<Providers<CheckerStageHandlers>>>;

#[derive(Clone, Debug)]
pub struct LanguageResolverEntry {
    pub plugin_id: String,
    pub function_name: String,
    pub display_name: String,
    pub default_filename: String,
    pub extensions: Vec<String>,
    pub template: String,
}

pub type LanguageResolverRegistry = Arc<RwLock<Providers<LanguageResolverEntry>>>;

/// A handler a plugin registered under a key of one of the registries.
pub trait Provided: Clone {
    /// The plugin that registered it.
    fn plugin_id(&self) -> &str;
}

macro_rules! provided {
    ($($handler:ty),*) => {$(
        impl Provided for $handler {
            fn plugin_id(&self) -> &str {
                &self.plugin_id
            }
        }
    )*};
}
provided!(
    ContestTypeHandlers,
    EvaluatorHandler,
    CheckerStageHandlers,
    LanguageResolverEntry
);

/// The handlers plugins registered, per key (a contest type, a problem
/// type, a checker format, a language). Several loaded plugins may register
/// the same key: every registration is kept, and the key resolves to the
/// one whose plugin id sorts first. The rule depends only on which plugins
/// are loaded, never on the order they loaded in, so replicas that load the
/// same plugins resolve every key alike; and unloading the plugin a key
/// resolves to hands the key to the next provider instead of dropping it.
#[derive(Debug)]
pub struct Providers<H> {
    keys: HashMap<String, BTreeMap<String, H>>,
}

impl<H> Default for Providers<H> {
    fn default() -> Self {
        Self {
            keys: HashMap::new(),
        }
    }
}

impl<H: Provided> Providers<H> {
    /// Record that `handler`'s plugin provides `key`, replacing that
    /// plugin's earlier registration of it.
    pub fn insert(&mut self, key: String, handler: H) {
        self.keys
            .entry(key)
            .or_default()
            .insert(handler.plugin_id().to_string(), handler);
    }

    /// The handler `key` resolves to.
    pub fn get(&self, key: &str) -> Option<&H> {
        self.keys.get(key)?.values().next()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.keys.contains_key(key)
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Every provided key.
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.keys.keys()
    }

    /// Every provided key with the handler it resolves to.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &H)> {
        self.keys
            .iter()
            .filter_map(|(key, providers)| Some((key, providers.values().next()?)))
    }

    /// The handler every provided key resolves to.
    pub fn values(&self) -> impl Iterator<Item = &H> {
        self.iter().map(|(_, handler)| handler)
    }

    /// What `plugin_id` registered, whether or not each key resolves to it.
    pub fn registered_by(&self, plugin_id: &str) -> Vec<(String, H)> {
        self.keys
            .iter()
            .filter_map(|(key, providers)| Some((key.clone(), providers.get(plugin_id)?.clone())))
            .collect()
    }

    /// Drop everything `plugin_id` registered.
    pub fn release(&mut self, plugin_id: &str) {
        self.keys.retain(|_, providers| {
            providers.remove(plugin_id);
            !providers.is_empty()
        });
    }
}

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
    /// The dispatch the operation works for, if any: a platform fault the
    /// operation meets is recorded there.
    pub faults: Option<crate::services::judging::DispatchFaults>,
}

impl OperationWaiter {
    pub fn new(
        result_tx: oneshot::Sender<TaskResult>,
        evaluate_case: Option<EvaluateCaseRef>,
        faults: Option<crate::services::judging::DispatchFaults>,
    ) -> Self {
        Self {
            result_tx,
            evaluate_case,
            published_at: Instant::now(),
            published: false,
            faults,
        }
    }
}

pub type OperationWaiters = Arc<DashMap<String, OperationWaiter>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(plugin_id: &str) -> ContestTypeHandlers {
        ContestTypeHandlers {
            plugin_id: plugin_id.to_string(),
            submission_fn: "submit".to_string(),
            code_run_fn: "run".to_string(),
            submission_admission: false,
        }
    }

    fn owner<'a>(providers: &'a Providers<ContestTypeHandlers>, key: &str) -> Option<&'a str> {
        providers.get(key).map(|h| h.plugin_id.as_str())
    }

    #[test]
    fn a_key_two_plugins_provide_resolves_alike_in_either_load_order() {
        let mut first = Providers::default();
        first.insert("shared".into(), handler("beta"));
        first.insert("shared".into(), handler("alpha"));
        let mut second = Providers::default();
        second.insert("shared".into(), handler("alpha"));
        second.insert("shared".into(), handler("beta"));
        assert_eq!(owner(&first, "shared"), Some("alpha"));
        assert_eq!(owner(&second, "shared"), Some("alpha"));
    }

    #[test]
    fn unloading_the_resolved_provider_hands_the_key_to_the_next() {
        let mut providers = Providers::default();
        providers.insert("shared".into(), handler("alpha"));
        providers.insert("shared".into(), handler("beta"));
        providers.insert("own".into(), handler("alpha"));

        providers.release("alpha");
        assert_eq!(owner(&providers, "shared"), Some("beta"));
        assert!(!providers.contains_key("own"));

        providers.release("beta");
        assert!(providers.is_empty());
    }

    #[test]
    fn what_a_plugin_registered_includes_keys_another_resolves_to() {
        let mut providers = Providers::default();
        providers.insert("shared".into(), handler("alpha"));
        providers.insert("shared".into(), handler("beta"));
        let keys: Vec<String> = providers
            .registered_by("beta")
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(keys, vec!["shared".to_string()]);
        assert_eq!(providers.iter().count(), 1, "one resolved handler per key");
    }
}
