//! Per-viewer reachability kernel. Every contest-scoped read and write
//! resolves through here. See
//! `docs/superpowers/specs/2026-09-15-visibility-kernel-design.md`.

use std::collections::{HashMap, HashSet};

mod decision;
mod host_rules;
mod mask;
mod plugin_query;
mod subject;
mod visible;

pub use decision::{Decision, FieldMask};
pub(crate) use host_rules::host_decide;
pub use mask::apply_mask;
pub(crate) use plugin_query::query_plugins;
pub use subject::{Action, Resource, Subject};
pub use visible::Visible;

use crate::error::AppError;
use crate::state::AppState;

/// Best-effort single contest scope for a batch of resources, used ONLY as
/// `QueryContext.contest_id` - a HINT for plugins, never authoritative for
/// any individual resource. See [`resource_contest_id`] for the
/// authoritative, per-resource value that actually lands in each
/// `QueryResource.contest_id` on the wire; conflating the two (using this
/// function's result as a stand-in for per-resource scope) was a real bug -
/// see the task report's CRITICAL fix note.
///
/// `Resource` does not uniformly carry a contest id (`Submission` and
/// `Clarification` do not - resolving those needs a DB round trip, which
/// this function deliberately does not do, unlike [`resource_contest_id`]),
/// so this only looks at the variants that do carry one directly:
/// `Contest`, and `Problem`/`Sample` with an explicit `contest_id`. Returns
/// `None` when the batch references zero or more than one distinct contest
/// this way - guessing one contest out of several would be a wrong signal
/// to hand a plugin, so "no single scope" is reported as `None` rather than
/// picked arbitrarily.
fn contest_scope(resources: &[Resource]) -> Option<i32> {
    let mut ids: HashSet<i32> = HashSet::new();
    for r in resources {
        match r {
            Resource::Contest(id) => {
                ids.insert(*id);
            }
            Resource::Problem {
                contest_id: Some(cid),
                ..
            }
            | Resource::Sample {
                contest_id: Some(cid),
                ..
            } => {
                ids.insert(*cid);
            }
            _ => {}
        }
    }
    if ids.len() == 1 {
        ids.into_iter().next()
    } else {
        None
    }
}

/// Resolve `resource`'s AUTHORITATIVE contest scope, stamped onto its own
/// `QueryResource.contest_id` on the wire - see that field's doc comment
/// (`packages/broccoli-types/src/types/visibility.rs`) for why this is
/// distinct from, and takes precedence over, the batch-level
/// `QueryContext.contest_id` hint computed by [`contest_scope`]. A batch
/// spanning several contests at once (e.g. a submission list mixing
/// submissions from more than one contest) is losslessly expressible this
/// way, where a single batch-level value could not represent it.
///
/// `submission_contest_ids` is `host_decide`'s out-param
/// (`submission_id -> submission.contest_id`), populated for every
/// `Resource::Submission` that `host_decide` resolved while producing the
/// `Decision`s for this same `decide_batch` call - by the time this runs,
/// every `Resource::Submission` passed in is guaranteed to be a key in it.
///
/// `clarification_contest_ids` is the analogous out-param for `Resource::
/// Clarification` (`clarification_id -> clarification.contest_id`), added
/// in Task 12 alongside `host_rules::decide_clarification`. Unlike a
/// submission, a clarification's `contest_id` column is never null - it
/// belongs to exactly one contest, always - so returning `None` for a
/// `Resource::Clarification` that WAS resolved by `host_decide` would be
/// the exact Task 7 `Submission` bug this function's own history warns
/// about: a real, single-valued `contest_id` exists and is knowable, so
/// reporting `None` would be silently dishonest to a plugin, not merely
/// imprecise. It differs from `Resource::Attachment`'s principled `None`
/// below in exactly this way - a clarification has ONE contest, a problem
/// backing an attachment can have zero, one, or many. A miss in this map
/// (the id absent as a key) can only mean this resource was never a host
/// `Deny` target that reached here in the first place, mirroring
/// `submission_contest_ids`'s own miss handling.
///
/// `Resource::Attachment` DOES reach this function now: `host_decide` can
/// `Allow` it (see `host_rules::decide_standalone_problem_access`, Task 10),
/// so its `None` here is a deliberate, principled answer, not the same kind
/// of "unresolved" gap `Clarification` used to document before Task 12, and
/// NOT a repeat of the Task 7 `Submission` bug this function's history
/// warns about. The `Submission` bug was losing a real, single-valued
/// `contest_id` that existed but was not inline in the `Resource` (it had
/// to come from `submission_contest_ids`) - dropping it to `None` was
/// silently wrong. `Resource::Attachment` is different in kind, not just in
/// whether a lookup was wired up: it carries a `problem_id`, not a
/// `contest_id`, and the rule that admits it
/// (`decide_standalone_problem_access`) is the same contest-agnostic rule
/// used for standalone `Resource::Problem`/`Resource::Sample`
/// (`contest_id: None`, handled by the arm below) - a problem can be
/// attached to zero, one, or many contests via `contest_problem`, so even a
/// DB lookup would produce a SET, not a single authoritative value. There
/// is no real single answer to lose here, so `None` is not fail-open; it is
/// the same "not scoped to one contest" answer this function already gives
/// for a standalone `Resource::Problem`/`Resource::Sample`.
fn resource_contest_id(
    resource: &Resource,
    submission_contest_ids: &HashMap<i32, Option<i32>>,
    clarification_contest_ids: &HashMap<i32, i32>,
) -> Option<i32> {
    match resource {
        Resource::Contest(id) => Some(*id),
        Resource::Problem { contest_id, .. } | Resource::Sample { contest_id, .. } => *contest_id,
        Resource::Submission(id) => submission_contest_ids.get(id).copied().flatten(),
        Resource::Clarification(id) => clarification_contest_ids.get(id).copied(),
        // See the doc comment above: `Attachment` has no single contest
        // scope even in principle (problem -> contest is many-to-many).
        Resource::Attachment { .. } => None,
    }
}

