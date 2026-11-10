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

    /// Distinct variant for "instance pool acquisition timed out" so callers can
    /// retry with backoff. Plugin pool contention is *not* a permanent failure
    /// of the user's request and should never be surfaced as a final verdict.
    #[error("Plugin '{0}' pool acquisition timed out")]
    PoolTimeout(String),
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
