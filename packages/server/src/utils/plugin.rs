use plugin_core::traits::PluginManager;
use std::sync::Arc;
use tracing::instrument;

use plugin_core::error::PluginError;
use plugin_core::hook::PluginHook;
use plugin_core::manifest::PluginManifest;

use crate::state::{AppState, RegistryState};

pub async fn purge_plugin_registrations(registries: &RegistryState, plugin_id: &str) {
    registries
        .contest_type_registry
        .write()
        .await
        .retain(|_, h| h.plugin_id != plugin_id);
    registries
        .evaluator_registry
        .write()
        .await
        .retain(|_, h| h.plugin_id != plugin_id);
    registries
        .checker_stage_registry
        .write()
        .await
        .retain(|_, h| h.plugin_id != plugin_id);
    registries
        .language_resolver_registry
        .write()
        .await
        .retain(|_, h| h.plugin_id != plugin_id);
    registries
        .hook_registry
        .write()
        .await
        .unregister_plugin(plugin_id);
}

pub async fn call_plugin_init(
    plugins: &dyn PluginManager,
    plugin_id: &str,
) -> Result<(), PluginError> {
    match plugins.call_raw(plugin_id, "init", vec![]).await {
        Ok(_) => {
            tracing::info!("Plugin '{}' init() complete", plugin_id);
            Ok(())
        }
        Err(plugin_core::error::PluginError::NoRuntime(_)) => {
            tracing::debug!("Plugin '{}' is frontend-only, skipping init()", plugin_id);
            Ok(())
        }
        Err(plugin_core::error::PluginError::FunctionNotFound { .. }) => {
            tracing::debug!("Plugin '{}' has no init() function (optional)", plugin_id);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

fn plugin_manifest(
    plugins: &dyn PluginManager,
    plugin_id: &str,
) -> Result<PluginManifest, PluginError> {
    let registry = plugins
        .get_registry()
        .read()
        .map_err(|_| PluginError::Internal("Failed to acquire plugin registry read lock".into()))?;
    let entry = registry
        .get(plugin_id)
        .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;
    Ok(entry.manifest.clone())
}

fn mark_plugin_failed(
    plugins: &dyn PluginManager,
    plugin_id: &str,
    error: &PluginError,
) -> Result<(), PluginError> {
    let mut registry = plugins.get_registry().write().map_err(|_| {
        PluginError::Internal("Failed to acquire plugin registry write lock".into())
    })?;
    let entry = registry
        .get_mut(plugin_id)
        .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;
    entry.runtime = None;
    entry.status = plugin_core::registry::PluginStatus::Failed(error.to_string());
    Ok(())
}

/// Register a freshly loaded plugin's hooks and run its `init()`. On failure
/// the plugin is left unloaded with status `Failed` (see [`fail_plugin`]).
///
/// A failure here is LOCAL to this replica: it never touches fleet-wide
/// state such as the plugin's pending timers, which a peer that loaded the
/// plugin fine is still delivering. [`crate::plugin_lifecycle`] retries it.
pub async fn activate_plugin(state: &AppState, plugin_id: &str) -> Result<(), PluginError> {
    let result = async {
        state.plugins.load_plugin(plugin_id)?;
        let manifest = plugin_manifest(state.plugins.as_ref(), plugin_id)?;
        register_plugin_hooks(state, plugin_id, &manifest).await;
        call_plugin_init(state.plugins.as_ref(), plugin_id).await?;
        state.plugins.update_translations()
    }
    .await;
    if let Err(error) = &result {
        tracing::error!("Plugin '{}' activation failed: {}", plugin_id, error);
        fail_plugin(state, plugin_id, error).await;
    }
    result
}

/// Drop everything a half-activated plugin registered and mark it `Failed`.
pub async fn fail_plugin(state: &AppState, plugin_id: &str, error: &PluginError) {
    purge_plugin_registrations(&state.registries, plugin_id).await;
    let _ = state.plugins.unload_plugin(plugin_id);
    let _ = mark_plugin_failed(state.plugins.as_ref(), plugin_id, error);
}

/// Activation outcome for a single plugin during [`sync_plugins`].
///
/// Production startup logs and ignores failures so one broken plugin doesn't
/// abort boot; tests inspect the returned vec and fail loudly when non-empty.
#[derive(Debug)]
pub struct PluginActivationFailure {
    pub plugin_id: String,
    pub error: PluginError,
}

/// Boot-time plugin sync: discover plugins on disk and converge onto the
/// fleet's desired state. See [`crate::plugin_lifecycle::boot`].
#[instrument(skip(state))]
pub async fn sync_plugins(state: &AppState) -> anyhow::Result<Vec<PluginActivationFailure>> {
    crate::plugin_lifecycle::boot(state).await
}

async fn register_plugin_hooks(
    state: &AppState,
    plugin_id: &str,
    manifest: &plugin_core::manifest::PluginManifest,
) {
    let server_config = match &manifest.server {
        Some(sc) => sc,
        None => return,
    };

    if server_config.hooks.is_empty() {
        return;
    }

    let mut registry = state.registries.hook_registry.write().await;

    for decl in &server_config.hooks {
        if decl.mode == plugin_core::hook::HookMode::Notify && decl.topic.starts_with("before_") {
            tracing::warn!(
                plugin_id,
                topic = %decl.topic,
                function = %decl.function,
                "Notify hook registered on blocking topic (before_*). \
                 Its response will be ignored at runtime.",
            );
        }

        if decl.mode == plugin_core::hook::HookMode::Blocking && decl.topic.starts_with("after_") {
            tracing::warn!(
                plugin_id,
                topic = %decl.topic,
                function = %decl.function,
                "Blocking hook registered on background topic (after_*). \
                 Reject/Stop responses will be silently discarded at runtime.",
            );
        }

        let hook = Arc::new(PluginHook::new(
            state.plugins.clone(),
            plugin_id.to_string(),
            decl.function.clone(),
            vec![decl.topic.clone()],
            decl.scope,
            decl.mode,
        ));

        registry.register(hook);

        tracing::info!(
            plugin_id,
            topic = %decl.topic,
            function = %decl.function,
            scope = ?decl.scope,
            mode = ?decl.mode,
            "Registered hook",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::CheckerStageHandlers;
    use std::collections::HashMap;
    use tokio::sync::RwLock;

    fn empty_registries() -> RegistryState {
        RegistryState {
            contest_type_registry: Arc::new(RwLock::new(HashMap::new())),
            evaluator_registry: Arc::new(RwLock::new(HashMap::new())),
            checker_stage_registry: Arc::new(RwLock::new(HashMap::new())),
            language_resolver_registry: Arc::new(RwLock::new(HashMap::new())),
            operation_batches: Arc::new(dashmap::DashMap::new()),
            operation_waiters: Arc::new(dashmap::DashMap::new()),
            evaluate_batches: Arc::new(dashmap::DashMap::new()),
            hook_registry: crate::hooks::new_shared_registry(),
        }
    }

    fn handlers(plugin_id: &str) -> CheckerStageHandlers {
        CheckerStageHandlers {
            plugin_id: plugin_id.to_string(),
            resolve_fn: "resolve".to_string(),
            interpret_fn: "interpret".to_string(),
        }
    }

    #[tokio::test]
    async fn purge_removes_checker_stage_registrations() {
        let registries = empty_registries();
        {
            let mut reg = registries.checker_stage_registry.write().await;
            reg.insert("exact".to_string(), handlers("standard-checkers"));
            reg.insert("none".to_string(), handlers("standard-checkers"));
            reg.insert("custom".to_string(), handlers("other-plugin"));
        }

        purge_plugin_registrations(&registries, "standard-checkers").await;

        let reg = registries.checker_stage_registry.read().await;
        assert!(!reg.contains_key("exact"), "purged plugin's format removed");
        assert!(!reg.contains_key("none"), "purged plugin's none removed");
        assert!(
            reg.contains_key("custom"),
            "another plugin's format must be retained"
        );
    }
}
