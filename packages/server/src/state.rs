use std::sync::Arc;

use common::db::DatabaseConnection;
use common::storage::BlobStore;
use plugin_core::traits::PluginManager;

use crate::config::AppConfig;
use crate::dispatcher::permits::DispatcherSemaphore;
use crate::hooks::SharedHookRegistry;
use crate::registry::{
    CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
    LanguageResolverRegistry, OperationBatches, OperationWaiters,
};

#[derive(Clone)]
pub struct RegistryState {
    pub contest_type_registry: ContestTypeRegistry,
    pub evaluator_registry: EvaluatorRegistry,
    pub checker_stage_registry: CheckerStageRegistry,
    pub language_resolver_registry: LanguageResolverRegistry,
    pub operation_batches: OperationBatches,
    pub operation_waiters: OperationWaiters,
    pub evaluate_batches: EvaluateBatches,
    pub hook_registry: SharedHookRegistry,
}

impl RegistryState {
    /// Snapshot of the language ids currently registered by plugins, for
    /// validating a request's language or submission format against.
    pub async fn known_language_ids(&self) -> std::collections::HashSet<String> {
        self.language_resolver_registry
            .read()
            .await
            .keys()
            .cloned()
            .collect()
    }
}

#[derive(Clone)]
pub struct AppState {
    pub plugins: Arc<dyn PluginManager>,
    pub db: DatabaseConnection,
    pub config: AppConfig,
    pub mq: Option<mq::Mq>,
    pub redis_client: Option<Arc<redis::Client>>,
    pub blob_store: Arc<dyn BlobStore>,
    pub registries: RegistryState,
    pub metrics: common::metrics::Metrics,
    pub prometheus_registry: prometheus::Registry,
    pub dispatcher_permits: DispatcherSemaphore,
    /// In-memory, per-process brute-force throttle for the login endpoint.
    pub login_throttle: Arc<crate::utils::login_throttle::LoginThrottle>,
    /// What this replica applied of the fleet-wide plugin desired state.
    pub plugin_lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>,
}
