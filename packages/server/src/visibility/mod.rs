//! Per-viewer reachability kernel. Every contest-scoped read and write
//! resolves through here. See
//! `docs/superpowers/specs/2026-09-15-visibility-kernel-design.md`.

use std::collections::{HashMap, HashSet};

mod decision;
mod host_rules;
mod mask;
mod plugin_query;
mod subject;

pub use decision::{Decision, FieldMask};
pub use mask::apply_mask;
pub(crate) use host_rules::host_decide;
pub(crate) use plugin_query::query_plugins;
pub use subject::{Action, Resource, Subject};

use crate::error::AppError;
use crate::state::AppState;

/// Best-effort single contest scope for a batch of resources, sent to
/// plugins as `QueryContext.contest_id`.
///
/// `Resource` does not uniformly carry a contest id (`Submission` and
/// `Clarification` do not - resolving those would need a DB round trip,
/// which this function deliberately does not do), so this only looks at the
/// variants that do carry one directly: `Contest`, and `Problem`/`Sample`
/// with an explicit `contest_id`. Returns `None` when the batch references
/// zero or more than one distinct contest this way - guessing one contest
/// out of several would be a wrong signal to hand a plugin, so "no single
/// scope" is reported as `None` rather than picked arbitrarily.
fn contest_scope(resources: &[Resource]) -> Option<i32> {
    let mut ids: HashSet<i32> = HashSet::new();
    for r in resources {
        match r {
            Resource::Contest(id) => {
                ids.insert(*id);
            }
            Resource::Problem { contest_id: Some(cid), .. }
            | Resource::Sample { contest_id: Some(cid), .. } => {
                ids.insert(*cid);
            }
            _ => {}
        }
    }
    if ids.len() == 1 { ids.into_iter().next() } else { None }
}

/// One kernel per HTTP request. Owns a memo of every `(Action, Resource)`
/// decision already computed against `subject` this request, so a
/// problem-list render that asks about the same resource twice - or a
/// `decide` call that repeats an earlier `decide_batch` call's resource -
/// costs at most one host-rule evaluation and one plugin crossing per
/// distinct resource, not one per call.
///
/// The memo key is `(Action, Resource)` only, not `(Subject, Action,
/// Resource)`: a kernel is constructed once per request and is expected to
/// be driven by the same `Subject` for its whole lifetime (the request's
/// authenticated viewer). This matches the pinned struct shape from the
/// task brief; see the task report for the implication if that assumption
/// is ever violated.
///
/// The memo is `tokio::sync::Mutex`-guarded, not `RefCell`: the kernel is
/// held across `.await` points (both `host_decide` and `query_plugins` are
/// async) and must stay `Send` to be usable from an axum handler.
///
/// Deliberately **not** cached beyond the kernel's own lifetime - see the
/// design doc's "Caching" section. Construct a fresh kernel per request;
/// never store one in `AppState` or anywhere longer-lived than a request.
pub struct VisibilityKernel<'a> {
    state: &'a AppState,
    memo: tokio::sync::Mutex<HashMap<(Action, Resource), Decision>>,
}

impl<'a> VisibilityKernel<'a> {
    /// One kernel per request. The memo lives exactly as long as this value.
    pub fn new(state: &'a AppState) -> Self {
        Self { state, memo: tokio::sync::Mutex::new(HashMap::new()) }
    }

