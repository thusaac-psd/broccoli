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

/// What a replica would run for a plugin: its manifest version plus a hash
/// of the files the host loads (see [`PluginManifest::loaded_paths`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    pub version: String,
    pub hash: String,
}

/// Fingerprint the plugin as it is on this replica's disk now. Blocking file
/// reads, so it runs off the async runtime. A referenced file that is missing
/// hashes as missing rather than failing, so a half-copied plugin simply
/// fingerprints differently.
pub(crate) async fn fingerprint(state: &AppState, plugin_id: &str) -> Option<Fingerprint> {
    let (root, manifest) = {
        let registry = state.plugins.get_registry().read().ok()?;
        let entry = registry.get(plugin_id)?;
        (entry.root_dir.clone(), entry.manifest.clone())
    };
    tokio::task::spawn_blocking(move || {
        // Re-read the manifest: the registry's copy may predate the files.
        let manifest = plugin_core::registry::PluginEntry::from_dir(&root)
            .map(|e| e.manifest)
            .unwrap_or(manifest);
        let mut files = vec![std::path::PathBuf::from("plugin.toml")];
        for path in manifest.loaded_paths() {
            collect_files(&root, std::path::Path::new(path), &mut files);
        }
        files.sort();
        files.dedup();
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        for rel in &files {
            hasher.update(rel.to_string_lossy().as_bytes());
            match std::fs::read(root.join(rel)) {
                Ok(bytes) => {
                    hasher.update([1]);
                    hasher.update((bytes.len() as u64).to_le_bytes());
                    hasher.update(&bytes);
                }
                Err(_) => hasher.update([0]),
            }
        }
        Fingerprint {
            version: manifest.version,
            hash: hex::encode(hasher.finalize()),
        }
    })
    .await
    .ok()
}

/// `rel` itself if it is a file (or missing), else every file below it.
fn collect_files(root: &std::path::Path, rel: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let full = root.join(rel);
    match std::fs::read_dir(&full) {
        Ok(entries) => {
            for entry in entries.flatten() {
                collect_files(root, &rel.join(entry.file_name()), out);
            }
        }
        Err(_) => out.push(rel.to_path_buf()),
    }
}

/// Everything a loaded plugin contributes to the host besides its registry
/// entry: what its `init()` registered, keyed by the plugin id. Hooks are not
/// captured; they come from the manifest and are re-registered from it.
#[derive(Default)]
pub(crate) struct Registrations {
    contest_types: Vec<(String, crate::registry::ContestTypeHandlers)>,
    evaluators: Vec<(String, crate::registry::EvaluatorHandler)>,
    checker_stages: Vec<(String, crate::registry::CheckerStageHandlers)>,
    language_resolvers: Vec<(String, crate::registry::LanguageResolverEntry)>,
}

impl Registrations {
    pub(crate) async fn capture(registries: &RegistryState, plugin_id: &str) -> Self {
        fn owned<T: Clone>(
            map: &std::collections::HashMap<String, T>,
            plugin_id: &str,
            owner: impl Fn(&T) -> &str,
        ) -> Vec<(String, T)> {
            map.iter()
                .filter(|(_, v)| owner(v) == plugin_id)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        }
        Self {
            contest_types: owned(
                &*registries.contest_type_registry.read().await,
                plugin_id,
                |h| &h.plugin_id,
            ),
            evaluators: owned(
                &*registries.evaluator_registry.read().await,
                plugin_id,
                |h| &h.plugin_id,
            ),
            checker_stages: owned(
                &*registries.checker_stage_registry.read().await,
                plugin_id,
                |h| &h.plugin_id,
            ),
            language_resolvers: owned(
                &*registries.language_resolver_registry.read().await,
                plugin_id,
                |h| &h.plugin_id,
            ),
        }
    }