/// One kernel per HTTP request, for exactly one `Subject`.
///
/// Owns a memo of every `(Action, Resource)` decision already computed this
/// request, so a problem-list render that asks about the same resource
/// twice - or a `decide` call that repeats an earlier `decide_batch` call's
/// resource - costs at most one host-rule evaluation and, barring a
/// concurrent race (see below), one plugin crossing per distinct resource,
/// not one per call.
///
/// The memo key is `(Action, Resource)`, with no `Subject` component. That
/// is only sound because `Subject` is fixed for this kernel's entire
/// lifetime: it is consumed once by [`Self::new`], not accepted per call by
/// [`Self::decide`] or [`Self::decide_batch`] - so there is no way to drive
/// one kernel with two different subjects and have the second silently get
/// back the first one's cached decision. That would otherwise be a
/// cross-user authorization leak with no error and no failing test; making
/// it a compile error instead (there is no `subject` parameter to pass a
/// second, different value to) is the same kind of structural guarantee
/// `Decision::meet` gives against widening and `Visible<T>` gives against
/// bypassing the kernel altogether. Construct a fresh kernel per request,
/// alongside the request's authenticated viewer; never store one in
/// `AppState` or anywhere longer-lived than a request, and never share one
/// across subjects.
///
/// The memo is `tokio::sync::Mutex`-guarded, not `RefCell`: the kernel is
/// held across `.await` points (both `host_decide` and `query_plugins` are
/// async) and must stay `Send` to be usable from an axum handler.
///
/// "At most one plugin crossing per distinct resource" holds for
/// sequential use of a kernel. Two concurrent calls on the SAME kernel
/// (e.g. two branches of a `tokio::join!` in one handler, both asking about
/// the same resource) can each miss the memo before either has written
/// back, and both then call `query_plugins` for that resource - this is
/// redundant (an extra WASM crossing) but not incorrect: both calls `meet`
/// the same host decision with the same plugin answer and store the
/// identical final `Decision`, just twice.
///
/// Deliberately **not** cached beyond the kernel's own lifetime - see the
/// design doc's "Caching" section. Construct a fresh kernel per request;
/// never store one in `AppState` or anywhere longer-lived than a request.
pub struct VisibilityKernel<'a> {
    state: &'a AppState,
    subject: Subject,
    memo: tokio::sync::Mutex<HashMap<(Action, Resource), Decision>>,
}

