use async_trait::async_trait;
use extism::{Manifest, PluginBuilder, Wasm};
use opentelemetry::KeyValue;
use tracing::{debug, error, info, instrument, warn};

use crate::admission::CallClass;
use crate::config::PluginConfig;
use crate::error::{AssetError, PluginError};
use crate::executor::{CallOptions, Executor, NewScope, ScopeGuard};
use crate::host::HostFunctionRegistry;
use crate::i18n::{I18nRegistry, TranslationMap};
use crate::manifest::PluginManifest;
use crate::registry::{PluginEntry, PluginInfo, PluginRegistry, PluginStatus};

/// One call counted in `plugin_calls_in_flight` while alive.
struct InFlight(Option<(common::metrics::Metrics, [KeyValue; 1])>);

impl InFlight {
    fn enter(metrics: Option<&common::metrics::Metrics>, plugin_id: &str) -> Self {
        Self(metrics.map(|metrics| {
            let attrs = [KeyValue::new("plugin.id", plugin_id.to_string())];
            metrics.plugin_calls_in_flight.add(1, &attrs);
            (metrics.clone(), attrs)
        }))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        if let Some((metrics, attrs)) = &self.0 {
            metrics.plugin_calls_in_flight.add(-1, attrs);
        }
    }
}

fn class_label(class: Option<CallClass>) -> &'static str {
    class.map_or("lifecycle", CallClass::as_str)
}

