use std::time::Duration;

use async_trait::async_trait;
use extism::{Manifest, PluginBuilder, Wasm};
use opentelemetry::KeyValue;
use serde::{Serialize, de::DeserializeOwned};
use tracing::{debug, error, info, instrument, warn};

use crate::config::PluginConfig;
use crate::error::{AssetError, PluginError};
use crate::host::HostFunctionRegistry;
use crate::i18n::{I18nRegistry, TranslationMap};
use crate::manifest::PluginManifest;
use crate::registry::{PluginEntry, PluginInfo, PluginRegistry, PluginStatus};

#[derive(Clone, Copy)]
enum PoolAcquireOutcome {
    Success,
    Timeout,
    Error,
}

impl PoolAcquireOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Timeout => "timeout",
            Self::Error => "error",
        }
    }

    fn failure_kind(self) -> Option<&'static str> {
        match self {
            Self::Success => None,
            Self::Timeout => Some("timeout"),
            Self::Error => Some("error"),
        }
    }
}

fn pool_contention_bucket(elapsed: Duration) -> Option<&'static str> {
    let elapsed_ms = elapsed.as_secs_f64() * 1_000.0;
    if elapsed_ms >= 5_000.0 {
        Some("5s")
    } else if elapsed_ms >= 1_000.0 {
        Some("1s")
    } else if elapsed_ms >= 200.0 {
        Some("200ms")
    } else if elapsed_ms >= 50.0 {
        Some("50ms")
    } else {
        None
    }
}

fn record_pool_acquire_metrics(
    metrics: Option<&common::metrics::Metrics>,
    plugin_id: &str,
    func_name: &str,
    outcome: PoolAcquireOutcome,
    elapsed: Duration,
) {
    let Some(metrics) = metrics else {
        return;
    };

    metrics.plugin_instance_acquire_duration.record(
        elapsed.as_secs_f64(),
        &[
            KeyValue::new("plugin.id", plugin_id.to_string()),
            KeyValue::new("plugin.function", func_name.to_string()),
            KeyValue::new("outcome", outcome.as_str()),
        ],
    );

    if let Some(failure_kind) = outcome.failure_kind() {
        metrics.plugin_instance_acquire_failures.add(
            1,
            &[
                KeyValue::new("plugin.id", plugin_id.to_string()),
                KeyValue::new("plugin.function", func_name.to_string()),
                KeyValue::new("failure.kind", failure_kind),
            ],
        );
    }

    if let Some(bucket) = pool_contention_bucket(elapsed) {
        metrics.plugin_pool_contention_total.add(
            1,
            &[
                KeyValue::new("plugin.id", plugin_id.to_string()),
                KeyValue::new("plugin.function", func_name.to_string()),
                KeyValue::new("bucket", bucket),
            ],
        );
    }
}

/// Invocation surface: the ability to *call into* an already-loaded plugin.
///
/// Split out from [`PluginManager`] so host functions -- which run *inside* a
/// plugin call and only ever need to invoke other plugins (checker / language
/// delegation) -- can be handed an `Arc<dyn PluginInvoker>`. That narrows their
/// capability: an invoker cannot discover, load, unload, or reload plugins. The
/// accessors declared here are exactly the state that
/// [`call_raw_with_host_context`](PluginInvoker::call_raw_with_host_context)
/// needs; the wider management surface lives on the [`PluginManager`] subtrait.
#[async_trait]
pub trait PluginInvoker: Send + Sync {
    fn get_registry(&self) -> &PluginRegistry;

    fn get_config(&self) -> &PluginConfig;

    fn get_metrics(&self) -> Option<&common::metrics::Metrics> {
        None
    }

    async fn call_raw(
        &self,
        plugin_id: &str,
        func_name: &str,
        input: Vec<u8>,
    ) -> Result<Vec<u8>, PluginError> {
        self.call_raw_with_host_context(plugin_id, func_name, input, None)
            .await
    }

