use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use broccoli_server_sdk::types::HookEvent;
use common::event::GenericEvent;
use common::hook::{GenericHook, HookAction};
use lru::LruCache;
use plugin_core::hook::{HookMode, HookScope, PLUGIN_RUNTIME_ERROR_CODE, PluginHook};
use plugin_core::traits::PluginManager;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use std::sync::LazyLock;
use tokio::sync::RwLock;

static DISPATCHED_EVENTS: LazyLock<Mutex<LruCache<String, ()>>> =
    LazyLock::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(10_000).unwrap())));

use crate::entity::plugin_config;
use crate::error::AppError;
use crate::host_funcs::config::extract_plugin_id;

const MAX_CHAIN_DEPTH: u8 = 3;

struct HookEntry {
    plugin_id: String,
    scope: HookScope,
    mode: HookMode,
    hook: Arc<dyn GenericHook<Context = ()>>,
}

pub struct ServerHookRegistry {
    hooks: HashMap<String, Vec<HookEntry>>,
}

impl Default for ServerHookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerHookRegistry {
    pub fn new() -> Self {
        Self {
            hooks: HashMap::new(),
        }
    }

    pub fn register<M: PluginManager + Send + Sync + ?Sized + 'static>(
        &mut self,
        hook: Arc<PluginHook<M>>,
    ) {
        let plugin_id = hook.plugin_id().to_string();
        let scope = hook.scope;
        let mode = *hook.mode();

        for topic in hook.topics() {
            let entry = HookEntry {
                plugin_id: plugin_id.clone(),
                scope,
                mode,
                hook: hook.clone(),
            };
            self.hooks.entry(topic.clone()).or_default().push(entry);
        }
    }

    pub fn unregister_plugin(&mut self, plugin_id: &str) {
        for entries in self.hooks.values_mut() {
            entries.retain(|e| e.plugin_id != plugin_id);
        }
    }

    fn get_hooks(&self, topic: &str) -> Option<&Vec<HookEntry>> {
        self.hooks.get(topic)
    }

    pub fn iter_summaries(&self) -> Vec<(String, String, HookScope, HookMode)> {
        self.hooks
            .iter()
            .flat_map(|(topic, entries)| {
                entries
                    .iter()
                    .map(move |e| (topic.clone(), e.plugin_id.clone(), e.scope, e.mode))
            })
            .collect()
    }
}

pub type SharedHookRegistry = Arc<RwLock<ServerHookRegistry>>;

pub fn new_shared_registry() -> SharedHookRegistry {
    Arc::new(RwLock::new(ServerHookRegistry::new()))
}

pub type ResourceEnablements = HashMap<String, i32>;

fn scope_priority(scope: &str) -> u8 {
    match scope {
        "contest_problem" => 3,
        "contest" => 2,
        "problem" => 1,
        _ => 0,
    }
}

/// Merge the plugin_config rows that apply to one resource (multiple scopes ×
/// multiple namespaces per plugin) into a single enablement + position per
/// plugin, ORDER-INDEPENDENTLY.
///
/// The source query has no `ORDER BY`, so Postgres may return these rows in any
/// order; the reducer must therefore be commutative. Semantics (as the per-row
/// `enabled.is_some()` guard has always intended):
///   * position — from the most specific scope present
///     (contest_problem > contest > problem); ties across a plugin's namespaces
///     take the smallest position.
///   * enabled  — inherited from the most specific scope that actually *sets*
///     it; a more specific row whose `enabled` is NULL overrides position only,
///     never enablement. Within one scope, any `true` wins over `false`.
///
/// A plugin is enabled iff its merged enablement is `Some(true)`.
///
/// Rows are `(stored_namespace, scope, enabled, position)`.
fn merge_resource_enablements<I>(rows: I) -> ResourceEnablements
where
    I: IntoIterator<Item = (String, String, Option<bool>, i32)>,
{
    struct Agg {
        pos_pri: u8,
        pos: i32,
        enabled_pri: Option<u8>,
        enabled: Option<bool>,
    }

    let mut best: HashMap<String, Agg> = HashMap::new();

    for (namespace, scope, enabled, position) in rows {
        let pid = extract_plugin_id(&namespace).to_string();
        let pri = scope_priority(&scope);

        let agg = best.entry(pid).or_insert(Agg {
            pos_pri: 0,
            pos: 0,
            enabled_pri: None,
            enabled: None,
        });

        // Position: the most specific scope wins; within a scope take the min.
        match pri.cmp(&agg.pos_pri) {
            std::cmp::Ordering::Greater => {
                agg.pos_pri = pri;
                agg.pos = position;
            }
            std::cmp::Ordering::Equal => {
                agg.pos = agg.pos.min(position);
            }
            std::cmp::Ordering::Less => {}
        }

        // Enablement: only rows that actually set it participate, so a NULL at a
        // more specific scope can never clobber an inherited value. Priority
        // decides which scope's value wins; ties OR together (true beats false).
        if let Some(e) = enabled {
            match agg.enabled_pri {
                None => {
                    agg.enabled_pri = Some(pri);
                    agg.enabled = Some(e);
                }
                Some(cur) => match pri.cmp(&cur) {
                    std::cmp::Ordering::Greater => {
                        agg.enabled_pri = Some(pri);
                        agg.enabled = Some(e);
                    }
                    std::cmp::Ordering::Equal => {
                        agg.enabled = Some(agg.enabled == Some(true) || e);
                    }
                    std::cmp::Ordering::Less => {}
                },
            }
        }
    }

    best.into_iter()
        .filter(|(_, agg)| agg.enabled == Some(true))
        .map(|(pid, agg)| (pid, agg.pos))
        .collect()
}

