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
    /// The detached judging sessions running here, the same the plugin
    /// manager's host functions start.
    pub judging_sessions: crate::services::judging_sessions::JudgingSessions,
    /// The dispatches running here, whose leases the lease refresh keeps.
    pub live_dispatches: crate::services::judging::LiveDispatches,
}

#[derive(Clone)]
pub struct AppState {
    pub plugins: Arc<dyn PluginManager>,
    pub db: DatabaseConnection,
    /// The platform wall clock (the database's); see [`crate::clock`].
    pub clock: crate::clock::PlatformClock,
    /// `plugin_config` point reads; see [`crate::services::plugin_config_cache`].
    pub plugin_config_cache: crate::services::plugin_config_cache::PluginConfigCache,
    pub config: AppConfig,
    /// The message queue. Required: a server never runs without it; boot
    /// waits for Redis, and the claim fiber claims nothing while it is down.
    pub mq: mq::Mq,
    pub redis_client: Option<Arc<redis::Client>>,
    pub blob_store: Arc<dyn BlobStore>,
    pub registries: RegistryState,
    pub metrics: common::metrics::Metrics,
    pub prometheus_registry: prometheus::Registry,
    pub dispatcher_permits: DispatcherSemaphore,
    /// Task slots of the live workers, refreshed from their heartbeats.
    pub worker_slots: crate::dispatcher::fanout::WorkerSlots,
    /// What this replica applied of the fleet-wide plugin desired state.
    pub plugin_lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>,
    /// The plugins' raw SQL pools, the same the `sql` host functions use.
    pub plugin_sql: crate::plugin_db::PluginSqlPools,
    /// The worker list the system overview last read.
    pub last_workers: crate::handlers::system::LastWorkers,
}

impl AppState {
    /// Which plugin owns each type a problem or contest has, by the fleet
    /// rule (`registry::TypeProviders`).
    pub fn type_providers(&self) -> crate::registry::TypeProviders {
        crate::registry::TypeProviders::new(
            self.registries.contest_type_registry.clone(),
            self.registries.evaluator_registry.clone(),
            self.registries.checker_stage_registry.clone(),
            self.plugin_lifecycle.clone(),
        )
    }
}

impl AppState {
    /// The `kind` keys work and requests may name: every key a plugin of the
    /// fleet recorded providing, and every one registered here. A key whose
    /// plugin is loaded nowhere or not here is still known, and refused by
    /// why it cannot run (`PluginLifecycle::key_closure`, the submit paths'
    /// `accepting_handler`), never as unknown.
    pub async fn known_keys(
        &self,
        kind: broccoli_api_types::visibility::Provided,
    ) -> std::collections::HashSet<String> {
        use broccoli_api_types::visibility::Provided;
        let mut known = self.plugin_lifecycle.recorded_keys(kind);
        let registries = &self.registries;
        match kind {
            Provided::ContestType => known.extend(
                registries
                    .contest_type_registry
                    .read()
                    .await
                    .keys()
                    .cloned(),
            ),
            Provided::ProblemType => {
                known.extend(registries.evaluator_registry.read().await.keys().cloned())
            }
            Provided::CheckerFormat => known.extend(
                registries
                    .checker_stage_registry
                    .read()
                    .await
                    .keys()
                    .cloned(),
            ),
            Provided::Language => known.extend(
                registries
                    .language_resolver_registry
                    .read()
                    .await
                    .keys()
                    .cloned(),
            ),
        }
        known
    }

    /// The languages work may name ([`Self::known_keys`]).
    pub async fn known_languages(&self) -> std::collections::HashSet<String> {
        self.known_keys(broccoli_api_types::visibility::Provided::Language)
            .await
    }
}