    #[instrument(skip(self, input, host_context), fields(plugin_id = %plugin_id, function = %func_name))]
    async fn call_raw_with_host_context(
        &self,
        plugin_id: &str,
        func_name: &str,
        input: Vec<u8>,
        host_context: Option<serde_json::Value>,
    ) -> Result<Vec<u8>, PluginError> {
        let start = std::time::Instant::now();

        // How long to wait for a free pooled instance. Distinct from the
        // guest execution deadline (`call_timeout_secs`), which is enforced by
        // extism via the manifest timeout set in `load_plugin`.
        let timeout = self.get_config().pool_acquire_timeout();
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

        let result = tokio::task::spawn_blocking({
            let plugin_id = plugin_id_owned.clone();
            let func_name = func_name_owned.clone();
            let metrics = metrics.clone();
            move || {
                let _span_guard = parent_span.enter();
                crate::host_context::with_context(host_context, || {
                    let acquire_start = std::time::Instant::now();
                    let mut plugin = match pool.get(timeout) {
                        Ok(Some(plugin)) => {
                            record_pool_acquire_metrics(
                                metrics.as_ref(),
                                &plugin_id,
                                &func_name,
                                PoolAcquireOutcome::Success,
                                acquire_start.elapsed(),
                            );
                            plugin
                        }
                        Ok(None) => {
                            record_pool_acquire_metrics(
                                metrics.as_ref(),
                                &plugin_id,
                                &func_name,
                                PoolAcquireOutcome::Timeout,
                                acquire_start.elapsed(),
                            );
                            return Err(PluginError::PoolTimeout(plugin_id.clone()));
                        }
                        Err(e) => {
                            record_pool_acquire_metrics(
                                metrics.as_ref(),
                                &plugin_id,
                                &func_name,
                                PoolAcquireOutcome::Error,
                                acquire_start.elapsed(),
                            );
                            return Err(PluginError::Internal(format!(
                                "Failed to acquire runtime instance for plugin '{}': {}",
                                plugin_id, e
                            )));
                        }
                    };

                    if !plugin.function_exists(&func_name) {
                        return Err(PluginError::FunctionNotFound {
                            plugin_id: plugin_id.clone(),
                            func_name: func_name.clone(),
                        });
                    }

                    let input_len = input.len();
                    // Reset the per-call streamed-byte counter; host functions
                    // (e.g. blob_read_range) add to it during the call so the
                    // pool can weigh how much data this instance churned through.
                    crate::host_context::reset_stream_bytes();
                    let call_result: Result<Vec<u8>, PluginError> = plugin
                        .call::<_, Vec<u8>>(func_name.as_str(), input)
                        .map_err(|e| PluginError::ExecutionFailed {
                            plugin_id: plugin_id.clone(),
                            func_name: func_name.clone(),
                            message: e.to_string(),
                        });
                    // Heap-profile hook (opt-in via BROCCOLI_PROFILE_PLUGIN_IO): record the
                    // bytes marshalled in/out of WASM per plugin call. Copying input/output
                    // across the host<->guest boundary grows the pooled instance's WASM
                    // linear memory, which the pool never reclaims - the suspected driver of
                    // the per-testcase RSS growth. Generic; no plugin-specific knowledge.
                    let output_len = call_result.as_ref().map(|o| o.len()).unwrap_or(0);
                    if std::env::var_os("BROCCOLI_PROFILE_PLUGIN_IO").is_some() {
                        info!(
                            target: "plugin_io_profile",
                            plugin_id = %plugin_id,
                            func = %func_name,
                            input_bytes = input_len,
                            output_bytes = output_len,
                            "plugin call I/O"
                        );
                    }

                    // Account this call's data volume against the instance and
                    // recycle it if it has churned through enough to have grown
                    // its (never-shrinking) WASM linear memory. The streamed
                    // bytes - data pulled in via host functions like
                    // blob_read_range - are the real driver; call I/O is added
                    // for completeness.
                    let processed_bytes =
                        input_len as u64 + output_len as u64 + crate::host_context::stream_bytes();
                    if plugin.note_call(processed_bytes) {
                        debug!(
                            plugin_id = %plugin_id,
                            func = %func_name,
                            processed_bytes,
                            "Recycled plugin instance to reclaim WASM linear memory"
                        );
                        if let Some(metrics) = metrics.as_ref() {
                            metrics
                                .plugin_instance_recycled_total
                                .add(1, &[KeyValue::new("plugin.id", plugin_id.clone())]);
                        }
                    }
                    call_result
                })
            }
        })
        .await
        .map_err(|e| PluginError::Internal(format!("plugin task join failed: {e}")))?;

        let duration = start.elapsed();
        if let Some(metrics) = self.get_metrics() {
            let outcome = if result.is_ok() { "success" } else { "error" };
            let attrs = [
                KeyValue::new("plugin.id", plugin_id.to_string()),
                KeyValue::new("plugin.function", func_name.to_string()),
                KeyValue::new("outcome", outcome),
            ];
            metrics
                .plugin_call_duration
                .record(duration.as_secs_f64(), &attrs);
            if result.is_err() {
                metrics.plugin_call_failures.add(1, &attrs);
            }

            // UP#13: also expose per-host-fn duration + call totals. After UP#1
            // the spawn_blocking above is where every plugin entry/exit can be
            // observed; we tag with the function name so dashboards can break
            // out latency by host_fn (e.g. "evaluate.start_batch").
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

        Ok(result?)
    }
}

#[async_trait]
pub trait PluginInvokerExt: PluginInvoker {
    async fn call<T, R>(&self, plugin_id: &str, func_name: &str, input: T) -> Result<R, PluginError>
    where
        T: Serialize + Send + Sync,
        R: DeserializeOwned + Send + Sync,
    {
        let input_bytes = serde_json::to_vec(&input)?;

        let output_bytes = self.call_raw(plugin_id, func_name, input_bytes).await?;

        let result = serde_json::from_slice(&output_bytes)?;
        Ok(result)
    }
}

impl<T: ?Sized + PluginInvoker> PluginInvokerExt for T {}

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