/// Where a plugin that owns a contest type sorts among resource-scoped hooks
/// when no config row gives it a position: before the opt-in ones, so the
/// format's own rules are checked first.
const CONTEST_TYPE_OWNER_POSITION: i32 = i32::MIN;

/// Add the plugin that registered the contest's type to `enablements`. A
/// format's own hooks are part of its rules, not an optional feature, so its
/// plugin never has to be switched on for its own contests. A position set by
/// config is kept. The host learns only which plugin owns the type, nothing
/// about what the format does.
fn add_contest_type_owner(enablements: &mut ResourceEnablements, owner: Option<&str>) {
    if let Some(owner) = owner {
        enablements
            .entry(owner.to_string())
            .or_insert(CONTEST_TYPE_OWNER_POSITION);
    }
}

/// The plugins whose resource-scoped hooks run for a submission to
/// `problem_id`: those enabled by plugin config for the problem, contest, or
/// contest problem, plus, for a contest submission, the plugin that owns the
/// contest's type. Practice submissions (`contest_id: None`) get
/// config-enabled plugins only; a practice submission has no contest whose
/// rules apply.
///
/// The owner comes from the contest's own `contest_type` column. A contest
/// with no type gets no owner, even though its submissions are judged under
/// a fallback type: a fallback is not a choice of format, and must not impose
/// that format's rules on the contest.
///
/// Every hook dispatch site uses this, so `before_submission`,
/// `after_submission` and `after_judging` agree on which plugins run.
pub async fn resource_enablements<C: ConnectionTrait>(
    db: &C,
    contest_types: &crate::registry::ContestTypeRegistry,
    problem_id: i32,
    contest_id: Option<i32>,
) -> Result<ResourceEnablements, AppError> {
    let mut enablements = fetch_resource_enablements(problem_id, contest_id, db).await?;
    let Some(contest_id) = contest_id else {
        return Ok(enablements);
    };
    let contest_type = crate::entity::contest::Entity::find_by_id(contest_id)
        .one(db)
        .await?
        .and_then(|c| c.contest_type);
    if let Some(contest_type) = contest_type {
        let owner = contest_types
            .read()
            .await
            .get(&contest_type)
            .map(|h| h.plugin_id.clone());
        add_contest_type_owner(&mut enablements, owner.as_deref());
    }
    Ok(enablements)
}

async fn fetch_resource_enablements<C: ConnectionTrait>(
    problem_id: i32,
    contest_id: Option<i32>,
    db: &C,
) -> Result<ResourceEnablements, AppError> {
    use sea_orm::sea_query::Condition;

    let problem_ref = crate::models::plugin_config::config_key::problem(problem_id);

    let mut condition = Condition::any().add(
        Condition::all()
            .add(plugin_config::Column::Scope.eq("problem"))
            .add(plugin_config::Column::RefId.eq(&problem_ref)),
    );

    if let Some(cid) = contest_id {
        let contest_ref = crate::models::plugin_config::config_key::contest(cid);
        let cp_ref = crate::models::plugin_config::config_key::contest_problem(cid, problem_id);

        condition = condition
            .add(
                Condition::all()
                    .add(plugin_config::Column::Scope.eq("contest"))
                    .add(plugin_config::Column::RefId.eq(&contest_ref)),
            )
            .add(
                Condition::all()
                    .add(plugin_config::Column::Scope.eq("contest_problem"))
                    .add(plugin_config::Column::RefId.eq(&cp_ref)),
            );
    }

    let rows = plugin_config::Entity::find()
        .filter(condition)
        .all(db)
        .await?;

    Ok(merge_resource_enablements(
        rows.into_iter()
            .map(|r| (r.namespace, r.scope, r.enabled, r.position)),
    ))
}