/// Invocation surface: the ability to *call into* an already-loaded plugin.
///
/// Split out from [`PluginManager`] so host functions -- which run *inside* a
/// plugin call and only ever need to invoke other plugins (checker / language
/// delegation) -- can be handed an `Arc<dyn PluginInvoker>`. That narrows their
/// capability: an invoker cannot discover, load, unload, or reload plugins. The
/// accessors declared here are exactly the state that
/// [`call_raw`](PluginInvoker::call_raw) needs; the wider management surface lives on the [`PluginManager`] subtrait.
// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait PluginInvoker: Send + Sync {
    fn get_registry(&self) -> &PluginRegistry;

    fn get_config(&self) -> &PluginConfig;

    /// Admission and memory state shared by every call of this server.
    fn executor(&self) -> &Executor;

    fn get_metrics(&self) -> Option<&common::metrics::Metrics> {
        None
    }

    /// Gate every call passes through, so the host can refuse new calls and
    /// wait for running ones before it shuts down (see [`crate::call_gate`]).
    /// `None` for managers that never shut down mid-call (tests).
    fn call_gate(&self) -> Option<&crate::call_gate::CallGate> {
        None
    }

    /// Call `func_name` on `plugin_id` as `options` says; see
    /// [`crate::executor`] for how the call is admitted, run and bounded.
    #[instrument(skip(self, input, options), fields(plugin_id = %plugin_id, function = %func_name))]
    async fn call_raw(
        &self,
        plugin_id: &str,
        func_name: &str,
        input: Vec<u8>,
        options: CallOptions,
    ) -> Result<Vec<u8>, PluginError> {
        let start = std::time::Instant::now();
        // First, before anything else: once the host is shutting down every
        // call is refused the same way, whatever plugin it is for.
        let guard = match self.call_gate() {
            Some(gate) => Some(gate.enter().ok_or(PluginError::ShuttingDown)?),
            None => None,
        };
        let unavailable = || PluginError::Unavailable {
            plugin_id: plugin_id.to_string(),
            func_name: func_name.to_string(),
        };
        let resolved = crate::executor::resolve(
            options,
            self.get_config().call_timeout(),
            plugin_id,
            func_name,
        )?;
        let admission = self.executor().admission().clone();

        // Admission comes before the thread and before the pool lookup: a
        // queued call holds no thread, and one queued across a reload runs on
        // the new version.
        let admitted = match resolved.queue {
            Some(kind) => {
                let waited = std::time::Instant::now();
                let outcome = admission.admit(kind, resolved.deadline).await;
                if let Some(metrics) = self.get_metrics() {
                    if outcome
                        .as_ref()
                        .is_ok_and(|(admitted, _)| admitted.queued())
                    {
                        metrics
                            .plugin_admission_queued_total
                            .add(1, &[KeyValue::new("plugin.kind", kind.as_str())]);
                    }
                    metrics.plugin_admission_wait_duration.record(
                        waited.elapsed().as_secs_f64(),
                        &[
                            KeyValue::new("plugin.kind", kind.as_str()),
                            KeyValue::new(
                                "outcome",
                                if outcome.is_ok() {
                                    "admitted"
                                } else {
                                    "expired"
                                },
                            ),
                        ],
                    );
                }
                Some(outcome.map_err(|_| unavailable())?)
            }
            None => None,
        };

        let pool = {
            let registry = self.get_registry().read().map_err(|_| {
                PluginError::Internal("Failed to acquire registry read lock".into())
            })?;

            let plugin_entry = registry
                .get(plugin_id)
                .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

            if plugin_entry.status != PluginStatus::Loaded {
                return Err(PluginError::NotLoaded(plugin_id.to_string()));
            }

            plugin_entry
                .runtime
                .as_ref()
                .ok_or_else(|| PluginError::NoRuntime(plugin_id.to_string()))?
                .clone()
        };

        let metrics = self.get_metrics().cloned();
        let plugin_id_owned = plugin_id.to_string();
        let func_name_owned = func_name.to_string();
        let parent_span = tracing::Span::current();

        // Counted until the guest returns, whether or not anyone still
        // awaits it.
        let in_flight = InFlight::enter(metrics.as_ref(), &plugin_id_owned);
        let runtime = tokio::runtime::Handle::current();
        let (done, result) = tokio::sync::oneshot::channel();
        self.executor().threads().run({
            let plugin_id = plugin_id_owned.clone();
            let func_name = func_name_owned.clone();
            move || {
                let _in_flight = in_flight;
                // Host functions bridge back into the runtime with `block_on`.
                let _runtime = runtime.enter();
                let _ = done.send(run_guest(
                    guard,
                    parent_span,
                    admitted,
                    admission,
                    resolved,
                    pool,
                    plugin_id,
                    func_name,
                    input,
                ));
            }
        });
        let result = result
            .await
            .map_err(|_| PluginError::Internal("plugin guest thread panicked".into()));
        let result = result?;

        let duration = start.elapsed();
        if let Some(metrics) = self.get_metrics() {
            let outcome = if result.is_ok() { "success" } else { "error" };
            let attrs = [
                KeyValue::new("plugin.id", plugin_id.to_string()),
                KeyValue::new("plugin.function", func_name.to_string()),
                KeyValue::new("plugin.class", class_label(resolved.class)),
                KeyValue::new("outcome", outcome),
            ];
            metrics
                .plugin_call_duration
                .record(duration.as_secs_f64(), &attrs);
            if result.is_err() {
                metrics.plugin_call_failures.add(1, &attrs);
            }
            let host_fn_attrs = [
                KeyValue::new("host_fn", func_name.to_string()),
                KeyValue::new("outcome", outcome),
            ];
            metrics
                .host_fn_duration
                .record(duration.as_secs_f64(), &host_fn_attrs);
            metrics.host_fn_calls_total.add(1, &host_fn_attrs);
        }
        debug!(
            duration_ms = duration.as_millis() as u64,
            success = result.is_ok(),
            "plugin call completed"
        );

        result
    }
}

