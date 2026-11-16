use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Plugin '{0}' is not loaded")]
    NotLoaded(String),

    #[error("Plugin '{0}' has no Wasm runtime (frontend-only or configuration mismatch)")]
    NoRuntime(String),

    #[error("Plugin load failed: {0}")]
    LoadFailed(String),

    #[error("Plugin discovery failed: {0}")]
    DiscoveryFailed(String),

    #[error("Function '{func_name}' not found in plugin '{plugin_id}'")]
    FunctionNotFound {
        plugin_id: String,
        func_name: String,
    },

    #[error("Failed to call function '{func_name}' on plugin '{plugin_id}': {message}")]
    ExecutionFailed {
        plugin_id: String,
        func_name: String,
        message: String,
    },

    #[error("Extism error: {0}")]
    Extism(#[from] extism::Error),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),

    /// The host is shutting down and refused to start this call. Not a
    /// failure of the plugin or of the work it was asked to do: callers
    /// leave that work in flight for another server to pick up.
    #[error("Server is shutting down; plugin call refused")]
    ShuttingDown,

    /// The call could not run to completion before its deadline: it was
    /// still queued for admission, or the guest was cancelled at the
    /// deadline. A statement about load on this server, never about the
    /// work the call was asked to do: callers answer it as "try again" and
    /// never as a decision.
    #[error("Plugin '{plugin_id}' could not finish '{func_name}' before its deadline")]
    Unavailable {
        plugin_id: String,
        func_name: String,
    },

    /// No instance of the plugin could be built for this call (out of memory,
    /// the module file gone). Not an answer from the plugin.
    #[error("Plugin '{plugin_id}' could not start an instance: {message}")]
    InstanceBuild { plugin_id: String, message: String },

    /// A host function called back into a plugin deeper than
    /// [`crate::executor::MAX_DEPTH`] allows.
    #[error(
        "Call to '{func_name}' on plugin '{plugin_id}' nests deeper than {max_depth} plugin calls"
    )]
    NestingTooDeep {
        plugin_id: String,
        func_name: String,
        max_depth: u8,
    },
}

impl PluginError {
    /// Whether this says the plugin cannot answer right now rather than
    /// anything about the request: the call reached its deadline, the server
    /// is draining, the plugin is between loads, or no instance could be
    /// built. Callers answer these as "try again", never as a decision.
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            PluginError::Unavailable { .. }
                | PluginError::ShuttingDown
                | PluginError::NotLoaded(_)
                | PluginError::InstanceBuild { .. }
        )
    }
}

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("Plugin has no [web] configuration")]
    NoWebConfig,
    #[error("Path traversal attempt detected")]
    PathTraversal,
    #[error("File not found")]
    NotFound,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Internal error: {0}")]
    Internal(String),
}
