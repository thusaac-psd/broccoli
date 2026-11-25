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

    /// The guest failed: it returned an error, trapped, or a host function
    /// it called failed. `message` is the reason, with the reason of every
    /// nested call that failed under it; `depth` is the call's nesting depth
    /// (1 for a top-level call).
    #[error(
        "Failed to call function '{func_name}' on plugin '{plugin_id}' at call depth {depth}: {message}"
    )]
    ExecutionFailed {
        plugin_id: String,
        func_name: String,
        depth: u8,
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

    /// The call could not run to completion because the host's database was
    /// unreachable (a host function failed it with
    /// [`crate::executor::fail_database_unavailable`]). Like
    /// [`Self::Unavailable`] it says nothing about the work; callers answer
    /// it as the database's outage.
    #[error("Plugin '{plugin_id}' could not finish '{func_name}': the database is unreachable")]
    DatabaseUnavailable {
        plugin_id: String,
        func_name: String,
    },

    /// The call could not run to completion because every connection to the
    /// host's database was in use (a host function failed it with
    /// [`crate::executor::fail_database_busy`]). The database answers; a
    /// moment later it serves again.
    #[error(
        "Plugin '{plugin_id}' could not finish '{func_name}': every database connection is in use"
    )]
    DatabaseBusy {
        plugin_id: String,
        func_name: String,
    },

    /// The call could not run to completion because its own SQL found no
    /// plugin SQL connection or permit free in time (a host function failed
    /// it with [`crate::executor::fail_sql_busy`]): contention among the
    /// plugin's calls, which a moment later serve again. The host database
    /// answers, so it is not a host outage ([`PluginError::is_host_outage`]).
    #[error(
        "Plugin '{plugin_id}' could not finish '{func_name}': no plugin SQL connection was free"
    )]
    SqlBusy {
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
    /// anything about the request: the call reached its deadline, the
    /// host's database is unreachable or busy, the server is draining, the plugin is
    /// between loads, or no instance could be built. Callers answer these as
    /// "try again", never as a decision.
    /// Whether the platform, not the plugin, kept the call from answering:
    /// the host database is lost or saturated, or the server is draining.
    /// The plugin had no say in it, so nothing it is owed may be spent on it.
    pub fn is_host_outage(&self) -> bool {
        matches!(
            self,
            PluginError::DatabaseUnavailable { .. }
                | PluginError::DatabaseBusy { .. }
                | PluginError::ShuttingDown
        )
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(
            self,
            PluginError::Unavailable { .. }
                | PluginError::DatabaseUnavailable { .. }
                | PluginError::DatabaseBusy { .. }
                | PluginError::SqlBusy { .. }
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