#[derive(Debug)]
pub enum HookOutcome {
    Allowed(serde_json::Value),
    Rejected {
        code: String,
        message: String,
        status_code: u16,
        details: Option<serde_json::Value>,
    },
    Stopped,
}

fn parse_reject_detail(reason: &str) -> (String, String, u16, Option<serde_json::Value>) {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(reason) {
        let code = v
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or("PLUGIN_REJECTED")
            .to_string();
        let message = v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Request rejected by plugin")
            .to_string();
        let status_code = v
            .get("status_code")
            .and_then(|s| s.as_u64())
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(400);
        let details = v.get("details").cloned();
        (code, message, status_code, details)
    } else {
        ("PLUGIN_REJECTED".into(), reason.to_string(), 400, None)
    }
}

pub async fn dispatch_hooks(
    topic: &str,
    event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
) -> Result<HookOutcome, AppError> {
    dispatch_hooks_inner(
        topic,
        event_payload,
        enabled_plugins,
        hook_registry,
        0,
        None,
    )
    .await
}

pub async fn dispatch_hooks_typed<E: HookEvent>(
    event: &E,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
) -> Result<HookOutcome, AppError> {
    let payload = serde_json::to_value(event)
        .map_err(|e| AppError::Internal(format!("Failed to serialize hook event: {e}")))?;
    dispatch_hooks(E::TOPIC, payload, enabled_plugins, hook_registry).await
}

/// Admission effects are reserved to the registered contest owner. Other
/// gates may reject or preserve the payload, but cannot inject, replace, or
/// remove the owner's effects.
pub(crate) async fn dispatch_submission_hooks(
    event: &broccoli_server_sdk::types::BeforeSubmissionEvent,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
    admission_owner: Option<&str>,
) -> Result<HookOutcome, AppError> {
    let payload = serde_json::to_value(event)
        .map_err(|e| AppError::Internal(format!("Failed to serialize hook event: {e}")))?;
    dispatch_hooks_inner(
        "before_submission",
        payload,
        enabled_plugins,
        hook_registry,
        0,
        admission_owner,
    )
    .await
}

pub fn dispatch_hooks_background(
    topic: String,
    event_payload: serde_json::Value,
    enabled_plugins: Option<ResourceEnablements>,
    hook_registry: SharedHookRegistry,
    event_id: Option<String>,
) {
    if let Some(ref id) = event_id {
        let mut cache = DISPATCHED_EVENTS.lock().unwrap_or_else(|e| e.into_inner());
        if cache.get(id).is_some() {
            tracing::debug!(event_id = %id, topic, "Skipping duplicate background hook event");
            return;
        }
        cache.put(id.clone(), ());
    }

    tokio::spawn(async move {
        match dispatch_hooks(
            &topic,
            event_payload,
            enabled_plugins.as_ref(),
            &hook_registry,
        )
        .await
        {
            Ok(outcome) => match outcome {
                HookOutcome::Allowed(_) => {}
                HookOutcome::Rejected { code, message, .. } => {
                    tracing::warn!(
                        topic,
                        code,
                        message,
                        "Background hook returned Reject (no effect — response already sent)",
                    );
                }
                HookOutcome::Stopped => {
                    tracing::warn!(
                        topic,
                        "Background hook returned Stop (no effect — response already sent)",
                    );
                }
            },
            Err(e) => {
                tracing::warn!(topic, "Background hook dispatch failed: {e:?}");
            }
        }
    });
}

pub fn dispatch_hooks_background_typed<E: HookEvent + Send + 'static>(
    event: E,
    enabled_plugins: Option<ResourceEnablements>,
    hook_registry: SharedHookRegistry,
    event_id: Option<String>,
) {
    let topic = E::TOPIC.to_string();
    let payload = match serde_json::to_value(&event) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("Failed to serialize background hook event: {e}");
            return;
        }
    };
    dispatch_hooks_background(topic, payload, enabled_plugins, hook_registry, event_id);
}

