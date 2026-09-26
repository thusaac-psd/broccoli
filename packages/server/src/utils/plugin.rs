use plugin_core::traits::PluginManager;
use sea_orm::*;
use std::sync::Arc;
use tracing::instrument;

use plugin_core::error::PluginError;
use plugin_core::hook::PluginHook;
use plugin_core::manifest::PluginManifest;

use crate::entity::plugin as plugin_entity;
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

pub async fn activate_plugin(state: &AppState, plugin_id: &str) -> Result<(), PluginError> {
    state.plugins.load_plugin(plugin_id)?;

    let manifest = plugin_manifest(state.plugins.as_ref(), plugin_id)?;
    register_plugin_hooks(state, plugin_id, &manifest).await;

    match call_plugin_init(state.plugins.as_ref(), plugin_id).await {
        Ok(()) => {
            state.plugins.update_translations()?;
            Ok(())
        }
        Err(error) => {
            purge_plugin_registrations(&state.registries, plugin_id).await;
            let _ = state.plugins.unload_plugin(plugin_id);
            let _ = mark_plugin_failed(state.plugins.as_ref(), plugin_id, &error);
            if let Err(e) =
                crate::dispatcher::plugin_timer::delete_timers_for_plugin(&state.db, plugin_id)
                    .await
            {
                tracing::error!(
                    plugin_id,
                    error = %e,
                    "Failed to delete pending timers for a plugin that failed activation"
                );
            }
            tracing::error!("Plugin '{}' init() failed: {}", plugin_id, error);
            Err(error)
        }
    }
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

#[instrument(skip(state))]
pub async fn sync_plugins(state: &AppState) -> anyhow::Result<Vec<PluginActivationFailure>> {
    state.plugins.discover_plugins()?;

    let mut failures = Vec::new();

    for plugin in state.plugins.list_plugins()? {
        let plugin_id = plugin.id.clone();

        let plugin_model = plugin_entity::Entity::find_by_id(plugin_id.clone())
            .one(&state.db)
            .await?;
        let plugin_model = match plugin_model {
            None => {
                let new_plugin = plugin_entity::ActiveModel {
                    id: Set(plugin_id),
                    is_enabled: Set(true),
                    updated_at: Set(chrono::Utc::now()),
                };
                new_plugin.insert(&state.db).await?
            }
            Some(existing_plugin) => existing_plugin,
        };

        if plugin_model.is_enabled {
            match activate_plugin(state, &plugin.id).await {
                Ok(()) => {
                    tracing::info!("Plugin '{}' loaded successfully", plugin.id);
                }
                Err(error) => {
                    tracing::error!("Plugin '{}' activation failed: {}", plugin.id, error);
                    failures.push(PluginActivationFailure {
                        plugin_id: plugin.id.clone(),
                        error,
                    });
                }
            }
        } else {
            tracing::info!("Plugin '{}' is disabled, skipping load", plugin.id);
        }
    }

    Ok(failures)
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
