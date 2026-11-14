use std::sync::{Arc, Weak};

use common::db::DatabaseConnection;
use common::storage::BlobStore;
use plugin_core::traits::PluginManager;

use crate::config::AppConfig;
use crate::hooks::SharedHookRegistry;
use crate::registry::{
    CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
    LanguageResolverRegistry, OperationBatches, OperationWaiters,
};
use crate::services::operation_batch::OperationTaskPublisher;

#[derive(Clone)]
pub struct HostFunctionSystemDeps {
    /// The RESTRICTED plugin pool (runs as `broccoli_plugin`). Backs the raw
    /// `sql` capability; cannot write/DDL core tables.
    pub db: DatabaseConnection,
    /// The host function pool (the app role). Every host fn that touches the
    /// database other than raw `sql` runs SQL the host builds on it, and so do
    /// the host services those fns start. See `database::connect_host_fn_pool`.
    pub host_db: DatabaseConnection,
    pub mq: Option<mq::Mq>,
    pub operation_batches: OperationBatches,
    pub operation_waiters: OperationWaiters,
    pub contest_type_registry: ContestTypeRegistry,
    pub evaluator_registry: EvaluatorRegistry,
    pub checker_stage_registry: CheckerStageRegistry,
    pub language_resolver_registry: LanguageResolverRegistry,
    pub evaluate_batches: EvaluateBatches,
    pub blob_store: Arc<dyn BlobStore>,
    pub hook_registry: SharedHookRegistry,
    pub config: AppConfig,
    pub metrics: Option<common::metrics::Metrics>,
    pub redis_client: Option<Arc<redis::Client>>,
    /// Task slots of the live workers; sizes the judging fan-out.
    pub worker_slots: crate::dispatcher::fanout::WorkerSlots,
    pub plugin_config_cache: crate::services::plugin_config_cache::PluginConfigCache,
}

/// The plugin manager as the host functions it owns hold it: weakly, so the
/// manager and its host functions do not keep each other alive. A host
/// function only runs inside a call into the manager, so the manager is
/// there whenever one asks; work a host function starts takes a strong
/// reference ([`ManagerRef::get`]) for as long as it runs.
#[derive(Clone)]
pub struct ManagerRef(Weak<dyn PluginManager>);

impl ManagerRef {
    pub fn new(manager: &Arc<dyn PluginManager>) -> Self {
        Self(Arc::downgrade(manager))
    }

    /// The manager, or an error once it is gone (the server is stopping).
    pub fn get(&self) -> anyhow::Result<Arc<dyn PluginManager>> {
        self.0
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("the plugin manager has shut down"))
    }
}

#[derive(Clone)]
pub struct HostFunctionDeps {
    pub system: HostFunctionSystemDeps,
    pub plugin_manager: ManagerRef,
}

#[derive(Clone)]
pub struct OperationHostDeps {
    pub plugin_manager: Option<ManagerRef>,
    pub operation_publisher: Option<Arc<dyn OperationTaskPublisher>>,
    pub operation_batches: OperationBatches,
    pub operation_waiters: OperationWaiters,
    /// This server's id: tasks carry it as `reply_to`.
    pub server_id: String,
    pub blob_store: Arc<dyn BlobStore>,
    pub metrics: Option<common::metrics::Metrics>,
    /// Evaluate batches whose test cases an operation may run for.
    pub evaluate_batches: EvaluateBatches,
}

#[derive(Clone)]
pub struct EvaluateHostDeps {
    pub db: DatabaseConnection,
    pub evaluator_registry: EvaluatorRegistry,
    pub evaluate_batches: EvaluateBatches,
    pub operation_deps: OperationHostDeps,
    pub fanout: crate::dispatcher::fanout::Fanout,
    pub plugin_manager: ManagerRef,
    pub blob_store: Arc<dyn BlobStore>,
    pub hook_registry: SharedHookRegistry,
    /// Resolves which plugin owns a submission's contest type, for
    /// `after_judging` hook enablement (see `hooks::resource_enablements`).
    pub contest_type_registry: ContestTypeRegistry,
    pub metrics: Option<common::metrics::Metrics>,
    pub redis_client: Option<Arc<redis::Client>>,
    pub plugin_config_cache: crate::services::plugin_config_cache::PluginConfigCache,
    pub cancel_primitive_enabled: bool,
}

impl HostFunctionSystemDeps {
    pub fn with_plugin_manager(self, plugin_manager: &Arc<dyn PluginManager>) -> HostFunctionDeps {
        HostFunctionDeps {
            system: self,
            plugin_manager: ManagerRef::new(plugin_manager),
        }
    }

    pub fn operation_deps(&self) -> OperationHostDeps {
        OperationHostDeps {
            plugin_manager: None,
            operation_publisher: self
                .mq
                .clone()
                .map(|mq| Arc::new(mq) as Arc<dyn OperationTaskPublisher>),
            operation_batches: self.operation_batches.clone(),
            operation_waiters: self.operation_waiters.clone(),
            server_id: self.config.server.id.clone(),
            blob_store: self.blob_store.clone(),
            metrics: self.metrics.clone(),
            evaluate_batches: self.evaluate_batches.clone(),
        }
    }
}

impl HostFunctionDeps {
    pub fn evaluate_deps(&self, fanout: crate::dispatcher::fanout::Fanout) -> EvaluateHostDeps {
        EvaluateHostDeps {
            db: self.system.host_db.clone(),
            evaluator_registry: self.system.evaluator_registry.clone(),
            evaluate_batches: self.system.evaluate_batches.clone(),
            operation_deps: {
                let mut deps = self.system.operation_deps();
                deps.plugin_manager = Some(self.plugin_manager.clone());
                deps
            },
            fanout,
            plugin_manager: self.plugin_manager.clone(),
            blob_store: self.system.blob_store.clone(),
            hook_registry: self.system.hook_registry.clone(),
            contest_type_registry: self.system.contest_type_registry.clone(),
            metrics: self.system.metrics.clone(),
            redis_client: self.system.redis_client.clone(),
            plugin_config_cache: self.system.plugin_config_cache.clone(),
            cancel_primitive_enabled: self.system.config.server.cancel_primitive_enabled,
        }
    }

    pub fn operation_deps(&self) -> OperationHostDeps {
        let mut deps = self.system.operation_deps();
        deps.plugin_manager = Some(self.plugin_manager.clone());
        deps
    }
}