async fn dispatch_hooks_inner(
    topic: &str,
    mut event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
    depth: u8,
    admission_owner: Option<&str>,
) -> Result<HookOutcome, AppError> {
    if depth >= MAX_CHAIN_DEPTH {
        tracing::warn!(
            topic,
            depth,
            "Hook chain depth exceeded, ignoring chained events"
        );
        return Ok(HookOutcome::Allowed(event_payload));
    }

    struct DispatchEntry {
        plugin_id: String,
        position: i32,
        is_global: bool,
        mode: HookMode,
        hook: Arc<dyn GenericHook<Context = ()>>,
    }

    let mut dispatch_list: Vec<DispatchEntry> = {
        let registry = hook_registry.read().await;
        let entries = match registry.get_hooks(topic) {
            Some(e) if !e.is_empty() => e,
            _ => return Ok(HookOutcome::Allowed(event_payload)),
        };

        let mut list: Vec<DispatchEntry> = Vec::new();

        for entry in entries {
            match entry.scope {
                HookScope::Global => {
                    list.push(DispatchEntry {
                        plugin_id: entry.plugin_id.clone(),
                        position: i32::MIN,
                        is_global: true,
                        mode: entry.mode,
                        hook: entry.hook.clone(),
                    });
                }
                HookScope::Resource => {
                    if let Some(enabled) = enabled_plugins
                        && let Some(&pos) = enabled.get(&entry.plugin_id)
                    {
                        list.push(DispatchEntry {
                            plugin_id: entry.plugin_id.clone(),
                            position: pos,
                            is_global: false,
                            mode: entry.mode,
                            hook: entry.hook.clone(),
                        });
                    }
                }
            }
        }

        list
    };

    dispatch_list.sort_by(|a, b| {
        a.mode
            .cmp(&b.mode)
            .then(b.is_global.cmp(&a.is_global))
            .then(a.position.cmp(&b.position))
    });

    for entry in &dispatch_list {
        let generic_event = GenericEvent {
            topic: topic.to_string(),
            payload: event_payload.clone(),
        };

        match entry.mode {
            HookMode::Blocking => {
                let hook_timeout = std::time::Duration::from_secs(30);
                let action = match tokio::time::timeout(
                    hook_timeout,
                    entry.hook.on_event((), &generic_event),
                )
                .await
                {
                    Ok(result) => result.map_err(|e| {
                        tracing::error!(topic, "Hook execution error: {e}");
                        AppError::Internal(format!("Hook execution failed: {e}"))
                    })?,
                    Err(_) => {
                        tracing::error!(topic, "Hook execution timed out (30s)");
                        return Err(AppError::Internal("Hook execution timed out".into()));
                    }
                };

                match action {
                    HookAction::Pass => {}
                    HookAction::Modified(mut new_event) => {
                        if topic == "before_submission" {
                            // Admission must describe the same request that the
                            // handler inserts, including for subsequent gates.
                            let decode = |payload| {
                                serde_json::from_value::<
                                    broccoli_server_sdk::types::BeforeSubmissionEvent,
                                >(payload)
                                .map_err(|_| {
                                    AppError::Internal("Invalid before-submission event".into())
                                })
                            };
                            if decode(event_payload.clone())? != decode(new_event.payload.clone())?
                            {
                                return Err(AppError::Internal(
                                    "Submission hooks cannot rewrite request attributes".into(),
                                ));
                            }
                        }
                        if topic == "before_submission"
                            && admission_owner != Some(entry.plugin_id.as_str())
                        {
                            let effects = event_payload.get("submission_admission");
                            let modified = new_event.payload.as_object_mut().ok_or_else(|| {
                                AppError::Internal("Invalid before-submission event".into())
                            })?;
                            if modified
                                .get("submission_admission")
                                .is_some_and(|value| Some(value) != effects)
                            {
                                return Err(AppError::Internal(
                                    "Only the contest owner may change submission admission".into(),
                                ));
                            }
                            // Ordinary typed hooks may omit unknown fields; their
                            // rewrites must preserve the owner's admission effects.
                            if let Some(effects) = effects {
                                modified.insert("submission_admission".into(), effects.clone());
                            }
                        }
                        event_payload = new_event.payload;
                    }
                    HookAction::Stop => {
                        if topic == "before_submission" {
                            // Stop accepts the current event. Retain any owner
                            // admission already supplied by an earlier hook.
                            return Ok(HookOutcome::Allowed(event_payload));
                        }
                        return Ok(HookOutcome::Stopped);
                    }
                    HookAction::Reject(reason) => {
                        let (code, message, status_code, details) = parse_reject_detail(&reason);
                        if code == PLUGIN_RUNTIME_ERROR_CODE {
                            tracing::error!(topic, %code, %message, "Hook infrastructure error");
                            return Err(AppError::Internal(message));
                        }
                        return Ok(HookOutcome::Rejected {
                            code,
                            message,
                            status_code,
                            details,
                        });
                    }
                    HookAction::Chain(events) => {
                        for chained in events {
                            let result = Box::pin(dispatch_hooks_inner(
                                &chained.topic,
                                chained.payload,
                                enabled_plugins,
                                hook_registry,
                                depth + 1,
                                admission_owner,
                            ))
                            .await?;
                            if let HookOutcome::Rejected { .. } | HookOutcome::Stopped = result {
                                return Ok(result);
                            }
                        }
                        return Ok(HookOutcome::Allowed(event_payload));
                    }
                }
            }
            HookMode::Notify => {
                let notify_result = match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    entry.hook.on_event((), &generic_event),
                )
                .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        tracing::warn!(topic, "Notify hook timed out (30s), skipping");
                        continue;
                    }
                };
                match notify_result {
                    Ok(action) => match &action {
                        HookAction::Pass => {}
                        HookAction::Reject(reason) => {
                            tracing::debug!(
                                topic,
                                "Notify hook returned Reject (ignored): {reason}",
                            );
                        }
                        HookAction::Stop => {
                            tracing::debug!(topic, "Notify hook returned Stop (ignored)");
                        }
                        HookAction::Modified(_) => {
                            tracing::debug!(topic, "Notify hook returned Modified (ignored)");
                        }
                        HookAction::Chain(_) => {
                            tracing::debug!(topic, "Notify hook returned Chain (ignored)");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(topic, "Notify hook error (ignored): {e}");
                    }
                }
            }
        }
    }

    Ok(HookOutcome::Allowed(event_payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::async_trait::async_trait;
    use common::event::GenericEvent;
    use common::hook::GenericHookAction;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockHook {
        id: String,
        topics: Vec<String>,
        action: GenericHookAction,
        call_count: AtomicUsize,
    }

    impl MockHook {
        fn new(id: &str, topic: &str, action: GenericHookAction) -> Self {
            Self {
                id: id.into(),
                topics: vec![topic.into()],
                action,
                call_count: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl GenericHook for MockHook {
        type Context = ();

        fn id(&self) -> &str {
            &self.id
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: (),
            _event: &GenericEvent,
        ) -> anyhow::Result<GenericHookAction> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            Ok(match &self.action {
                HookAction::Pass => HookAction::Pass,
                HookAction::Stop => HookAction::Stop,
                HookAction::Reject(r) => HookAction::Reject(r.clone()),
                HookAction::Modified(e) => HookAction::Modified(e.clone()),
                HookAction::Chain(v) => HookAction::Chain(v.clone()),
            })
        }
    }

    struct FailingHook {
        id: String,
        topics: Vec<String>,
    }

    #[async_trait]
    impl GenericHook for FailingHook {
        type Context = ();

        fn id(&self) -> &str {
            &self.id
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: (),
            _event: &GenericEvent,
        ) -> anyhow::Result<GenericHookAction> {
            Err(anyhow::anyhow!("WASM crash"))
        }
    }

    fn register_direct(
        registry: &mut ServerHookRegistry,
        hook: Arc<dyn GenericHook<Context = ()>>,
        scope: HookScope,
        mode: HookMode,
    ) {
        let plugin_id = hook.id().to_string();
        for topic in hook.topics() {
            registry
                .hooks
                .entry(topic.clone())
                .or_default()
                .push(HookEntry {
                    plugin_id: plugin_id.clone(),
                    scope,
                    mode,
                    hook: hook.clone(),
                });
        }
    }

    async fn dispatch_global(
        registry: &SharedHookRegistry,
        topic: &str,
        payload: serde_json::Value,
    ) -> Result<HookOutcome, AppError> {
        dispatch_hooks(topic, payload, None, registry).await
    }

    #[tokio::test]
    async fn admission_effects_can_only_be_created_or_changed_by_the_contest_owner() {
        let event = broccoli_server_sdk::types::BeforeSubmissionEvent {
            user_id: 1,
            problem_id: 2,
            contest_id: Some(3),
            contest_type: "example".into(),
            language: "cpp".into(),
            file_count: 1,
        };
        let payload = serde_json::to_value(&event).unwrap();
        let mut admitted = payload.clone();
        admitted["submission_admission"] = serde_json::json!({"opaque":true});
        let mut cases = vec![
            (Some("owner"), "other", admitted.clone(), true),
            (Some("owner"), "other", payload.clone(), true),
            (
                Some("owner"),
                "other",
                serde_json::json!("invalid event"),
                false,
            ),
            (
                Some("owner"),
                "other",
                serde_json::json!({"submission_admission":null}),
                false,
            ),
            (None, "owner", admitted.clone(), false),
        ];
        for (field, replacement) in [
            ("user_id", serde_json::json!(99)),
            ("problem_id", serde_json::json!(99)),
            ("contest_id", serde_json::Value::Null),
            ("contest_type", serde_json::json!("another-format")),
            ("language", serde_json::json!("python")),
            ("file_count", serde_json::json!(99)),
        ] {
            for plugin in ["owner", "other"] {
                let mut rewritten = admitted.clone();
                rewritten[field] = replacement.clone();
                cases.push((Some("owner"), plugin, rewritten, false));
            }
        }
        let cases = cases
            .into_iter()
            .map(|(owner, plugin, payload, succeeds)| {
                (
                    owner,
                    plugin,
                    HookAction::Modified(GenericEvent {
                        topic: "before_submission".into(),
                        payload,
                    }),
                    succeeds,
                )
            })
            .chain([(Some("owner"), "other", HookAction::Stop, true)]);
        for (owner, second_plugin, second_action, succeeds) in cases {
            let registry = new_shared_registry();
            let mut hooks = registry.write().await;
            for (id, action) in [
                (
                    "owner",
                    HookAction::Modified(GenericEvent {
                        topic: "before_submission".into(),
                        payload: admitted.clone(),
                    }),
                ),
                (second_plugin, second_action),
            ] {
                register_direct(
                    &mut hooks,
                    Arc::new(MockHook::new(id, "before_submission", action)),
                    HookScope::Global,
                    HookMode::Blocking,
                );
            }
            drop(hooks);
            let result = dispatch_submission_hooks(&event, None, &registry, owner).await;
            assert_eq!(result.is_ok(), succeeds);
            if succeeds {
                let HookOutcome::Allowed(payload) = result.unwrap() else {
                    panic!("expected admission")
                };
                assert_eq!(
                    payload["submission_admission"],
                    serde_json::json!({"opaque":true})
                );
            }
        }
        let registry = new_shared_registry();
        register_direct(
            &mut *registry.write().await,
            Arc::new(MockHook::new(
                "other",
                "before_submission",
                HookAction::Modified(GenericEvent {
                    topic: "before_submission".into(),
                    payload: admitted,
                }),
            )),
            HookScope::Global,
            HookMode::Blocking,
        );
        assert!(
            dispatch_submission_hooks(&event, None, &registry, Some("owner"))
                .await
                .is_err()
        );
    }

    #[test]
    fn merge_resource_enablements_inherits_enabled_regardless_of_row_order() {
        // A plugin enabled at `problem` scope, plus a more specific
        // `contest_problem` row that sets only position (enabled = NULL). The
        // inherited `true` must survive whichever order the rows arrive in, and
        // the position must come from the more specific scope.
        let problem_row = (
            "cooldown:main".to_string(),
            "problem".to_string(),
            Some(true),
            5,
        );
        let cp_row = (
            "cooldown:main".to_string(),
            "contest_problem".to_string(),
            None,
            10,
        );

        let forward = merge_resource_enablements([problem_row.clone(), cp_row.clone()]);
        let reverse = merge_resource_enablements([cp_row, problem_row]);

        assert_eq!(forward, reverse);
        assert_eq!(forward.get("cooldown"), Some(&10));
    }

    #[test]
    fn merge_resource_enablements_specific_disable_overrides_inherited_enable() {
        // An explicit `false` at the more specific scope must win over an
        // inherited `true`, in either arrival order.
        let problem_row = (
            "cooldown:main".to_string(),
            "problem".to_string(),
            Some(true),
            5,
        );
        let cp_row = (
            "cooldown:main".to_string(),
            "contest_problem".to_string(),
            Some(false),
            10,
        );

        let forward = merge_resource_enablements([problem_row.clone(), cp_row.clone()]);
        let reverse = merge_resource_enablements([cp_row, problem_row]);

        assert_eq!(forward, reverse);
        assert!(!forward.contains_key("cooldown"));
    }

    #[test]
    fn merge_resource_enablements_true_wins_within_a_scope() {
        // Two namespaces of the same plugin at the same scope: any `true` wins,
        // and position is the min across the scope's namespaces.
        let a = (
            "cooldown:a".to_string(),
            "problem".to_string(),
            Some(false),
            3,
        );
        let b = (
            "cooldown:b".to_string(),
            "problem".to_string(),
            Some(true),
            7,
        );

        let forward = merge_resource_enablements([a.clone(), b.clone()]);
        let reverse = merge_resource_enablements([b, a]);

        assert_eq!(forward, reverse);
        assert_eq!(forward.get("cooldown"), Some(&3));
    }

    #[tokio::test]
    async fn blocking_hook_reject_returns_rejected_with_parsed_code() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "rejector",
            "test",
            HookAction::Reject(r#"{"code":"NO","message":"nope","status_code":429}"#.into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Rejected { ref code, .. } if code == "NO"));
        assert_eq!(hook.calls(), 1);
    }

    #[tokio::test]
    async fn blocking_stop_halts_chain_and_skips_remaining_hooks() {
        let registry = new_shared_registry();
        let stop = Arc::new(MockHook::new("stopper", "test", HookAction::Stop));
        let pass = Arc::new(MockHook::new("passer", "test", HookAction::Pass));
        {
            let mut r = registry.write().await;
            register_direct(&mut r, stop.clone(), HookScope::Global, HookMode::Blocking);
            register_direct(&mut r, pass.clone(), HookScope::Global, HookMode::Blocking);
        }

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Stopped));
        assert_eq!(stop.calls(), 1);
        assert_eq!(pass.calls(), 0);
    }

    #[tokio::test]
    async fn notify_hook_reject_does_not_block_event() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "notify_rejector",
            "test",
            HookAction::Reject("denied".into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Global,
            HookMode::Notify,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Allowed(_)));
        assert_eq!(hook.calls(), 1);
    }

    #[tokio::test]
    async fn notify_hook_stop_does_not_halt_chain() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new("notify_stopper", "test", HookAction::Stop));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Global,
            HookMode::Notify,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Allowed(_)));
        assert_eq!(hook.calls(), 1);
    }

    #[tokio::test]
    async fn notify_hook_wasm_crash_does_not_propagate_error() {
        let registry = new_shared_registry();
        let hook = Arc::new(FailingHook {
            id: "crasher".into(),
            topics: vec!["test".into()],
        });
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Notify,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Allowed(_)));
    }

    #[tokio::test]
    async fn blocking_hook_wasm_crash_propagates_as_app_error() {
        let registry = new_shared_registry();
        let hook = Arc::new(FailingHook {
            id: "crasher".into(),
            topics: vec!["test".into()],
        });
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn blocking_hooks_execute_before_notify_hooks_regardless_of_registration_order() {
        let registry = new_shared_registry();
        let notify = Arc::new(MockHook::new("notify", "test", HookAction::Pass));
        let blocking = Arc::new(MockHook::new("blocker", "test", HookAction::Stop));
        {
            let mut r = registry.write().await;
            register_direct(&mut r, notify.clone(), HookScope::Global, HookMode::Notify);
            register_direct(
                &mut r,
                blocking.clone(),
                HookScope::Global,
                HookMode::Blocking,
            );
        }

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Stopped));
        assert_eq!(blocking.calls(), 1);
        assert_eq!(notify.calls(), 0);
    }

    #[tokio::test]
    async fn topic_with_no_hooks_returns_allowed_with_original_payload() {
        let registry = new_shared_registry();
        let result = dispatch_global(&registry, "nonexistent", serde_json::json!({"x": 1}))
            .await
            .unwrap();
        match result {
            HookOutcome::Allowed(payload) => assert_eq!(payload["x"], 1),
            other => panic!("Expected Allowed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn unregistered_plugin_hooks_no_longer_fire() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new("plugin_a", "test", HookAction::Stop));
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Stopped));

        registry.write().await.unregister_plugin("plugin_a");

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Allowed(_)));
    }

    #[tokio::test]
    async fn modified_payload_replaces_original_for_subsequent_hooks() {
        let registry = new_shared_registry();
        let modifier = Arc::new(MockHook::new(
            "modifier",
            "test",
            HookAction::Modified(GenericEvent {
                topic: "test".into(),
                payload: serde_json::json!({"modified": true}),
            }),
        ));
        let pass = Arc::new(MockHook::new("passer", "test", HookAction::Pass));
        {
            let mut r = registry.write().await;
            register_direct(&mut r, modifier, HookScope::Global, HookMode::Blocking);
            register_direct(&mut r, pass, HookScope::Global, HookMode::Blocking);
        }

        let result = dispatch_global(&registry, "test", serde_json::json!({"original": true}))
            .await
            .unwrap();
        match result {
            HookOutcome::Allowed(payload) => {
                assert_eq!(payload["modified"], true);
                assert!(payload.get("original").is_none());
            }
            other => panic!("Expected Allowed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn reject_with_details_propagates_structured_data() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "limiter",
            "test",
            HookAction::Reject(
                r#"{"code":"LIMIT","message":"limit hit","status_code":429,"details":{"remaining":5,"total":10}}"#.into(),
            ),
        ));
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        match result {
            HookOutcome::Rejected { code, details, .. } => {
                assert_eq!(code, "LIMIT");
                let details = details.expect("details should be Some");
                assert_eq!(details["remaining"], 5);
                assert_eq!(details["total"], 10);
            }
            other => panic!("Expected Rejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn reject_without_details_returns_none() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "rejector",
            "test",
            HookAction::Reject(r#"{"code":"NO","message":"nope","status_code":400}"#.into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        match result {
            HookOutcome::Rejected { code, details, .. } => {
                assert_eq!(code, "NO");
                assert!(details.is_none(), "details should be None when absent");
            }
            other => panic!("Expected Rejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn reject_with_plain_string_returns_no_details() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "rejector",
            "test",
            HookAction::Reject("plain text rejection".into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        match result {
            HookOutcome::Rejected {
                code,
                message,
                details,
                ..
            } => {
                assert_eq!(code, "PLUGIN_REJECTED");
                assert_eq!(message, "plain text rejection");
                assert!(details.is_none());
            }
            other => panic!("Expected Rejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn resource_scoped_hook_fires_when_plugin_is_in_enablements() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new("cooldown", "test", HookAction::Stop));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Resource,
            HookMode::Blocking,
        );

        let mut enabled = HashMap::new();
        enabled.insert("cooldown".to_string(), 0);
        let result = dispatch_hooks("test", serde_json::json!({}), Some(&enabled), &registry)
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Stopped));
        assert_eq!(hook.calls(), 1);
    }

    #[tokio::test]
    async fn resource_scoped_hook_skipped_when_no_enablements() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new("cooldown", "test", HookAction::Stop));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Resource,
            HookMode::Blocking,
        );

        let result = dispatch_hooks("test", serde_json::json!({}), None, &registry)
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Allowed(_)));
        assert_eq!(hook.calls(), 0);
    }

    #[test]
    fn contest_type_owner_is_enabled_ahead_of_opt_in_plugins() {
        let mut enabled = HashMap::new();
        enabled.insert("cooldown".to_string(), 0);
        add_contest_type_owner(&mut enabled, Some("codelink-bracket"));
        assert_eq!(
            enabled.get("codelink-bracket"),
            Some(&CONTEST_TYPE_OWNER_POSITION)
        );
        assert_eq!(enabled.get("cooldown"), Some(&0));
    }

    #[test]
    fn contest_type_owner_keeps_a_position_set_by_config() {
        let mut enabled = HashMap::new();
        enabled.insert("codelink-bracket".to_string(), 5);
        add_contest_type_owner(&mut enabled, Some("codelink-bracket"));
        assert_eq!(enabled.get("codelink-bracket"), Some(&5));
    }

    #[test]
    fn no_contest_type_owner_leaves_enablements_alone() {
        let mut enabled = HashMap::new();
        add_contest_type_owner(&mut enabled, None);
        assert!(enabled.is_empty());
    }

    #[tokio::test]
    async fn resource_scoped_hook_skipped_when_plugin_not_in_enablements() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new("cooldown", "test", HookAction::Stop));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Resource,
            HookMode::Blocking,
        );

        let mut enabled = HashMap::new();
        enabled.insert("other-plugin".to_string(), 0);
        let result = dispatch_hooks("test", serde_json::json!({}), Some(&enabled), &registry)
            .await
            .unwrap();
        assert!(matches!(result, HookOutcome::Allowed(_)));
        assert_eq!(hook.calls(), 0);
    }

    #[tokio::test]
    async fn plugin_runtime_error_reject_returns_internal_error_not_400() {
        let registry = new_shared_registry();
        let reject_json = serde_json::json!({
            "code": PLUGIN_RUNTIME_ERROR_CODE,
            "message": "Plugin 'x' hook 'y' failed: OOM",
            "status_code": 500,
        });
        let hook = Arc::new(MockHook::new(
            "crasher",
            "test",
            HookAction::Reject(reject_json.to_string()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Blocking,
        );

        let result = dispatch_global(&registry, "test", serde_json::json!({})).await;
        assert!(
            result.is_err(),
            "Plugin runtime error should return Err(Internal), not Ok(Rejected)"
        );
    }
}
