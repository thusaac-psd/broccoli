use std::sync::Arc;

use common::db::DatabaseConnection;
use common::storage::BlobStore;
use plugin_core::traits::PluginManager;
use tokio::sync::Semaphore;

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
    /// `sql` capability and read-only host fns; cannot write/DDL core tables.
    pub db: DatabaseConnection,
    /// The PRIVILEGED plugin pool (runs as the app role, no `SET ROLE`). Backs
    /// the gated core-WRITE host fns: `host.submission.*`, `host.storage.*`,
    /// and `config:write`. See `database::init_plugin_db_privileged`.
    pub privileged_db: DatabaseConnection,
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
}

#[derive(Clone)]
pub struct HostFunctionDeps {
    pub system: HostFunctionSystemDeps,
    pub plugin_manager: Arc<dyn PluginManager>,
}

#[derive(Clone)]
pub struct OperationHostDeps {
    pub plugin_manager: Option<Arc<dyn PluginManager>>,
    pub operation_publisher: Option<Arc<dyn OperationTaskPublisher>>,
    pub operation_batches: OperationBatches,
    pub operation_waiters: OperationWaiters,
    /// This server's id: tasks carry it as `reply_to`.
    pub server_id: String,
    pub blob_store: Arc<dyn BlobStore>,
    pub metrics: Option<common::metrics::Metrics>,
    /// Evaluate batches whose test cases an operation may run for.
    pub evaluate_batches: EvaluateBatches,
    /// Per-batch cap on concurrent in-flight blob-externalize + mq.publish
    /// operations inside `start_operation_batch`. Sourced from
    /// `server.operation_batch_publish_concurrency`.
    pub operation_batch_publish_concurrency: usize,
}

#[derive(Clone)]
pub struct EvaluateHostDeps {
    pub db: DatabaseConnection,
    pub evaluator_registry: EvaluatorRegistry,
    pub evaluate_batches: EvaluateBatches,
    pub operation_deps: OperationHostDeps,
    pub evaluator_slots: Arc<Semaphore>,
    pub fanout_slots: crate::dispatcher::fanout::FanoutSemaphore,
    pub plugin_manager: Arc<dyn PluginManager>,
    pub blob_store: Arc<dyn BlobStore>,
    pub hook_registry: SharedHookRegistry,
    /// Resolves which plugin owns a submission's contest type, for
    /// `after_judging` hook enablement (see `hooks::resource_enablements`).
    pub contest_type_registry: ContestTypeRegistry,
    pub metrics: Option<common::metrics::Metrics>,
    pub redis_client: Option<Arc<redis::Client>>,
    pub cancel_primitive_enabled: bool,
}

impl HostFunctionSystemDeps {
    pub fn with_plugin_manager(self, plugin_manager: Arc<dyn PluginManager>) -> HostFunctionDeps {
        HostFunctionDeps {
            system: self,
            plugin_manager,
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
            operation_batch_publish_concurrency: self
                .config
                .server
                .operation_batch_publish_concurrency
                as usize,
        }
    }
}

impl HostFunctionDeps {
    pub fn evaluate_deps(
        &self,
        evaluator_slots: Arc<Semaphore>,
        fanout_slots: crate::dispatcher::fanout::FanoutSemaphore,
    ) -> EvaluateHostDeps {
        EvaluateHostDeps {
            db: self.system.db.clone(),
            evaluator_registry: self.system.evaluator_registry.clone(),
            evaluate_batches: self.system.evaluate_batches.clone(),
            operation_deps: {
                let mut deps = self.system.operation_deps();
                deps.plugin_manager = Some(self.plugin_manager.clone());
                deps
            },
            evaluator_slots,
            fanout_slots,
            plugin_manager: self.plugin_manager.clone(),
            blob_store: self.system.blob_store.clone(),
            hook_registry: self.system.hook_registry.clone(),
            contest_type_registry: self.system.contest_type_registry.clone(),
            metrics: self.system.metrics.clone(),
            redis_client: self.system.redis_client.clone(),
            cancel_primitive_enabled: self.system.config.server.cancel_primitive_enabled,
        }
    }

    pub fn operation_deps(&self) -> OperationHostDeps {
        let mut deps = self.system.operation_deps();
        deps.plugin_manager = Some(self.plugin_manager.clone());
        deps
    }
}