impl<'a> VisibilityKernel<'a> {
    /// One kernel per request, for exactly one subject. The memo lives
    /// exactly as long as this value.
    pub fn new(state: &'a AppState, subject: Subject) -> Self {
        Self {
            state,
            subject,
            memo: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Single-resource convenience wrapper over [`Self::decide_batch`].
    pub async fn decide(&self, action: Action, resource: Resource) -> Result<Decision, AppError> {
        Ok(self.decide_batch(action, &[resource]).await?.remove(0))
    }

    /// Batched, memoized, per-viewer reachability decision, for this
    /// kernel's fixed `Subject` (see the struct docs for why `Subject` is
    /// not a parameter here).
    ///
    /// 1. Dedupe `resources`, preserving first-seen order.
    /// 2. Drop any entry the memo already has an answer for.
    /// 3. `host_decide` the remaining misses in one batch (`admin_override`
    ///    short-circuits *inside* `host_decide` to all-`Allow`, with zero
    ///    queries - that part is Task 4's, not this function's). This also
    ///    resolves `submission_contest_ids`, `host_decide`'s out-param
    ///    mapping every `Resource::Submission` miss to its own
    ///    `contest_id` - needed by step 4 so each resource can be stamped
    ///    with its OWN authoritative contest scope (see
    ///    `resource_contest_id`) instead of a lossy batch-level guess.
    /// 4. Unless `self.subject.is_admin_override()` - in which case plugins
    ///    are never consulted at all, full stop - send only the misses the
    ///    host did NOT already `Deny` to `query_plugins`, in their original
    ///    relative order, each carrying its own `contest_id` via
    ///    `resource_contest_id`. A host `Deny` is final (`meet(Deny, _)
    ///    == Deny`), so a resource the host denied never crosses into
    ///    plugin code.
    /// 5. `meet` each host decision with its corresponding plugin decision,
    ///    written back at that resource's own index in the miss list - never
    ///    appended positionally, since the plugin's answer slice is shorter
    ///    than the miss list whenever any resource was host-denied.
    /// 6. Store every miss's final decision in the memo.
    /// 7. Re-expand: look every entry of the caller's ORIGINAL `resources`
    ///    slice up in the memo, in order, including duplicates.
    pub async fn decide_batch(
        &self,
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
        // kernel is never blocked behind a slow DB or plugin round trip
        // (see the struct docs for what that concurrency can and cannot
        // cause).
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
            let mut submission_contest_ids: HashMap<i32, Option<i32>> = HashMap::new();
            let mut clarification_contest_ids: HashMap<i32, i32> = HashMap::new();
            let host_decisions = host_decide(
                &self.state.db,
                &self.subject,
                action,
                &misses,
                &mut submission_contest_ids,
                &mut clarification_contest_ids,
            )
            .await?;
            debug_assert_eq!(
                host_decisions.len(),
                misses.len(),
                "host_decide must answer one Decision per resource, positionally"
            );

            let final_decisions = if self.subject.is_admin_override() {
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
                    // CRITICAL fix: stamp each resource's OWN authoritative
                    // contest_id, built in lockstep with `plugin_targets` so
                    // it stays positional with it - `contest_scope` below is
                    // only ever the batch-level hint, never a substitute for
                    // this.
                    let target_contest_ids: Vec<Option<i32>> = plugin_targets
                        .iter()
                        .map(|r| {
                            resource_contest_id(
                                r,
                                &submission_contest_ids,
                                &clarification_contest_ids,
                            )
                        })
                        .collect();
                    let contest_id = contest_scope(&plugin_targets);
                    let plugin_decisions = query_plugins(
                        self.state,
                        &self.subject,
                        action,
                        contest_id,
                        &plugin_targets,
                        &target_contest_ids,
                    )
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

    /// Decide, then wrap. The only public way to obtain a `Visible<T>`.
    /// Returns `Ok(None)` when the decision is `Deny`, so callers filter a
    /// list by dropping `None` and render a detail 404 on `None` - a denied
    /// resource is never rendered as a placeholder, because a placeholder
    /// confirms it exists.
    pub async fn fetch_visible<T: serde::Serialize>(
        &self,
        action: Action,
        resource: Resource,
        entity: T,
    ) -> Result<Option<Visible<T>>, AppError> {
        let decision = self.decide(action, resource).await?;
        Ok(match decision {
            Decision::Deny => None,
            d => Some(Visible::new(entity, d)),
        })
    }

    /// Batched form. Input and output are positionally aligned; denied
    /// entries come back as `None`.
    pub async fn fetch_visible_batch<T: serde::Serialize>(
        &self,
        action: Action,
        items: Vec<(Resource, T)>,
    ) -> Result<Vec<Option<Visible<T>>>, AppError> {
        let resources: Vec<Resource> = items.iter().map(|(r, _)| r.clone()).collect();
        let decisions = self.decide_batch(action, &resources).await?;
        Ok(items
            .into_iter()
            .zip(decisions)
            .map(|((_, entity), d)| match d {
                Decision::Deny => None,
                d => Some(Visible::new(entity, d)),
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
    use common::SubmissionStatus;
    use common::storage::filesystem::FilesystemBlobStore;
    use plugin_core::config::PluginConfig;
    use plugin_core::error::PluginError;
    use plugin_core::host::HostFunctionRegistry;
    use plugin_core::i18n::I18nRegistry;
    use plugin_core::manifest::{
        PluginManifest, ServerConfig as ManifestServerConfig, ServerQuery,
    };
    use plugin_core::registry::{PluginEntry, PluginRegistry, PluginStatus};
    use plugin_core::traits::{PluginInvoker, PluginManager};
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase};

    use super::plugin_query::PanicPluginManager;
    use super::*;
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

    /// Mirrors `host_rules::tests::submission_row` (private to that
    /// module's own test module).
    fn submission_row(
        id: i32,
        user_id: i32,
        contest_id: Option<i32>,
    ) -> crate::entity::submission::Model {
        let now = chrono::Utc::now();
        crate::entity::submission::Model {
            id,
            files: serde_json::json!({}),
            language: "cpp".into(),
            user_id,
            problem_id: 1,
            contest_id,
            contest_type: "ioi".into(),
            status: SubmissionStatus::Pending,
            verdict: None,
            compile_output: None,
            error_code: None,
            error_message: None,
            score: None,
            time_used: None,
            memory_used: None,
            judge_epoch: 0,
            target_worker_id: None,
            owner_server_id: None,
            lease_heartbeat_at: None,
            leased_at: None,
            retry_count: 0,
            created_at: now,
            judged_at: None,
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
    /// called and the full parsed input of each call - the latter lets
    /// tests assert on exactly what was sent to the plugin (which resources,
    /// with which `contest_id`, and the batch-level `QueryContext`).
    struct RecordingPluginManager {
        registry: PluginRegistry,
        config: PluginConfig,
        host_functions: HostFunctionRegistry,
        i18n: I18nRegistry,
        calls: Arc<AtomicUsize>,
        captured: Arc<StdMutex<Vec<VisibilityQueryInput>>>,
        answers: HashMap<(String, String), WireDecision>,
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

            let decisions = parsed
                .resources
                .iter()
                .map(|r| {
                    self.answers
                        .get(&(r.kind.clone(), r.id.clone()))
                        .cloned()
                        .unwrap_or(WireDecision::Allow {})
                })
                .collect();
            self.captured.lock().unwrap().push(parsed);

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
        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager::new(
            registry_with_visibility_plugin("vis-plugin"),
        ));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let decisions = kernel
            .decide_batch(Action::Read, &[Resource::Contest(7)])
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
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: calls.clone(),
            captured: captured.clone(),
            answers: HashMap::new(),
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let decisions = kernel
            .decide_batch(Action::Read, &[Resource::Contest(7), Resource::Contest(7)])
            .await
            .unwrap();

        assert_eq!(decisions, vec![Decision::Allow, Decision::Allow]);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the plugin must be called exactly once"
        );
        assert_eq!(
            captured.lock().unwrap()[0].resources.len(),
            1,
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
        answers.insert(
            ("contest".to_string(), "3".to_string()),
            WireDecision::Redact {
                fields: vec!["x".to_string()],
            },
        );
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            captured: Arc::new(StdMutex::new(Vec::new())),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let resources = vec![
            Resource::Contest(1),
            Resource::Contest(2),
            Resource::Contest(1),
            Resource::Contest(3),
        ];
        let decisions = kernel.decide_batch(Action::Read, &resources).await.unwrap();

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
        answers.insert(
            ("contest".to_string(), "2".to_string()),
            WireDecision::Deny {},
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: calls.clone(),
            captured: captured.clone(),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let resources = vec![
            Resource::Contest(1),
            Resource::Contest(2),
            Resource::Contest(3),
        ];
        let decisions = kernel.decide_batch(Action::Read, &resources).await.unwrap();

        // Every position asserted independently.
        assert_eq!(decisions[0], Decision::Deny, "contest 1 is host-denied");
        assert_eq!(
            decisions[1],
            Decision::Deny,
            "contest 2 is host-allowed but plugin-denied"
        );
        assert_eq!(
            decisions[2],
            Decision::Allow,
            "contest 3 is allowed by both host and plugin"
        );

        // The host-denied resource never crossed into the plugin call.
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(captured.lock().unwrap()[0].resources.len(), 2);
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
            ("contest".to_string(), "5".to_string()),
            WireDecision::Redact {
                fields: vec!["score".to_string()],
            },
        );
        let calls = Arc::new(AtomicUsize::new(0));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: calls.clone(),
            captured: Arc::new(StdMutex::new(Vec::new())),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let decisions = kernel
            .decide_batch(Action::Read, &[Resource::Contest(5), Resource::Contest(5)])
            .await
            .unwrap();

        assert_eq!(decisions.len(), 2);
        assert_eq!(
            decisions[0], decisions[1],
            "both positions must carry the identical decision"
        );
        assert_eq!(
            decisions[0],
            Decision::Redact(FieldMask::new(["score".to_string()]))
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the plugin must be called at most once"
        );
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
        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager::new(
            registry_with_visibility_plugin("vis-plugin"),
        ));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, Subject::admin_override());

        let resources = vec![
            Resource::Contest(1),
            Resource::Submission(2),
            Resource::Problem {
                contest_id: Some(1),
                problem_id: 3,
            },
        ];
        let decisions = kernel.decide_batch(Action::Read, &resources).await.unwrap();

        assert_eq!(
            decisions,
            vec![Decision::Allow, Decision::Allow, Decision::Allow]
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn empty_batch_returns_empty_without_touching_state() {
        let _guard = crate::metrics_test_lock();

        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager::new(
            registry_with_visibility_plugin("vis-plugin"),
        ));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let decisions = kernel.decide_batch(Action::Read, &[]).await.unwrap();

        assert_eq!(decisions, Vec::<Decision>::new());
    }

    // -----------------------------------------------------------------
    // CRITICAL fix: per-resource contest_id (review round 2)
    // -----------------------------------------------------------------

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn submission_resources_each_carry_their_own_contest_id() {
        let _guard = crate::metrics_test_lock();

        // Both submissions are owned by the viewer, so host_decide's rule 6
        // owner bypass answers Allow for both without ever fetching a
        // contest - Query 0 (submissions) is the only query this batch
        // needs (see `host_decide`'s module docs' "Batching" section).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                submission_row(10, 1, Some(7)),
                submission_row(11, 1, Some(7)),
            ]])
            .into_connection();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            captured: captured.clone(),
            answers: HashMap::new(),
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let decisions = kernel
            .decide_batch(
                Action::Read,
                &[Resource::Submission(10), Resource::Submission(11)],
            )
            .await
            .unwrap();
        assert_eq!(decisions, vec![Decision::Allow, Decision::Allow]);

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        let sent = &captured[0].resources;
        assert_eq!(sent.len(), 2);
        for r in sent {
            assert_eq!(r.kind, "submission");
            assert_eq!(
                r.contest_id,
                Some(7),
                "a pure-Submission batch must not lose its known contest_id (resource id {})",
                r.id
            );
        }
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn submission_batch_spanning_two_contests_stamps_each_resource_independently() {
        let _guard = crate::metrics_test_lock();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                submission_row(10, 1, Some(7)),
                submission_row(20, 1, Some(8)),
            ]])
            .into_connection();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            captured: captured.clone(),
            answers: HashMap::new(),
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        kernel
            .decide_batch(
                Action::Read,
                &[Resource::Submission(10), Resource::Submission(20)],
            )
            .await
            .unwrap();

        let captured = captured.lock().unwrap();
        let sent = &captured[0].resources;
        let contest_id_of = |id: i32| {
            sent.iter()
                .find(|r| r.id == id.to_string())
                .unwrap()
                .contest_id
        };
        assert_eq!(
            contest_id_of(10),
            Some(7),
            "submission 10 must carry its own contest, not submission 20's"
        );
        assert_eq!(
            contest_id_of(20),
            Some(8),
            "submission 20 must carry its own contest, not submission 10's"
        );

        // The batch-level hint cannot represent two contests at once - this
        // is exactly why per-resource `contest_id` is the authority, not
        // this value. Grouping by contest does not fix the loss; this
        // assertion documents that the ambiguous hint is expected to stay
        // `None` even though every individual resource above still carries
        // its own correct id.
        assert_eq!(
            captured[0].context.contest_id, None,
            "a two-contest batch has no single request-level scope to hint at"
        );
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn contest_less_submission_gets_no_contest_id_not_a_wrong_one() {
        let _guard = crate::metrics_test_lock();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![submission_row(30, 1, None)]])
            .into_connection();

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            captured: captured.clone(),
            answers: HashMap::new(),
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let decisions = kernel
            .decide_batch(Action::Read, &[Resource::Submission(30)])
            .await
            .unwrap();
        assert_eq!(decisions, vec![Decision::Allow]);

        let captured = captured.lock().unwrap();
        assert_eq!(
            captured[0].resources[0].contest_id, None,
            "a genuinely contest-less submission must stay None, not silently inherit \
             some other resource's contest"
        );
    }

    // -----------------------------------------------------------------
    // `fetch_visible` / `fetch_visible_batch`
    // -----------------------------------------------------------------

    #[derive(Debug, Clone, serde::Serialize)]
    struct ScoredEntity {
        id: i32,
        score: Option<i32>,
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn fetch_visible_batch_aligns_positions_across_allow_redact_and_deny() {
        let _guard = crate::metrics_test_lock();

        // Contest 1: private, non-member -> host Deny.
        // Contest 2: public, in-window -> host Allow; plugin redacts "score".
        // Contest 3: public, in-window -> host Allow; plugin allows too.
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                contest_row(1, false, Some(-1), None, true),
                contest_row(2, true, Some(-1), None, true),
                contest_row(3, true, Some(-1), None, true),
            ]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        let mut answers = HashMap::new();
        answers.insert(
            ("contest".to_string(), "2".to_string()),
            WireDecision::Redact {
                fields: vec!["score".to_string()],
            },
        );
        let plugins: Arc<dyn PluginManager> = Arc::new(RecordingPluginManager {
            registry: registry_with_visibility_plugin("vis-plugin"),
            config: PluginConfig::default(),
            host_functions: HostFunctionRegistry::new(),
            i18n: I18nRegistry::new(),
            calls: Arc::new(AtomicUsize::new(0)),
            captured: Arc::new(StdMutex::new(Vec::new())),
            answers,
        });
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let items = vec![
            (
                Resource::Contest(1),
                ScoredEntity {
                    id: 1,
                    score: Some(10),
                },
            ),
            (
                Resource::Contest(2),
                ScoredEntity {
                    id: 2,
                    score: Some(20),
                },
            ),
            (
                Resource::Contest(3),
                ScoredEntity {
                    id: 3,
                    score: Some(30),
                },
            ),
        ];
        let mut results = kernel
            .fetch_visible_batch(Action::Read, items)
            .await
            .unwrap();
        assert_eq!(results.len(), 3, "output must be the same length as input");

        // Index 0: host-denied -> None, never a wrapped placeholder - a
        // placeholder would confirm the resource exists.
        assert!(
            results[0].is_none(),
            "a denied resource must come back as None, not Some(Visible) around a placeholder"
        );

        // Index 1: plugin-redacted -> Some, and the mask actually blanks the
        // field once serialized, while leaving the other field untouched.
        let redacted = results[1]
            .take()
            .expect("contest 2 was redacted, not denied");
        assert_eq!(redacted.as_inner().id, 2);
        let json = redacted.into_masked_json().unwrap();
        assert!(
            json["score"].is_null(),
            "redacted field must be blanked in the final JSON"
        );
        assert_eq!(json["id"], 2, "non-masked fields must survive untouched");

        // Index 2: fully allowed -> Some, unmasked.
        let allowed = results[2].take().expect("contest 3 was allowed");
        let json = allowed.into_masked_json().unwrap();
        assert_eq!(json["score"], 30, "an allowed resource must not be masked");
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn fetch_visible_single_deny_returns_none_not_placeholder() {
        let _guard = crate::metrics_test_lock();

        // Out-of-window contest -> host Deny (rule 2).
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![contest_row(7, true, Some(-3), Some(-1), true)]])
            .append_query_results([Vec::<contest_user::Model>::new()])
            .into_connection();

        let plugins: Arc<dyn PluginManager> = Arc::new(PanicPluginManager::new(
            registry_with_visibility_plugin("vis-plugin"),
        ));
        let state = test_app_state(plugins, db).await;
        let kernel = VisibilityKernel::new(&state, subject(1));

        let result = kernel
            .fetch_visible(
                Action::Read,
                Resource::Contest(7),
                ScoredEntity {
                    id: 7,
                    score: Some(1),
                },
            )
            .await
            .unwrap();

        assert!(
            result.is_none(),
            "a denied resource must be None so callers 404 instead of rendering a placeholder"
        );
    }
}
