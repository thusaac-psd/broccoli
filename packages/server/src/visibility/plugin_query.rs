//! Batched plugin visibility query - the point where an UNTRUSTED plugin
//! response enters the host's access-control decision.
//!
//! [`query_plugins`] is **infallible by design**: it returns `Vec<Decision>`,
//! never a `Result`. Every failure mode - a poisoned plugin-registry lock, a
//! plugin timeout/trap/pool-acquire failure, non-JSON or unparseable output,
//! a `decisions` vector whose length doesn't match the resource batch (in
//! EITHER direction), or an unrecognized decision variant - collapses to
//! `Decision::Deny` for every resource in the batch. This mirrors the host's
//! established fail-closed hook stance, already pinned by
//! `plugin_core::hook`'s `non_json_output_fails_closed_reject` and
//! `empty_output_fails_closed_reject` tests: a failed visibility query must
//! become a 404 to the end user, never a 500 (a 500 both leaks that plugin
//! logic exists and takes the scoreboard down mid-contest).
//!
//! The pure mapping ([`decisions_from_output`]) is kept separate from the
//! async plugin call so the failure modes are unit-testable without a
//! plugin host.
//!
//! Multiple plugins may register `[[server.queries]] topic = "visibility"`.
//! Each is queried independently with an identical [`VisibilityQueryInput`],
//! and the per-plugin decision vectors are combined element-wise with
//! [`Decision::meet`] - associative and commutative (Task 2), so the
//! registry's `HashMap`-backed (i.e. unordered) iteration order cannot
//! affect the result.
//!
//! Not yet called from any handler - Task 7 (`decide_batch`) wires this in
//! alongside `host_rules::host_decide`. `#[allow(dead_code)]` is temporary
//! and should come off once that wiring lands.

use broccoli_server_sdk::types::{
    QueryContext, QueryResource, QuerySubject, VisibilityQueryInput, VisibilityQueryOutput,
    WireDecision,
};
use plugin_core::registry::PluginStatus;
use plugin_core::traits::PluginInvokerExt;

use crate::state::AppState;

use super::{Action, Decision, FieldMask, Resource, Subject};

/// One plugin that declared a `[[server.queries]]` entry with
/// `topic = "visibility"` in its manifest: which plugin, and which function
/// to call.
struct VisibilityQuerier {
    plugin_id: String,
    function: String,
}

/// Scan the live plugin registry for every currently-`Loaded` plugin that
/// registered a `[[server.queries]]` entry with `topic = "visibility"`.
///
/// Returns `Err(())` only if the registry's `RwLock` is poisoned (a prior
/// panic while some other code held the write lock) - the caller treats that
/// exactly like any other plugin-side failure and fails the whole batch
/// closed, since a poisoned lock means the set of registered queriers can no
/// longer be established with confidence.
fn visibility_queriers(state: &AppState) -> Result<Vec<VisibilityQuerier>, ()> {
    let registry = state.plugins.get_registry().read().map_err(|_| ())?;

    let mut queriers = Vec::new();
    for entry in registry.values() {
        if entry.status != PluginStatus::Loaded {
            continue;
        }
        let Some(server) = &entry.manifest.server else {
            continue;
        };
        for query in &server.queries {
            if query.topic == "visibility" {
                queriers.push(VisibilityQuerier {
                    plugin_id: entry.id.clone(),
                    function: query.function.clone(),
                });
            }
        }
    }
    Ok(queriers)
}

/// Map a plugin response to decisions. Every failure mode collapses to
/// all-`Deny` — matching the host's established fail-closed hook stance
/// (`plugin_core::hook` tests `non_json_output_fails_closed_reject` and
/// `empty_output_fails_closed_reject`).
fn decisions_from_output(
    output: Result<VisibilityQueryOutput, String>,
    expected_len: usize,
) -> Vec<Decision> {
    let deny_all = || vec![Decision::Deny; expected_len];

    let Ok(out) = output else { return deny_all() };
    if out.decisions.len() != expected_len {
        return deny_all();
    }
    out.decisions
        .into_iter()
        .map(|d| match d {
            WireDecision::Allow {} => Decision::Allow,
            WireDecision::Deny {} => Decision::Deny,
            WireDecision::Redact { fields } => Decision::Redact(FieldMask::new(fields)),
        })
        .collect()
}