    /// Drop pooled instances that have sat unused for `idle_for`, across
    /// every loaded plugin. Returns how many were dropped. Blocking: frees
    /// WASM memory, so run it off the async runtime.
    fn evict_idle_instances(&self, idle_for: std::time::Duration) -> usize {
        let pools: Vec<(String, crate::pool::RecyclingPool)> = {
            let registry = self.get_registry().read().unwrap();
            registry
                .values()
                .filter_map(|entry| Some((entry.id.clone(), entry.runtime.clone()?)))
                .collect()
        };
        let mut total = 0;
        for (plugin_id, pool) in pools {
            let evicted = pool.evict_idle(idle_for);
            if evicted > 0 {
                debug!(plugin_id = %plugin_id, evicted, "Evicted idle plugin instances");
                if let Some(metrics) = self.get_metrics() {
                    metrics
                        .plugin_instance_evicted_total
                        .add(evicted as u64, &[KeyValue::new("plugin.id", plugin_id)]);
                }
            }
            total += evicted;
        }
        total
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

        let mut runtime = None;
        if let Some((entry_point, permissions)) = self.resolve(&plugin_entry.manifest) {
            let wasm_path = plugin_entry.root_dir.join(&entry_point);
            if !wasm_path.exists() {
                return Err(PluginError::LoadFailed(format!(
                    "Wasm file not found at path: {}",
                    wasm_path.display()
                )));
            }

            let mut manifest = Manifest::new([Wasm::file(&wasm_path)]);
            // Defense-in-depth: cap each instance's WASM linear memory so a runaway
            // instance fails its own testcase instead of OOM-killing the server.
            // Generic (no plugin-specific knowledge); the default is far above any
            // legitimate working set so it never traps a real testcase.
            if let Some(pages) = self.get_config().max_instance_memory_pages {
                manifest = manifest.with_memory_max(pages);
            }
            // Same defense on the time axis: bound every guest call's execution.
            // extism's timer thread bumps the store epoch when the deadline
            // passes, the running call traps with "timeout", and the pooled
            // instance is released. Without this, one hung export (infinite
            // loop on pathological input) would pin a pool slot and a blocking
            // thread forever; `pool_max_instances` such hangs would wedge the
            // plugin's pool for the rest of the contest. Applied to the shared
            // manifest so both pool-built and recycled instances inherit it.
            manifest = manifest.with_timeout(self.get_config().call_timeout());
            let host_functions = self.get_host_functions().resolve(plugin_id, &permissions);
            let wasi = self.get_config().enable_wasi;
            let max_instances = self.get_config().pool_max_instances;
            let reclaim_bytes = self.get_config().instance_reclaim_bytes;
            let min_calls = self.get_config().instance_min_calls_before_recycle;

            // One factory builds instances on demand and rebuilds bloated ones.
            let build_metrics = self.get_metrics().cloned();
            let build_plugin_id = plugin_id.to_string();
            let source: crate::pool::PluginSource = std::sync::Arc::new(move || {
                let started = std::time::Instant::now();
                let built = PluginBuilder::new(&manifest)
                    .with_wasi(wasi)
                    .with_functions(host_functions.clone())
                    .build();
                if let Some(metrics) = build_metrics.as_ref() {
                    let attrs = [KeyValue::new("plugin.id", build_plugin_id.clone())];
                    metrics
                        .plugin_instance_create_duration
                        .record(started.elapsed().as_secs_f64(), &attrs);
                    metrics.plugin_instance_created_total.add(1, &attrs);
                }
                built
            });
            runtime = Some(crate::pool::RecyclingPool::new(
                source,
                max_instances,
                reclaim_bytes,
                min_calls,
            ));
        }

        plugin_entry.runtime = runtime;
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

        let new_entry = PluginEntry::from_dir(&root_dir)?;

        {
            let mut registry = self.get_registry().write().map_err(|_| {
                PluginError::Internal("Failed to acquire registry write lock".into())
            })?;
            registry.insert(plugin_id.to_string(), new_entry);
        }

        self.load_plugin(plugin_id)?;
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
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn pool_contention_bucket_ignores_fast_acquires() {
        assert_eq!(pool_contention_bucket(Duration::from_millis(49)), None);
    }

    #[test]
    fn pool_contention_bucket_uses_largest_exceeded_threshold() {
        assert_eq!(
            pool_contention_bucket(Duration::from_millis(50)),
            Some("50ms")
        );
        assert_eq!(
            pool_contention_bucket(Duration::from_millis(250)),
            Some("200ms")
        );
        assert_eq!(
            pool_contention_bucket(Duration::from_millis(1_500)),
            Some("1s")
        );
        assert_eq!(
            pool_contention_bucket(Duration::from_millis(5_500)),
            Some("5s")
        );
    }

    #[test]
    fn pool_acquire_metrics_records_all_pool_get_outcomes() {
        let (metrics, registry) =
            common::observability::init_metrics("broccoli-plugin-core-pool-test");

        record_pool_acquire_metrics(
            Some(&metrics),
            "evaluator",
            "evaluate",
            PoolAcquireOutcome::Success,
            Duration::from_millis(1),
        );
        record_pool_acquire_metrics(
            Some(&metrics),
            "evaluator",
            "evaluate",
            PoolAcquireOutcome::Timeout,
            Duration::from_millis(250),
        );
        record_pool_acquire_metrics(
            Some(&metrics),
            "evaluator",
            "evaluate",
            PoolAcquireOutcome::Error,
            Duration::from_millis(5),
        );

        let families = registry.gather();
        let acquire_duration = families
            .iter()
            .find(|family| family.name() == "broccoli_plugin_instance_acquire_duration_seconds")
            .expect("plugin instance acquire duration should be exported");
        for outcome in ["success", "timeout", "error"] {
            assert!(
                acquire_duration.get_metric().iter().any(|metric| metric
                    .get_label()
                    .iter()
                    .any(|label| label.name() == "outcome" && label.value() == outcome)),
                "acquire duration should include outcome={outcome}"
            );
        }

        let acquire_failures = families
            .iter()
            .find(|family| family.name() == "broccoli_plugin_instance_acquire_failures_total")
            .expect("plugin instance acquire failures should be exported");
        for failure_kind in ["timeout", "error"] {
            assert!(
                acquire_failures
                    .get_metric()
                    .iter()
                    .any(|metric| metric.get_label().iter().any(|label| label.name()
                        == "failure_kind"
                        && label.value() == failure_kind)),
                "acquire failures should include failure_kind={failure_kind}"
            );
        }

        let pool_contention = families
            .iter()
            .find(|family| family.name() == "broccoli_plugin_pool_contention_total")
            .expect("plugin pool contention counter should be exported");
        let bucketed_counter = pool_contention
            .get_metric()
            .iter()
            .find(|metric| {
                metric
                    .get_label()
                    .iter()
                    .any(|label| label.name() == "bucket" && label.value() == "200ms")
            })
            .expect("pool contention should include bucket=200ms");
        assert_eq!(bucketed_counter.get_counter().value(), 1.0);
    }
}