    /// Single-resource convenience wrapper over [`Self::decide_batch`].
    pub async fn decide(
        &self,
        subject: &Subject,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, AppError> {
        Ok(self.decide_batch(subject, action, &[resource]).await?.remove(0))
    }

    /// Batched, memoized, per-viewer reachability decision.
    ///
    /// 1. Dedupe `resources`, preserving first-seen order.
    /// 2. Drop any entry the memo already has an answer for.
    /// 3. `host_decide` the remaining misses in one batch (`admin_override`
    ///    short-circuits *inside* `host_decide` to all-`Allow`, with zero
    ///    queries - that part is Task 4's, not this function's).
    /// 4. Unless `subject.is_admin_override()` - in which case plugins are
    ///    never consulted at all, full stop - send only the misses the host
    ///    did NOT already `Deny` to `query_plugins`, in their original
    ///    relative order. A host `Deny` is final (`meet(Deny, _) == Deny`),
    ///    so a resource the host denied never crosses into plugin code.
    /// 5. `meet` each host decision with its corresponding plugin decision,
    ///    written back at that resource's own index in the miss list - never
    ///    appended positionally, since the plugin's answer slice is shorter
    ///    than the miss list whenever any resource was host-denied.
    /// 6. Store every miss's final decision in the memo.
    /// 7. Re-expand: look every entry of the caller's ORIGINAL `resources`
    ///    slice up in the memo, in order, including duplicates.
    pub async fn decide_batch(
        &self,
        subject: &Subject,
        action: Action,
        resources: &[Resource],
    ) -> Result<Vec<Decision>, AppError> {
        if resources.is_empty() {
            return Ok(Vec::new());
        }

        // Step 1: dedupe, preserving first-seen order.
        let mut unique: Vec<Resource> = Vec::new();
        let mut seen: HashSet<Resource> = HashSet::new();
        for r in resources {
            if seen.insert(r.clone()) {
                unique.push(r.clone());
            }
        }

        // Step 2: consult the memo. Locked only long enough to read it - the
        // lock is dropped before the `host_decide`/`query_plugins` awaits
        // below, so a concurrent `decide`/`decide_batch` call on this same
        // kernel is never blocked behind a slow DB or plugin round trip.
        let mut misses: Vec<Resource> = Vec::new();
        {
            let memo = self.memo.lock().await;
            for r in &unique {
                if !memo.contains_key(&(action, r.clone())) {
                    misses.push(r.clone());
                }
            }
        }

        if !misses.is_empty() {
            let host_decisions = host_decide(&self.state.db, subject, action, &misses).await?;
            debug_assert_eq!(
                host_decisions.len(),
                misses.len(),
                "host_decide must answer one Decision per resource, positionally"
            );

            let final_decisions = if subject.is_admin_override() {
                // Decision #1: admin_override short-circuits BEFORE plugins.
                // host_decide already answered all-Allow above with zero
                // queries; that answer is final and query_plugins is never
                // called, for any resource in this batch.
                host_decisions
            } else {
                // Decision #2: never pay a WASM crossing to confirm a host
                // Deny. Only resources the host did NOT deny go to
                // query_plugins, and `plugin_target_indices[i]` records
                // exactly which index in `misses`/`host_decisions` the i-th
                // plugin answer belongs back at - getting this misaligned
                // is the most dangerous bug available here, since it would
                // hand one resource's decision to another.
                let mut plugin_targets: Vec<Resource> = Vec::new();
                let mut plugin_target_indices: Vec<usize> = Vec::new();
                for (i, d) in host_decisions.iter().enumerate() {
                    if !d.is_denied() {
                        plugin_targets.push(misses[i].clone());
                        plugin_target_indices.push(i);
                    }
                }

                let mut final_decisions = host_decisions;
                if !plugin_targets.is_empty() {
                    let contest_id = contest_scope(&plugin_targets);
                    let plugin_decisions =
                        query_plugins(self.state, subject, action, contest_id, &plugin_targets)
                            .await;
                    debug_assert_eq!(
                        plugin_decisions.len(),
                        plugin_targets.len(),
                        "query_plugins must answer one Decision per resource it was sent, positionally"
                    );

                    for (target_pos, &miss_index) in plugin_target_indices.iter().enumerate() {
                        final_decisions[miss_index] = final_decisions[miss_index]
                            .clone()
                            .meet(plugin_decisions[target_pos].clone());
                    }
                }
                final_decisions
            };

            // Step 6: store. Re-locks (rather than holding the Step 2 guard
            // across the awaits above) for the same reason noted there.
            let mut memo = self.memo.lock().await;
            for (r, d) in misses.into_iter().zip(final_decisions) {
                memo.insert((action, r), d);
            }
        }

        // Step 7: re-expand to the caller's original positional order,
        // including duplicates. Every entry of `resources` is guaranteed to
        // be in the memo by this point - it was either already there, or was
        // just inserted above.
        let memo = self.memo.lock().await;
        Ok(resources
            .iter()
            .map(|r| {
                memo.get(&(action, r.clone()))
                    .cloned()
                    .expect("every resource was decided and memoized above before re-expansion")
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex as StdMutex, RwLock};

    use broccoli_server_sdk::types::{VisibilityQueryInput, VisibilityQueryOutput, WireDecision};
    use common::storage::filesystem::FilesystemBlobStore;
    use plugin_core::config::PluginConfig;
    use plugin_core::error::PluginError;
    use plugin_core::host::HostFunctionRegistry;
    use plugin_core::i18n::I18nRegistry;
    use plugin_core::manifest::{PluginManifest, ServerConfig as ManifestServerConfig, ServerQuery};
    use plugin_core::registry::{PluginEntry, PluginRegistry, PluginStatus};
    use plugin_core::traits::{PluginInvoker, PluginManager};
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

    use super::*;
    use super::plugin_query::PanicPluginManager;
    use crate::config::{
        AppConfig, AuthConfig, BlobStoreConfig, BootstrapConfig, CorsConfig, DatabaseConfig,
        MqAppConfig, ServerConfig, SubmissionConfig,
    };
    use crate::dispatcher::permits::DispatcherSemaphore;
    use crate::entity::contest_user;
    use crate::extractors::auth::AuthUser;
    use crate::registry::{
        CheckerStageRegistry, ContestTypeRegistry, EvaluateBatches, EvaluatorRegistry,
        LanguageResolverRegistry, OperationBatches, OperationWaiters,
    };
    use crate::state::RegistryState;

    // -----------------------------------------------------------------
    // Fixtures shared by every test below.
    // -----------------------------------------------------------------

    fn subject(user_id: i32) -> Subject {
        Subject::from_auth_user(&AuthUser {
            user_id,
            username: "viewer".into(),
            roles: vec![],
            permissions: vec![],
        })
    }

    /// `hours` is an offset from now: negative = past, positive = future.
    /// Mirrors `host_rules::tests::contest_row` (duplicated locally - that
    /// helper is private to `host_rules`'s own test module).
    fn contest_row(
        id: i32,
        is_public: bool,
        activate_hours: Option<i64>,
        deactivate_hours: Option<i64>,
        submissions_visible: bool,
    ) -> crate::entity::contest::Model {
        let now = chrono::Utc::now();
        crate::entity::contest::Model {
            id,
            title: "Contest".into(),
            description: "desc".into(),
            activate_time: activate_hours.map(|h| now + chrono::Duration::hours(h)),
            deactivate_time: deactivate_hours.map(|h| now + chrono::Duration::hours(h)),
            start_time: now - chrono::Duration::hours(2),
            end_time: now + chrono::Duration::hours(2),
            is_public,
            submissions_visible,
            show_compile_output: true,
            show_participants_list: true,
            contest_type: None,
            created_at: now,
            updated_at: now,
            deleted_at: None,
        }
    }

    /// A `Loaded` plugin entry that registers one `visibility` query.
    /// Mirrors `plugin_query::tests::visibility_plugin_entry` (private to
    /// that module).
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

    fn registry_with_visibility_plugin(plugin_id: &str) -> PluginRegistry {
        let entry = visibility_plugin_entry(plugin_id);
        let mut map = HashMap::new();
        map.insert(entry.id.clone(), entry);
        Arc::new(RwLock::new(map))
    }

    /// Answers each call by looking the resource up in `answers` (default
    /// `Allow` for anything not listed), and records how many times it was
    /// called and how many resources each call carried - the two facts the
    /// memoization/dedup tests below need to assert on.
    struct RecordingPluginManager {
        registry: PluginRegistry,
        config: PluginConfig,
        host_functions: HostFunctionRegistry,
        i18n: I18nRegistry,
        calls: Arc<AtomicUsize>,
        call_resource_counts: Arc<StdMutex<Vec<usize>>>,
        answers: HashMap<(String, i32), WireDecision>,
    }

    #[async_trait::async_trait]
    impl PluginInvoker for RecordingPluginManager {
        fn get_registry(&self) -> &PluginRegistry {
            &self.registry
        }

        fn get_config(&self) -> &PluginConfig {
            &self.config
        }

        async fn call_raw(
            &self,
            _plugin_id: &str,
            _func_name: &str,
            input: Vec<u8>,
        ) -> Result<Vec<u8>, PluginError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let parsed: VisibilityQueryInput = serde_json::from_slice(&input)
                .expect("test double: input must deserialize as VisibilityQueryInput");
            self.call_resource_counts.lock().unwrap().push(parsed.resources.len());

            let decisions = parsed
                .resources
                .iter()
                .map(|r| {
                    self.answers
                        .get(&(r.kind.clone(), r.id))
                        .cloned()
                        .unwrap_or(WireDecision::Allow {})
                })
                .collect();
            let output = VisibilityQueryOutput { decisions };
            Ok(serde_json::to_vec(&output).expect("serialize VisibilityQueryOutput"))
        }
    }

    impl PluginManager for RecordingPluginManager {
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

    /// Mirrors `plugin_query::tests::test_app_state` (private to that
    /// module), parameterized on `db` since these tests - unlike
    /// `plugin_query`'s - actually drive `host_decide` through it.
    async fn test_app_state(plugins: Arc<dyn PluginManager>, db: DatabaseConnection) -> AppState {
        let blob_dir = tempfile::tempdir().expect("create blob tempdir");
        let blob_store = Arc::new(
            FilesystemBlobStore::new(blob_dir.path().join("blobs"), 1024 * 1024)
                .await
                .expect("create blob store"),
        );
        let (metrics, prometheus_registry) =
            common::observability::init_metrics("broccoli-visibility-kernel-test");

        let operation_batches: OperationBatches = Arc::new(dashmap::DashMap::new());
        let operation_waiters: OperationWaiters = Arc::new(dashmap::DashMap::new());
        let evaluate_batches: EvaluateBatches = Arc::new(dashmap::DashMap::new());
        let contest_type_registry: ContestTypeRegistry =
            Arc::new(tokio::sync::RwLock::new(HashMap::new()));
        let evaluator_registry: EvaluatorRegistry = Arc::new(tokio::sync::RwLock::new(HashMap::new()));
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
                    cors: CorsConfig { allow_origins: vec![], max_age: 3600 },
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
                    url: "mock://visibility-kernel-test".to_string(),
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
                mq: MqAppConfig { enabled: false, ..Default::default() },
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

    // -----------------------------------------------------------------
    // Brief's tests
    // -----------------------------------------------------------------

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn plugin_allow_cannot_override_host_deny() {
        let _guard = crate::metrics_test_lock();

        // Out-of-window contest -> host Deny (rule 2), regardless of
        // `is_public`.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-3), Some(-1), true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        // A registered visibility querier exists and WOULD answer Allow if
        // called - PanicPluginManager panics on any call, so this test only
        // passes if decide_batch never reaches for it for this resource.
        let plugins: Arc<dyn PluginManager> =
            Arc::new(PanicPluginManager::new(registry_with_visibility_plugin("vis-plugin")));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let decisions = kernel
            .decide_batch(&subject(1), Action::Read, &[Resource::Contest(7)])
            .await
            .unwrap();

        assert_eq!(decisions, vec![Decision::Deny]);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn memoizes_within_one_batch() {
        let _guard = crate::metrics_test_lock();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        let calls = Arc::new(AtomicUsize::new(0));
        let call_resource_counts = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: calls.clone(),
            call_resource_counts: call_resource_counts.clone(),
            answers: HashMap::new(),
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let decisions = kernel
            .decide_batch(
                &subject(1),
                Action::Read,
                &[Resource::Contest(7), Resource::Contest(7)],
            )
            .await
            .unwrap();

        assert_eq!(decisions, vec![Decision::Allow, Decision::Allow]);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the plugin must be called exactly once");
        assert_eq!(
            *call_resource_counts.lock().unwrap(),
            vec![1],
            "the one call must have been for the deduped resource, not twice"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn result_is_positional_and_same_length_as_input() {
        let _guard = crate::metrics_test_lock();

        // Contest 1: private, non-member -> host Deny. Contest 2 and 3:
        // public, in-window -> host Allow; the plugin then Redacts 3 only.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                contest_row(1, false, Some(-1), None, true),
                contest_row(2, true, Some(-1), None, true),
                contest_row(3, true, Some(-1), None, true),
            ]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        let mut answers = HashMap::new();
        answers.insert(("contest".to_string(), 3), WireDecision::Redact { fields: vec!["x".to_string()] });
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            call_resource_counts: Arc::new(StdMutex::new(Vec::new())),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let resources = vec![
            Resource::Contest(1),
            Resource::Contest(2),
            Resource::Contest(1),
            Resource::Contest(3),
        ];
        let decisions = kernel.decide_batch(&subject(1), Action::Read, &resources).await.unwrap();

        assert_eq!(decisions.len(), resources.len());
        assert_eq!(
            decisions,
            vec![
                Decision::Deny,
                Decision::Allow,
                Decision::Deny,
                Decision::Redact(FieldMask::new(["x".to_string()])),
            ]
        );
    }

    // -----------------------------------------------------------------
    // Required beyond the brief
    // -----------------------------------------------------------------

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn mixed_batch_denied_and_allowed_resources_resolve_independently() {
        let _guard = crate::metrics_test_lock();

        // Contest 1: private, non-member -> host Deny. Contest 2 and 3:
        // public, in-window -> host Allow; the plugin then denies 2 and
        // allows 3, so all three final outcomes differ.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                contest_row(1, false, Some(-1), None, true),
                contest_row(2, true, Some(-1), None, true),
                contest_row(3, true, Some(-1), None, true),
            ]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        let mut answers = HashMap::new();
        answers.insert(("contest".to_string(), 2), WireDecision::Deny {});
        let calls = Arc::new(AtomicUsize::new(0));
        let call_resource_counts = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: calls.clone(),
            call_resource_counts: call_resource_counts.clone(),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let resources = vec![Resource::Contest(1), Resource::Contest(2), Resource::Contest(3)];
        let decisions = kernel.decide_batch(&subject(1), Action::Read, &resources).await.unwrap();

        // Every position asserted independently.
        assert_eq!(decisions[0], Decision::Deny, "contest 1 is host-denied");
        assert_eq!(decisions[1], Decision::Deny, "contest 2 is host-allowed but plugin-denied");
        assert_eq!(decisions[2], Decision::Allow, "contest 3 is allowed by both host and plugin");

        // The host-denied resource never crossed into the plugin call.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(*call_resource_counts.lock().unwrap(), vec![2]);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn duplicate_resource_positions_get_identical_decision_and_single_plugin_call() {
        let _guard = crate::metrics_test_lock();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(5, true, Some(-1), None, true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        let mut answers = HashMap::new();
        answers.insert(
            ("contest".to_string(), 5),
            WireDecision::Redact { fields: vec!["score".to_string()] },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: calls.clone(),
            call_resource_counts: Arc::new(StdMutex::new(Vec::new())),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let decisions = kernel
            .decide_batch(
                &subject(1),
                Action::Read,
                &[Resource::Contest(5), Resource::Contest(5)],
            )
            .await
            .unwrap();

        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0], decisions[1], "both positions must carry the identical decision");
        assert_eq!(decisions[0], Decision::Redact(FieldMask::new(["score".to_string()])));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "the plugin must be called at most once");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn admin_override_allows_everything_without_reaching_plugins() {
        let _guard = crate::metrics_test_lock();

        // host_decide's admin_override path touches the database zero
        // times, so an empty MockDatabase is enough.
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        // A registered visibility querier exists - PanicPluginManager
        // proves decide_batch never reaches it for an admin_override
        // subject, for any resource in the batch.
        let plugins: Arc<dyn PluginManager> =
            Arc::new(PanicPluginManager::new(registry_with_visibility_plugin("vis-plugin")));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let resources = vec![
            Resource::Contest(1),
            Resource::Submission(2),
            Resource::Problem { contest_id: Some(1), problem_id: 3 },
        ];
        let decisions = kernel
            .decide_batch(&Subject::admin_override(), Action::Read, &resources)
            .await
            .unwrap();

        assert_eq!(decisions, vec![Decision::Allow, Decision::Allow, Decision::Allow]);
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn empty_batch_returns_empty_without_touching_state() {
        let _guard = crate::metrics_test_lock();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let plugins: Arc<dyn PluginManager> =
            Arc::new(PanicPluginManager::new(registry_with_visibility_plugin("vis-plugin")));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state);

        let decisions = kernel.decide_batch(&subject(1), Action::Read, &[]).await.unwrap();

        assert_eq!(decisions, Vec::<Decision>::new());
    }
}