/// Combine two same-length per-resource decision vectors positionally with
/// [`Decision::meet`]. Split out so order-independence (required because
/// multiple plugins are folded together, and the registry they come from is
/// a `HashMap`) is unit-testable on its own, without needing a plugin host.
fn meet_positionally(acc: Vec<Decision>, next: Vec<Decision>) -> Vec<Decision> {
    debug_assert_eq!(acc.len(), next.len());
    acc.into_iter().zip(next).map(|(a, b)| a.meet(b)).collect()
}

/// Batched, per-plugin visibility query. See the module docs for the
/// fail-closed contract. Returns one [`Decision`] per entry of `resources`,
/// in the same order — `Decision::Allow` for every resource when no plugin
/// has registered `topic = "visibility"` (vacuously true: no plugin
/// restricts anything further), which also means no plugin is called at all
/// when the batch is empty or no queriers are registered.
#[allow(dead_code)]
pub(crate) async fn query_plugins(
    state: &AppState,
    subject: &Subject,
    action: Action,
    contest_id: Option<i32>,
    resources: &[Resource],
) -> Vec<Decision> {
    if resources.is_empty() {
        return Vec::new();
    }

    let queriers = match visibility_queriers(state) {
        Ok(queriers) => queriers,
        Err(()) => {
            tracing::error!("visibility plugin registry lock poisoned; denying batch");
            return vec![Decision::Deny; resources.len()];
        }
    };

    if queriers.is_empty() {
        return vec![Decision::Allow; resources.len()];
    }

    let input = VisibilityQueryInput {
        subject: QuerySubject {
            user_id: subject.user_id,
            authenticated: subject.authenticated,
            permissions: subject.permissions.clone(),
        },
        action: action.as_wire().to_string(),
        context: QueryContext { contest_id },
        resources: resources
            .iter()
            .map(|r| QueryResource {
                kind: r.wire_kind().to_string(),
                id: r.wire_id(),
            })
            .collect(),
    };

    let mut combined = vec![Decision::Allow; resources.len()];
    for querier in &queriers {
        let call_result: Result<VisibilityQueryOutput, _> = state
            .plugins
            .call(&querier.plugin_id, &querier.function, &input)
            .await;

        let output = match call_result {
            Ok(out) => Ok(out),
            Err(e) => {
                tracing::error!(
                    plugin_id = %querier.plugin_id,
                    func = %querier.function,
                    error = %e,
                    "visibility query plugin call failed"
                );
                Err(e.to_string())
            }
        };

        let decisions = decisions_from_output(output, resources.len());
        combined = meet_positionally(combined, decisions);
    }

    combined
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};

    use common::storage::filesystem::FilesystemBlobStore;
    use plugin_core::config::PluginConfig;
    use plugin_core::error::PluginError;
    use plugin_core::host::HostFunctionRegistry;
    use plugin_core::i18n::I18nRegistry;
    use plugin_core::manifest::{PluginManifest, ServerConfig as ManifestServerConfig, ServerQuery};
    use plugin_core::registry::{PluginEntry, PluginRegistry};
    use plugin_core::traits::{PluginInvoker, PluginManager};
    use sea_orm::{DatabaseBackend, MockDatabase};

    use crate::config::{
        AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
        MqAppConfig, ServerConfig, SubmissionConfig,
    };
    use crate::dispatcher::permits::DispatcherSemaphore;
    use crate::registry::{
        CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
        LanguageResolverRegistry, OperationBatches, OperationWaiters,
    };
    use crate::state::RegistryState;

    fn mask(fields: &[&str]) -> FieldMask {
        FieldMask::new(fields.iter().map(|s| s.to_string()))
    }

    // ---------------------------------------------------------------------
    // decisions_from_output - pure, no plugin host needed
    // ---------------------------------------------------------------------

    #[test]
    fn length_mismatch_denies_whole_batch() {
        let out = VisibilityQueryOutput {
            decisions: vec![WireDecision::Allow {}],
        };
        assert_eq!(
            decisions_from_output(Ok(out), 3),
            vec![Decision::Deny, Decision::Deny, Decision::Deny],
            "a short decision vector is a protocol violation, not a partial answer"
        );
    }

    #[test]
    fn decisions_longer_than_resources_denies_whole_batch() {
        // The brief only pins the SHORT case; a plugin that answers for more
        // resources than were asked about is just as much a protocol
        // violation as answering for fewer, and must fail closed the same
        // way - never truncated and treated as a partial answer.
        let out = VisibilityQueryOutput {
            decisions: vec![
                WireDecision::Allow {},
                WireDecision::Allow {},
                WireDecision::Deny {},
            ],
        };
        assert_eq!(
            decisions_from_output(Ok(out), 2),
            vec![Decision::Deny, Decision::Deny],
            "a long decision vector is also a protocol violation, not a partial answer"
        );
    }

    #[test]
    fn plugin_error_denies_whole_batch() {
        assert_eq!(
            decisions_from_output(Err("trap".into()), 2),
            vec![Decision::Deny, Decision::Deny]
        );
    }

    #[test]
    fn well_formed_output_maps_positionally() {
        let out = VisibilityQueryOutput {
            decisions: vec![
                WireDecision::Allow {},
                WireDecision::Redact {
                    fields: vec!["result.verdict".into()],
                },
                WireDecision::Deny {},
            ],
        };
        assert_eq!(
            decisions_from_output(Ok(out), 3),
            vec![
                Decision::Allow,
                Decision::Redact(FieldMask::new(["result.verdict".to_string()])),
                Decision::Deny,
            ]
        );
    }

    // ---------------------------------------------------------------------
    // meet_positionally - pure, no plugin host needed
    // ---------------------------------------------------------------------

    #[test]
    fn combining_two_plugin_outputs_is_order_independent() {
        // query_plugins folds one plugin's decisions into another's with
        // `meet`, and the registry it iterates is a HashMap (unordered) -
        // the fold MUST NOT depend on which plugin's output is folded first.
        let a = vec![
            Decision::Allow,
            Decision::Deny,
            Decision::Redact(mask(&["x"])),
        ];
        let b = vec![
            Decision::Redact(mask(&["y"])),
            Decision::Allow,
            Decision::Allow,
        ];

        let a_then_b = meet_positionally(a.clone(), b.clone());
        let b_then_a = meet_positionally(b, a);

        assert_eq!(a_then_b, b_then_a);
        assert_eq!(
            a_then_b,
            vec![
                // meet(Allow, Redact(y)) = Redact(y) - Allow contributes no
                // fields, it's the identity element.
                Decision::Redact(mask(&["y"])),
                // meet(Deny, Allow) = Deny.
                Decision::Deny,
                // meet(Redact(x), Allow) = Redact(x).
                Decision::Redact(mask(&["x"])),
            ]
        );
    }

    // ---------------------------------------------------------------------
    // query_plugins - needs an AppState, so a minimal one is built here.
    // Modeled on `dispatcher::steal::tests::NoopPluginManager` (the only
    // existing precedent in this crate for constructing an AppState in a
    // unit test), but this manager PANICS on any call instead of returning
    // an error, so these tests also serve as a hard check that
    // `query_plugins` does not reach for the plugin host at all when it has
    // no work to do.
    // ---------------------------------------------------------------------

    struct PanicPluginManager {
        registry: PluginRegistry,
        config: PluginConfig,
        host_functions: HostFunctionRegistry,
        i18n: I18nRegistry,
    }

    #[async_trait::async_trait]
    impl PluginInvoker for PanicPluginManager {
        fn get_registry(&self) -> &PluginRegistry {
            &self.registry
        }

        fn get_config(&self) -> &PluginConfig {
            &self.config
        }

        async fn call_raw(
            &self,
            plugin_id: &str,
            func_name: &str,
            _input: Vec<u8>,
        ) -> Result<Vec<u8>, PluginError> {
            panic!(
                "query_plugins must not call plugin '{plugin_id}'::{func_name} here \
                 (no resources, or no registered visibility querier)"
            );
        }
    }

    impl PluginManager for PanicPluginManager {
        fn get_host_functions(&self) -> &HostFunctionRegistry {
            &self.host_functions
        }

        fn get_i18n_registry(&self) -> &I18nRegistry {
            &self.i18n
        }

        fn resolve(&self, _manifest: &PluginManifest) -> Option<(String, Vec<String>)> {
            None
        }
    }

    /// A `Loaded` plugin entry that registers one `visibility` query.
    fn visibility_plugin_entry(plugin_id: &str) -> PluginEntry {
        let manifest = PluginManifest {
            name: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            server: Some(ManifestServerConfig {
                entry: "entry.wasm".to_string(),
                permissions: vec![],
                routes: vec![],
                hooks: vec![],
                queries: vec![ServerQuery {
                    topic: "visibility".to_string(),
                    function: "decide_visibility".to_string(),
                }],
            }),
            worker: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::new(),
        };
        let mut entry = PluginEntry::new(
            plugin_id.to_string(),
            PathBuf::from("/tmp/does-not-need-to-exist"),
            manifest,
        )
        .expect("manifest with no routes builds a valid entry");
        entry.status = PluginStatus::Loaded;
        entry
    }

    /// A `Loaded` plugin entry that registers a server, but no queries at
    /// all (e.g. it only uses `[[server.hooks]]`).
    fn no_query_plugin_entry(plugin_id: &str) -> PluginEntry {
        let manifest = PluginManifest {
            name: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            description: None,
            server: Some(ManifestServerConfig {
                entry: "entry.wasm".to_string(),
                permissions: vec![],
                routes: vec![],
                hooks: vec![],
                queries: vec![],
            }),
            worker: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::new(),
        };
        let mut entry = PluginEntry::new(
            plugin_id.to_string(),
            PathBuf::from("/tmp/does-not-need-to-exist"),
            manifest,
        )
        .expect("manifest with no routes builds a valid entry");
        entry.status = PluginStatus::Loaded;
        entry
    }

    /// Mirrors `dispatcher::steal::tests`'s AppState literal (the sole
    /// existing precedent for this in the crate) so field drift between the
    /// two is easy to spot in review.
    async fn test_app_state(plugins: Arc<dyn PluginManager>) -> AppState {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let blob_dir = tempfile::tempdir().expect("create blob tempdir");
        let blob_store = Arc::new(
            FilesystemBlobStore::new(blob_dir.path().join("blobs"), 1024 * 1024)
                .await
                .expect("create blob store"),
        );
        let (metrics, prometheus_registry) =
            common::observability::init_metrics("broccoli-visibility-plugin-query-test");

        let operation_batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches: EvaluateBatches = Arc::new(dashmap::DashMap::new());
        let contest_type_registry: ContestTypeRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let evaluator_registry: EvaluatorRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let checker_stage_registry: CheckerStageRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let language_resolver_registry: LanguageResolverRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));

        AppState {
            plugins,
            db,
            config: AppConfig {
                server: ServerConfig {
                    host: "127.0.0.1".to_string(),
                    port: 0,
                    cors: CorsConfig {
                        allow_origins: vec![],
                        max_age: 3600,
                    },
                    public_base_url: None,
                    frontend_dist: PathBuf::from("/srv/dist"),
                    trusted_proxies: vec![],
                    rate_limit_auth: false,
                    id: String::new(),
                    expects_multi_replica: false,
                    dispatcher_lease_steal_enabled: true,
                    dispatcher_semaphore_enabled: true,
                    dispatcher_concurrency: 1,
                    dispatcher_admission_queue_max: 0,
                    max_queued_submissions: 0,
                    lease_ttl_secs: 1,
                    lease_refresh_interval_secs: 10,
                    steal_scan_interval_secs: 15,
                    steal_batch_size: 8,
                    sweep_interval_secs: 300,
                    max_dispatch_retries: 5,
                    max_stuck_retries: 5,
                    max_system_error_retries: 50,
                    sweeper_dry_run: true,
                    operation_reaper_enabled: false,
                    operation_reaper_interval_secs: 30,
                    operation_reaper_grace_secs: 30,
                    operation_reaper_max_requeues_per_tick: 1000,
                    operation_reaper_dry_run: false,
                    cancel_primitive_enabled: false,
                    max_blocking_threads: None,
                    batch_evaluator_fanout_concurrency: 64,
                    operation_batch_publish_concurrency: 32,
                    healthz_listen: None,
                    healthz_worker_threads: 2,
                    claim_fiber_enabled: false,
                    claim_poll_interval_ms: 1000,
                    claim_batch_size: 32,
                },
                database: DatabaseConfig {
                    url: "mock://visibility-plugin-query-test".to_string(),
                    max_connections: 1,
                    plugin_max_connections: 1,
                    plugin_privileged_max_connections: 1,
                    plugin_url: None,
                },
                auth: AuthConfig {
                    jwt_secret: "test-secret".to_string(),
                    secure_cookies: false,
                    login_failure_limit: 0,
                    login_failure_window_secs: 60,
                },
                plugin: PluginConfig::default(),
                submission: SubmissionConfig::default(),
                storage: BlobStoreConfig::default(),
                mq: MqAppConfig {
                    enabled: false,
                    ..Default::default()
                },
                observability: common::config::ObservabilityConfig::default(),
                batch_max_age_secs: 600,
                bootstrap: BootstrapConfig::default(),
            },
            mq: None,
            redis_client: None,
            blob_store,
            registries: RegistryState {
                contest_type_registry,
                evaluator_registry,
                checker_stage_registry,
                language_resolver_registry,
                operation_batches,
                operation_waiters,
                evaluate_batches,
                hook_registry: crate::hooks::new_shared_registry(),
            },
            device_codes: Arc::new(dashmap::DashMap::new()),
            metrics,
            prometheus_registry,
            dispatcher_permits: DispatcherSemaphore::new(true, 1, 0),
            login_throttle: Arc::new(crate::utils::login_throttle::LoginThrottle::new(
                0,
                std::time::Duration::from_secs(60),
            )),
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn empty_resource_batch_returns_empty_and_calls_no_plugin() {
        let _guard = crate::metrics_test_lock();

        let entry = visibility_plugin_entry("vis-plugin");
        let mut registry_map = HashMap::new();
        registry_map.insert(entry.id.clone(), entry);

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry: Arc::new(RwLock::new(registry_map)),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();

        // A registered visibility querier exists, so if query_plugins reached
        // for the plugin host at all here, PanicPluginManager would panic and
        // fail this test - the assertion below only runs if it did not.
        let result = query_plugins(&state, &subject, Action::Read, None, &[]).await;

        assert_eq!(result, Vec::<Decision>::new());
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn no_registered_queriers_allows_without_calling_any_plugin() {
        let _guard = crate::metrics_test_lock();

        let entry = no_query_plugin_entry("no-queries");
        let mut registry_map = HashMap::new();
        registry_map.insert(entry.id.clone(), entry);

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry: Arc::new(RwLock::new(registry_map)),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1), Resource::Submission(2)];

        let result = query_plugins(&state, &subject, Action::Read, Some(1), &resources).await;

        assert_eq!(result, vec![Decision::Allow, Decision::Allow]);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn poisoned_registry_lock_denies_whole_batch() {
        let _guard = crate::metrics_test_lock();

        let registry: PluginRegistry = Arc::new(RwLock::new(HashMap::new()));
        {
            let registry = registry.clone();
            // Deliberately poison the lock, mirroring how a panic elsewhere
            // in the process (e.g. mid plugin-load) would leave it.
            let _ = std::thread::spawn(move || {
                let _write_guard = registry.write().unwrap();
                panic!("deliberately poisoning the registry lock for this test");
            })
            .join();
        }
        assert!(registry.is_poisoned());

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager {
            registry,
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
        });

        let state = test_app_state(plugins).await;
        let subject = Subject::anonymous();
        let resources = vec![Resource::Contest(1), Resource::Submission(2)];

        let result = query_plugins(&state, &subject, Action::Read, Some(1), &resources).await;

        assert_eq!(result, vec![Decision::Deny, Decision::Deny]);
    }
}