    async fn restore(self, registries: &RegistryState) {
        registries
            .contest_type_registry
            .write()
            .await
            .extend(self.contest_types);
        registries
            .evaluator_registry
            .write()
            .await
            .extend(self.evaluators);
        registries
            .checker_stage_registry
            .write()
            .await
            .extend(self.checker_stages);
        registries
            .language_resolver_registry
            .write()
            .await
            .extend(self.language_resolvers);
    }
}

/// The version of a plugin that was loaded before a reload replaced it,
/// kept until the new version is known to work everywhere it has to.
pub(crate) struct PreviousVersion {
    entry: plugin_core::registry::PluginEntry,
    registrations: Registrations,
}

/// Why a reload did not take.
#[derive(Debug)]
pub(crate) struct LoadFailure {
    pub error: PluginError,
    /// The previously loaded version is still serving.
    pub kept_previous: bool,
}

/// Load `plugin_id` afresh from its directory (re-reading the manifest,
/// rebuilding the runtime, registering hooks, running `init()`) WITHOUT
/// losing a version that is already loaded: the old registry entry and
/// registrations are set aside first and put back if anything fails, so a
/// reload that cannot load never turns a working plugin into a failed one.
///
/// On success the set-aside version is returned, so a caller that must
/// still confirm the change (publish it to the fleet) can [`roll_back`] to it.
pub(crate) async fn reload_from_disk(
    state: &AppState,
    plugin_id: &str,
) -> Result<Option<PreviousVersion>, LoadFailure> {
    let fail = |error, kept_previous| LoadFailure {
        error,
        kept_previous,
    };
    let registry = state.plugins.get_registry();
    let lock_err = || PluginError::Internal("Failed to acquire plugin registry lock".into());

    let (root_dir, was_loaded) = {
        let reg = registry.read().map_err(|_| fail(lock_err(), false))?;
        let entry = reg
            .get(plugin_id)
            .ok_or_else(|| fail(PluginError::NotFound(plugin_id.to_string()), false))?;
        (
            entry.root_dir.clone(),
            entry.status == plugin_core::registry::PluginStatus::Loaded,
        )
    };
    // Parse first: a broken manifest leaves everything as it was.
    let fresh =
        plugin_core::registry::PluginEntry::from_dir(&root_dir).map_err(|e| fail(e, was_loaded))?;

    let registrations = if was_loaded {
        Registrations::capture(&state.registries, plugin_id).await
    } else {
        Registrations::default()
    };
    purge_plugin_registrations(&state.registries, plugin_id).await;
    let old = registry
        .write()
        .map_err(|_| fail(lock_err(), false))?
        .insert(plugin_id.to_string(), fresh);
    let previous = old.filter(|_| was_loaded).map(|entry| PreviousVersion {
        entry,
        registrations,
    });

    match activate_plugin(state, plugin_id).await {
        Ok(()) => Ok(previous),
        Err(error) => match previous {
            Some(previous) => {
                roll_back(state, plugin_id, previous).await;
                Err(fail(error, true))
            }
            None => Err(fail(error, false)),
        },
    }
}

/// Put back the version [`reload_from_disk`] replaced.
pub(crate) async fn roll_back(state: &AppState, plugin_id: &str, previous: PreviousVersion) {
    purge_plugin_registrations(&state.registries, plugin_id).await;
    let manifest = previous.entry.manifest.clone();
    if let Ok(mut reg) = state.plugins.get_registry().write() {
        reg.insert(plugin_id.to_string(), previous.entry);
    }
    previous.registrations.restore(&state.registries).await;
    register_plugin_hooks(state, plugin_id, &manifest).await;
    if let Err(e) = state.plugins.update_translations() {
        tracing::warn!(plugin_id, error = %e, "Failed to restore translations after a rollback");
    }
    tracing::warn!(
        plugin_id,
        "Kept the previously loaded version of the plugin"
    );
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
            operation_batches: Default::default(),
            operation_waiters: Arc::new(dashmap::DashMap::new()),
            evaluate_batches: Default::default(),
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
