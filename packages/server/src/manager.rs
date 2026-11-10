use plugin_core::config::PluginConfig;
use plugin_core::host::HostFunctionRegistry;
use plugin_core::i18n::I18nRegistry;
use plugin_core::manager::PluginManagerState;
use plugin_core::manifest::PluginManifest;
use plugin_core::registry::PluginRegistry;
use plugin_core::traits::{PluginInvoker, PluginManager};
use std::sync::{Arc, OnceLock};

use crate::host_funcs;
use crate::host_funcs::context::HostFunctionSystemDeps;

pub struct ServerManager {
    state: PluginManagerState,
    host_functions: OnceLock<HostFunctionRegistry>,
    i18n: I18nRegistry,
    metrics: Option<common::metrics::Metrics>,
    call_gate: plugin_core::call_gate::CallGate,
}

impl ServerManager {
    pub fn new(
        config: PluginConfig,
        host_deps: HostFunctionSystemDeps,
        metrics: Option<common::metrics::Metrics>,
    ) -> Result<Arc<Self>, anyhow::Error> {
        let manager = Arc::new(Self {
            state: PluginManagerState::new(config),
            host_functions: OnceLock::new(),
            i18n: I18nRegistry::new(),
            metrics,
            call_gate: Default::default(),
        });

        let host_functions = host_funcs::init_host_functions(
            host_deps.with_plugin_manager(manager.clone() as Arc<dyn PluginManager>),
        );

        manager
            .host_functions
            .set(host_functions)
            .map_err(|_| anyhow::anyhow!("Host functions already initialized"))?;

        Ok(manager)
    }
}

impl PluginInvoker for ServerManager {
    fn get_config(&self) -> &PluginConfig {
        &self.state.config
    }
    fn get_registry(&self) -> &PluginRegistry {
        &self.state.registry
    }

    fn get_metrics(&self) -> Option<&common::metrics::Metrics> {
        self.metrics.as_ref()
    }

    fn call_gate(&self) -> Option<&plugin_core::call_gate::CallGate> {
        Some(&self.call_gate)
    }
}

impl PluginManager for ServerManager {
    fn get_host_functions(&self) -> &HostFunctionRegistry {
        self.host_functions
            .get()
            .expect("Host functions not initialized")
    }
    fn get_i18n_registry(&self) -> &I18nRegistry {
        &self.i18n
    }

    fn resolve(&self, manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
        manifest
            .server
            .as_ref()
            .map(|s| (s.entry.clone(), s.permissions.clone()))
    }
}
