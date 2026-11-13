use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::config::PluginConfig;
use crate::executor::Executor;
use crate::registry::PluginRegistry;

pub struct PluginManagerState {
    pub config: PluginConfig,
    pub registry: PluginRegistry,
    pub executor: Executor,
}

impl PluginManagerState {
    /// State sized from `config` and this host.
    pub fn new(config: PluginConfig) -> Self {
        let executor = Executor::for_this_host(config.max_concurrent_calls, config.memory_budget);
        Self::with_executor(config, executor)
    }

    pub fn with_executor(config: PluginConfig, executor: Executor) -> Self {
        if !config.check_plugins_dir() {
            let _ = std::fs::create_dir_all(&config.plugins_dir);
        }

        Self {
            config,
            registry: Arc::new(RwLock::new(HashMap::new())),
            executor,
        }
    }
}
