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
    /// It checks output with the problem's checker, so work it evaluates
    /// needs a checker plugin for the problem's checker format.
    pub uses_checker: bool,
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

    /// The handler `key` resolves to among the plugins loaded here. A
    /// dispatch resolves fleet-wide instead (`providers::resolve`).
    pub fn get(&self, key: &str) -> Option<&H> {
        self.keys.get(key)?.values().next()
    }

    /// `plugin_id`'s registration of `key`, if it made one here.
    pub fn registration(&self, key: &str, plugin_id: &str) -> Option<&H> {
        self.keys.get(key)?.get(plugin_id)
    }

    /// The plugins that registered `key` here, ids sorted.
    pub fn plugins_of(&self, key: &str) -> Vec<String> {
        self.keys
            .get(key)
            .map(|providers| providers.keys().cloned().collect())
            .unwrap_or_default()
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

/// Which plugin owns each type a problem or contest has: a contest's
/// contest type, a problem's problem type, checker format and default
/// contest type, for what is stored or checked per plugin (config
/// namespaces, config issues, hook enablement). Each key's owner is its
/// fleet-wide provider (`providers::provider_id`), the same plugin judging
/// runs it on, whether or not it is loaded here. A plugin's config checks
/// reach every problem and contest of the types it owns, whether or not a
/// config is stored there (`services::config_issues`).
#[derive(Clone)]
pub struct TypeProviders {
    contest_types: ContestTypeRegistry,
    evaluators: EvaluatorRegistry,
    checker_stages: CheckerStageRegistry,
    lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>,
}

/// The keys of each type registry one plugin owns.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ProvidedTypes {
    pub contest_types: Vec<String>,
    pub problem_types: Vec<String>,
    pub checker_formats: Vec<String>,
}

impl TypeProviders {
    pub fn new(
        contest_types: ContestTypeRegistry,
        evaluators: EvaluatorRegistry,
        checker_stages: CheckerStageRegistry,
        lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>,
    ) -> Self {
        Self {
            contest_types,
            evaluators,
            checker_stages,
            lifecycle,
        }
    }

    async fn owner<H: Provided>(
        &self,
        registry: &RwLock<Providers<H>>,
        kind: crate::providers::Provided,
        key: &str,
    ) -> Option<String> {
        crate::providers::provider_id(
            &*registry.read().await,
            self.lifecycle.provider(kind, key),
            key,
        )
    }

    /// The plugin that owns contest type `key`.
    pub async fn of_contest_type(&self, key: &str) -> Option<String> {
        self.owner(
            &self.contest_types,
            crate::providers::Provided::ContestType,
            key,
        )
        .await
    }

    /// The plugins that own a problem's own types: its problem type, its
    /// checker format and its default contest type, each once.
    pub async fn of_problem(
        &self,
        problem_type: &str,
        checker_format: &str,
        default_contest_type: &str,
    ) -> Vec<String> {
        use crate::providers::Provided as Kind;
        let mut plugins: Vec<String> = [
            self.owner(&self.evaluators, Kind::ProblemType, problem_type)
                .await,
            self.owner(&self.checker_stages, Kind::CheckerFormat, checker_format)
                .await,
            self.owner(&self.contest_types, Kind::ContestType, default_contest_type)
                .await,
        ]
        .into_iter()
        .flatten()
        .collect();
        plugins.sort_unstable();
        plugins.dedup();
        plugins
    }

    /// The types `plugin_id` owns: of the keys it registered here or last
    /// recorded fleet-wide, those whose owner it is.
    pub async fn provided_by(&self, plugin_id: &str) -> ProvidedTypes {
        use crate::providers::Provided as Kind;
        ProvidedTypes {
            contest_types: self
                .owned(&self.contest_types, Kind::ContestType, plugin_id)
                .await,
            problem_types: self
                .owned(&self.evaluators, Kind::ProblemType, plugin_id)
                .await,
            checker_formats: self
                .owned(&self.checker_stages, Kind::CheckerFormat, plugin_id)
                .await,
        }
    }

    async fn owned<H: Provided>(
        &self,
        registry: &RwLock<Providers<H>>,
        kind: crate::providers::Provided,
        plugin_id: &str,
    ) -> Vec<String> {
        let registry = registry.read().await;
        let mut candidates: Vec<String> = registry
            .registered_by(plugin_id)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        if let Some(governs) = self.lifecycle.governs(plugin_id) {
            candidates.extend(governs.provides.keys(kind).iter().cloned());
        }
        candidates.sort_unstable();
        candidates.dedup();
        candidates
            .into_iter()
            .filter(|key| {
                crate::providers::provider_id(&registry, self.lifecycle.provider(kind, key), key)
                    .as_deref()
                    == Some(plugin_id)
            })
            .collect()
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