/// The part of a call that runs on its guest thread: take the token (or
/// resume one for a nested call), install the call scope, lease an instance
/// and run the guest under its deadline. Everything held here, the call gate
/// guard and the admission included, lives until the guest returns, even if
/// the awaiting task is cancelled.
#[allow(clippy::too_many_arguments)]
fn run_guest(
    guard: Option<crate::call_gate::CallGuard>,
    parent_span: tracing::Span,
    admitted: Option<(crate::admission::Admitted, Option<crate::admission::Token>)>,
    admission: crate::admission::Admission,
    resolved: crate::executor::Resolved,
    pool: crate::pool::InstancePool,
    plugin_id: String,
    func_name: String,
    input: Vec<u8>,
) -> Result<Vec<u8>, PluginError> {
    let _guard = guard;
    // Nested host-to-plugin calls from this thread are admitted.
    let _in_call = crate::call_gate::ThreadInCall::enter();
    let _span_guard = parent_span.enter();
    let (admitted, token) = match admitted {
        // A lifecycle call runs without a token.
        Some((admitted, token)) => (Some(admitted), token),
        // A nested call takes its token back the way a parked call
        // resumes.
        None => (
            None,
            resolved
                .class
                .and_then(|class| admission.resume(class, resolved.deadline)),
        ),
    };
    let _admitted = admitted;
    let _scope = ScopeGuard::enter(NewScope {
        admission: Some(admission),
        class: resolved.class,
        deadline: resolved.deadline,
        depth: resolved.depth,
        token,
    });

    let unavailable = || PluginError::Unavailable {
        plugin_id: plugin_id.clone(),
        func_name: func_name.clone(),
    };
    if std::time::Instant::now() >= resolved.deadline {
        return Err(unavailable());
    }
    let mut plugin = pool.get().map_err(|e| PluginError::InstanceBuild {
        plugin_id: plugin_id.clone(),
        message: e.to_string(),
    })?;
    if !plugin.function_exists(&func_name) {
        return Err(PluginError::FunctionNotFound {
            plugin_id: plugin_id.clone(),
            func_name: func_name.clone(),
        });
    }

    let input_len = input.len() as u64;
    let watch = crate::deadline::watch(plugin.cancel_handle(), resolved.deadline);
    let outcome = plugin.call::<_, Vec<u8>>(func_name.as_str(), input);
    let cancelled = watch.fired();
    drop(watch);
    let output_len = outcome.as_ref().map_or(0, |out| out.len() as u64);
    plugin.note_call(input_len + output_len);
    outcome.map_err(|e| {
        if cancelled {
            unavailable()
        } else {
            PluginError::ExecutionFailed {
                plugin_id: plugin_id.clone(),
                func_name: func_name.clone(),
                message: e.to_string(),
            }
        }
    })
}

/// Full plugin-management surface: discovery, the load / unload lifecycle,
/// i18n, web-asset resolution, and -- via the [`PluginInvoker`] supertrait --
/// invocation. Implemented by the process-wide manager. Depend on
/// [`PluginInvoker`] instead of this trait wherever a caller only needs to
/// invoke plugins, so it cannot reach the lifecycle methods.
pub trait PluginManager: PluginInvoker {
    fn get_host_functions(&self) -> &HostFunctionRegistry;

    fn get_i18n_registry(&self) -> &I18nRegistry;

    fn resolve(&self, manifest: &PluginManifest) -> Option<(String, Vec<String>)>;

    fn discover_plugins(&self) -> Result<(), PluginError> {
        let plugins_dir = &self.get_config().plugins_dir;

        if !plugins_dir.exists() || !plugins_dir.is_dir() {
            return Err(PluginError::DiscoveryFailed(format!(
                "Plugins directory '{}' does not exist or is not a directory",
                plugins_dir.display()
            )));
        }

        info!(
            "Discovering plugins in directory: {}",
            plugins_dir.display()
        );

        let entries = std::fs::read_dir(plugins_dir).map_err(PluginError::Io)?;
        let mut registry = self
            .get_registry()
            .write()
            .map_err(|_| PluginError::Internal("Failed to acquire registry write lock".into()))?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                PluginError::LoadFailed(format!("Failed to read plugin entry: {}", e))
            })?;
            if let Ok(file_type) = entry.file_type()
                && file_type.is_dir()
            {
                let id = entry.file_name().to_string_lossy().to_string();
                match PluginEntry::from_dir(&entry.path()) {
                    Ok(plugin_entry) => {
                        info!("Discovered plugin: {}", plugin_entry.manifest);
                        if plugin_entry.manifest.is_hollow() {
                            warn!(
                                "Plugin '{}' is hollow. It might be misconfigured.",
                                plugin_entry.id
                            );
                        }
                        registry.insert(id, plugin_entry);
                    }
                    Err(e) => {
                        error!("Failed to parse plugin '{}': {}", id, e);
                    }
                }
            }
        }

        Ok(())
    }

    /// The instance pool for `entry`, or `None` for a plugin with no server
    /// side. Builds no instance: they are built on demand.
    fn build_runtime(
        &self,
        plugin_id: &str,
        entry: &PluginEntry,
    ) -> Result<Option<crate::pool::InstancePool>, PluginError> {
        let Some((entry_point, permissions)) = self.resolve(&entry.manifest) else {
            return Ok(None);
        };
        let wasm_path = entry.root_dir.join(&entry_point);
        if !wasm_path.exists() {
            return Err(PluginError::LoadFailed(format!(
                "Wasm file not found at path: {}",
                wasm_path.display()
            )));
        }

        let budget = self.executor().budget().clone();
        // A runaway trap on one instance's linear memory, so an unbounded
        // allocation fails its own call instead of exhausting the
        // server. Far above any legitimate working set. Each call's
        // deadline is enforced by the deadline thread; the manifest
        // timeout, a margin past the longest deadline any call gets, is
        // the backstop for a guest the deadline thread never cancelled.
        let manifest = Manifest::new([Wasm::file(&wasm_path)])
            .with_memory_max(budget.instance_memory_pages())
            .with_timeout(
                self.get_config().call_timeout() + crate::config::TIMEOUT_BACKSTOP_MARGIN,
            );
        let host_functions = self.get_host_functions().resolve(plugin_id, &permissions);
        let wasi = self.get_config().enable_wasi;
        let compile_cache = crate::config::compile_cache_config(&self.get_config().cache_dir);

        let build_metrics = self.get_metrics().cloned();
        let build_plugin_id = plugin_id.to_string();
        let source: crate::pool::PluginSource = std::sync::Arc::new(move || {
            let started = std::time::Instant::now();
            let builder = PluginBuilder::new(&manifest)
                .with_wasi(wasi)
                .with_functions(host_functions.clone());
            let builder = match &compile_cache {
                Some(config) => builder.with_cache_config(config),
                None => builder,
            };
            let built = builder.build();
            if let Some(metrics) = build_metrics.as_ref() {
                let attrs = [KeyValue::new("plugin.id", build_plugin_id.clone())];
                metrics
                    .plugin_instance_create_duration
                    .record(started.elapsed().as_secs_f64(), &attrs);
                metrics.plugin_instance_created_total.add(1, &attrs);
            }
            built
        });
        Ok(Some(crate::pool::InstancePool::new(
            plugin_id,
            source,
            budget,
            self.get_metrics().cloned(),
        )))
    }

    #[instrument(skip(self), fields(plugin_id = %plugin_id))]
    fn load_plugin(&self, plugin_id: &str) -> Result<(), PluginError> {
        let mut registry = self
            .get_registry()
            .write()
            .map_err(|_| PluginError::Internal("Failed to acquire registry write lock".into()))?;

        let plugin_entry = registry
            .get_mut(plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        if plugin_entry.status == PluginStatus::Loaded {
            return Ok(());
        }

        plugin_entry.runtime = self.build_runtime(plugin_id, plugin_entry)?;
        plugin_entry.status = PluginStatus::Loaded;

        info!("Plugin {} loaded successfully", plugin_entry.manifest);
        Ok(())
    }

    #[instrument(skip(self), fields(plugin_id = %plugin_id))]
    fn unload_plugin(&self, plugin_id: &str) -> Result<(), PluginError> {
        let mut registry = self
            .get_registry()
            .write()
            .map_err(|_| PluginError::Internal("Failed to acquire registry write lock".into()))?;

        let plugin_entry = registry
            .get_mut(plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        plugin_entry.runtime = None;
        plugin_entry.status = PluginStatus::Unloaded;

        info!("Plugin {} unloaded successfully", plugin_entry.manifest);

        Ok(())
    }

    fn is_plugin_loaded(&self, plugin_id: &str) -> Result<bool, PluginError> {
        let registry = self
            .get_registry()
            .read()
            .map_err(|_| PluginError::Internal("Failed to acquire registry read lock".into()))?;

        let plugin_entry = registry
            .get(plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;

        Ok(plugin_entry.status == PluginStatus::Loaded)
    }

    fn list_plugins(&self) -> Result<Vec<PluginInfo>, PluginError> {
        let registry = self
            .get_registry()
            .read()
            .map_err(|_| PluginError::Internal("Failed to acquire registry read lock".into()))?;

        Ok(registry.values().map(PluginInfo::from).collect())
    }

    fn has_plugin(&self, plugin_id: &str) -> Result<bool, PluginError> {
        let registry = self
            .get_registry()
            .read()
            .map_err(|_| PluginError::Internal("Failed to acquire registry read lock".into()))?;

        Ok(registry.contains_key(plugin_id))
    }

    fn resolve_plugin_asset(
        &self,
        plugin_id: &str,
        asset_path: &str,
    ) -> Result<std::path::PathBuf, AssetError> {
        let registry = self
            .get_registry()
            .read()
            .map_err(|_| AssetError::Internal("Failed to acquire registry read lock".into()))?;

        let plugin_entry = registry.get(plugin_id).ok_or(AssetError::NotFound)?;
        plugin_entry.resolve_web_asset(asset_path)
    }

    fn update_translations(&self) -> Result<(), PluginError> {
        let registry = self
            .get_registry()
            .read()
            .map_err(|_| PluginError::Internal("Failed to acquire registry read lock".into()))?;

        let i18n = self.get_i18n_registry();
        i18n.clear();

        let loaded_plugins = registry
            .values()
            .filter(|e| e.status == PluginStatus::Loaded);
        for plugin_entry in loaded_plugins {
            for (lang, path) in &plugin_entry.manifest.translations {
                let translation_path = plugin_entry.root_dir.join(path);
                let content = std::fs::read_to_string(&translation_path).map_err(|e| {
                    PluginError::LoadFailed(format!(
                        "Failed to read translation file '{}': {}",
                        translation_path.display(),
                        e
                    ))
                })?;
                let translation: TranslationMap = toml::from_str(&content).map_err(|e| {
                    PluginError::LoadFailed(format!(
                        "Invalid translation file syntax in '{}': {}",
                        translation_path.display(),
                        e
                    ))
                })?;
                i18n.merge(lang.clone(), translation);
            }
        }

        Ok(())
    }

    fn reload_plugin(&self, plugin_id: &str) -> Result<(), PluginError> {
        let root_dir = {
            let registry = self.get_registry().read().map_err(|_| {
                PluginError::Internal("Failed to acquire registry read lock".into())
            })?;
            let entry = registry
                .get(plugin_id)
                .ok_or_else(|| PluginError::NotFound(plugin_id.to_string()))?;
            entry.root_dir.clone()
        };

        // Built in full before it replaces the old entry, so a call made
        // during the reload finds the old version or the new one, never an
        // unloaded plugin.
        let mut new_entry = PluginEntry::from_dir(&root_dir)?;
        new_entry.runtime = self.build_runtime(plugin_id, &new_entry)?;
        new_entry.status = PluginStatus::Loaded;
        info!("Plugin {} reloaded", new_entry.manifest);

        let mut registry = self
            .get_registry()
            .write()
            .map_err(|_| PluginError::Internal("Failed to acquire registry write lock".into()))?;
        registry.insert(plugin_id.to_string(), new_entry);
        Ok(())
    }

    fn rediscover_plugins(&self) -> Result<Vec<String>, PluginError> {
        let plugins_dir = &self.get_config().plugins_dir;

        if !plugins_dir.exists() || !plugins_dir.is_dir() {
            return Err(PluginError::DiscoveryFailed(format!(
                "Plugins directory '{}' does not exist or is not a directory",
                plugins_dir.display()
            )));
        }

        let entries = std::fs::read_dir(plugins_dir).map_err(PluginError::Io)?;
        let mut registry = self
            .get_registry()
            .write()
            .map_err(|_| PluginError::Internal("Failed to acquire registry write lock".into()))?;
        let mut new_ids = Vec::new();

        for entry in entries {
            let entry = entry.map_err(|e| {
                PluginError::LoadFailed(format!("Failed to read plugin entry: {}", e))
            })?;
            if let Ok(file_type) = entry.file_type()
                && file_type.is_dir()
            {
                let id = entry.file_name().to_string_lossy().to_string();
                if registry.contains_key(&id) {
                    continue;
                }
                match PluginEntry::from_dir(&entry.path()) {
                    Ok(plugin_entry) => {
                        info!("Discovered new plugin: {}", plugin_entry.manifest);
                        registry.insert(id.clone(), plugin_entry);
                        new_ids.push(id);
                    }
                    Err(e) => {
                        error!("Failed to parse plugin '{}': {}", id, e);
                    }
                }
            }
        }

        Ok(new_ids)
    }
}

#[cfg(test)]
mod tests;
