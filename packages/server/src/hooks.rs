use crate::error::HostCode;
use std::collections::HashMap;
use std::sync::Arc;

use broccoli_server_sdk::types::HookEvent;
use common::event::GenericEvent;
use common::hook::{GenericHook, HookAction};
use plugin_core::admission::CallClass;
use plugin_core::error::PluginError;
use plugin_core::executor::CallOptions;
use plugin_core::hook::{HookMode, HookScope, PLUGIN_RUNTIME_ERROR_CODE, PluginHook};
use plugin_core::traits::PluginManager;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use tokio::sync::RwLock;

/// How long one dispatch holds a background event before the re-scan may
/// run it again elsewhere: longer than a dispatch's hooks take, so a live
/// dispatch is not repeated, and short enough that an event whose replica
/// died runs again soon.
const EVENT_LEASE_SECS: i64 = 600;

/// How long a completed event's row is kept: a duplicate dispatch of it
/// within this is skipped.
const COMPLETED_KEPT_SECS: i64 = 7 * 24 * 3600;

/// How many events one re-scan claims.
const REDELIVERY_BATCH: i64 = 16;

/// How often a replica re-scans for events to run again.
pub const REDELIVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// How many events whose claim could not be recorded a replica keeps for
/// its next re-scan; past this the oldest are dropped, loudly.
const UNRECORDED_MAX: usize = 10_000;

/// A background event this replica could not record (the database refused
/// the claim), kept for its next re-scan to record and run.
struct Unrecorded {
    event_id: String,
    topic: String,
    payload: serde_json::Value,
    enabled_plugins: Option<ResourceEnablements>,
}

/// One dispatch's lease on a background event, fleet-wide: its row of
/// `hook_event`, which records the event (topic, payload, enablements)
/// before its hooks run. Delivery is at least once: a duplicate dispatch of
/// the event, on any replica and after a restart too, is skipped while the
/// row exists; a dispatch that completes marks it done; one whose hooks
/// could not answer, or that is dropped, gives the lease back; and the
/// re-scan ([`redeliver_once`]) runs every event that is not done once its
/// lease is free or has lapsed. Plugins take a hook event as idempotent.
struct EventClaim {
    db: common::db::DatabaseConnection,
    id: String,
    /// The `attempts` the lease was taken at, so a lapsed lease another
    /// replica took since is not given back.
    attempt: i32,
    settled: bool,
}

impl EventClaim {
    /// Records the event and takes its lease, or `None` when the event is
    /// recorded already (dispatched, being dispatched or waiting for the
    /// re-scan), on this replica or another.
    async fn take(
        db: &common::db::DatabaseConnection,
        id: &str,
        topic: &str,
        payload: &serde_json::Value,
        enabled_plugins: Option<&ResourceEnablements>,
    ) -> Result<Option<Self>, sea_orm::DbErr> {
        let enabled = enabled_plugins
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| sea_orm::DbErr::Custom(e.to_string()))?;
        let inserted = db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "INSERT INTO hook_event (event_id, topic, payload, enabled_plugins, claimed_at) \
                 VALUES ($1, $2, $3, $4, now()) ON CONFLICT DO NOTHING",
                [
                    id.into(),
                    topic.into(),
                    payload.clone().into(),
                    enabled.into(),
                ],
            ))
            .await?
            .rows_affected();
        Ok((inserted == 1).then(|| Self {
            db: db.clone(),
            id: id.to_string(),
            attempt: 1,
            settled: false,
        }))
    }

    /// Ends the lease: the event is done, or, when a hook could not answer
    /// (`unanswered`), given back for the re-scan to run again.
    async fn settle(mut self, unanswered: bool) {
        self.settled = true;
        let statement = if unanswered {
            sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "UPDATE hook_event SET claimed_at = NULL \
                  WHERE event_id = $1 AND attempts = $2 AND completed_at IS NULL",
                [self.id.clone().into(), self.attempt.into()],
            )
        } else {
            sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "UPDATE hook_event SET completed_at = COALESCE(completed_at, now()) \
                  WHERE event_id = $1",
                [self.id.clone().into()],
            )
        };
        let settled = self.db.execute_raw(statement).await;
        if let Err(e) = settled {
            tracing::warn!(event_id = %self.id, error = %e, "Ending a hook event lease failed; the event runs again once the lease lapses");
        }
    }
}

impl Drop for EventClaim {
    /// Gives the lease of a dispatch dropped before it ended back, in a task
    /// of its own: a drop never waits on the database, so a runtime shutting
    /// down with a dispatch in flight is not held up. A give-back that never
    /// runs leaves the lease to lapse.
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let db = self.db.clone();
        let id = std::mem::take(&mut self.id);
        let attempt = self.attempt;
        drop(runtime.spawn(async move {
            let released = db
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DbBackend::Postgres,
                    "UPDATE hook_event SET claimed_at = NULL \
                      WHERE event_id = $1 AND attempts = $2 AND completed_at IS NULL",
                    [id.clone().into(), attempt.into()],
                ))
                .await;
            if let Err(e) = released {
                tracing::warn!(event_id = %id, error = %e, "Giving back a hook event lease failed; it lapses instead");
            }
        }));
    }
}

use crate::entity::plugin_config;
use crate::error::AppError;
use crate::host_funcs::config::{extract_plugin_id, strip_namespace_prefix};
use crate::services::plugin_output::PluginRefusal;
use plugin_core::manifest::ConfigScope;

const MAX_CHAIN_DEPTH: u8 = 3;

/// Longest one hook call may run before its guest is cancelled.
const HOOK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Options for one hook call made now by a dispatch of `class`: an
/// interactive hook also ends with its request.
fn hook_call(class: CallClass) -> CallOptions {
    let deadline = std::time::Instant::now() + HOOK_TIMEOUT;
    match class {
        CallClass::Interactive => CallOptions::interactive(deadline.min(crate::request_deadline())),
        CallClass::Background => CallOptions::background_hook(deadline),
    }
}

fn is_unavailable(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<PluginError>()
        .is_some_and(PluginError::is_unavailable)
}

struct HookEntry {
    plugin_id: String,
    /// The plugin function the hook calls; with the plugin id it names the
    /// hook, the last tiebreak of the dispatch order.
    function: String,
    scope: HookScope,
    mode: HookMode,
    hook: Arc<dyn GenericHook<Context = CallOptions>>,
}

pub struct ServerHookRegistry {
    hooks: HashMap<String, Vec<HookEntry>>,
    /// Background events this replica could not record yet, oldest first.
    unrecorded: std::sync::Mutex<std::collections::VecDeque<Unrecorded>>,
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
            unrecorded: Default::default(),
        }
    }

    /// Keeps `event` for the next re-scan; past [`UNRECORDED_MAX`] the
    /// oldest kept event is dropped.
    fn keep_unrecorded(&self, event: Unrecorded) {
        let Ok(mut kept) = self.unrecorded.lock() else {
            return;
        };
        if kept.len() >= UNRECORDED_MAX
            && let Some(dropped) = kept.pop_front()
        {
            tracing::error!(event_id = %dropped.event_id, "Dropping a hook event that could not be recorded: too many are waiting");
        }
        kept.push_back(event);
    }

    fn take_unrecorded(&self) -> Vec<Unrecorded> {
        self.unrecorded
            .lock()
            .map(|mut kept| kept.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn register<M: PluginManager + Send + Sync + ?Sized + 'static>(
        &mut self,
        hook: Arc<PluginHook<M>>,
    ) {
        let plugin_id = hook.plugin_id().to_string();
        let function = hook.function_name().to_string();
        let scope = hook.scope;
        let mode = *hook.mode();

        for topic in hook.topics() {
            let entry = HookEntry {
                plugin_id: plugin_id.clone(),
                function: function.clone(),
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

/// One stored config row of a plugin that applies to a resource, as the
/// enablement reads it.
#[derive(Debug, Clone)]
struct EnablementRow {
    plugin_id: String,
    namespace: String,
    scope: ConfigScope,
    enabled: Option<bool>,
    position: i32,
}

impl EnablementRow {
    fn of(row: plugin_config::Model) -> Option<Self> {
        let scope = ConfigScope::parse(&row.scope)?;
        let plugin_id = match scope {
            ConfigScope::Plugin => row.ref_id,
            _ => extract_plugin_id(&row.namespace).to_string(),
        };
        Some(Self {
            plugin_id,
            namespace: strip_namespace_prefix(&row.namespace).to_string(),
            scope,
            enabled: row.enabled,
            position: row.position,
        })
    }
}

/// The cascade's scopes, most specific first: the order
/// [`broccoli_server_sdk::types::decides_enabled`] reads them in.
const SCOPES: [ConfigScope; 4] = [
    ConfigScope::ContestProblem,
    ConfigScope::Contest,
    ConfigScope::Problem,
    ConfigScope::Plugin,
];

fn specificity(scope: ConfigScope) -> usize {
    SCOPES
        .iter()
        .position(|s| *s == scope)
        .unwrap_or(SCOPES.len())
}

/// Merge the plugin_config rows that apply to one resource (scopes ×
/// namespaces per plugin) into a single enablement + position per plugin,
/// ORDER-INDEPENDENTLY (the source query has no `ORDER BY`).
///
/// * enabled — each of the plugin's namespaces is on or off by the one rule
///   the plugin's own config reads use
///   ([`broccoli_server_sdk::types::decides_enabled`]): the most specific scope
///   whose row says decides, the plugin-wide row last. The plugin runs for
///   the resource when any of its namespaces is on.
/// * position — from the most specific scope present (contest_problem >
///   contest > problem > plugin); ties across a plugin's namespaces take the
///   smallest position.
fn merge_resource_enablements<I>(rows: I) -> ResourceEnablements
where
    I: IntoIterator<Item = EnablementRow>,
{
    let mut enabled: HashMap<(String, String), [Option<bool>; 4]> = HashMap::new();
    let mut position: HashMap<String, (usize, i32)> = HashMap::new();
    for row in rows {
        let at = specificity(row.scope);
        if let Some(slot) = enabled
            .entry((row.plugin_id.clone(), row.namespace))
            .or_default()
            .get_mut(at)
        {
            *slot = row.enabled;
        }
        position
            .entry(row.plugin_id)
            .and_modify(|(best, pos)| match at.cmp(best) {
                std::cmp::Ordering::Less => (*best, *pos) = (at, row.position),
                std::cmp::Ordering::Equal => *pos = (*pos).min(row.position),
                std::cmp::Ordering::Greater => {}
            })
            .or_insert((at, row.position));
    }
    let on: std::collections::HashSet<String> = enabled
        .into_iter()
        .filter(|(_, scopes)| broccoli_server_sdk::types::decides_enabled(*scopes) == Some(true))
        .map(|((plugin_id, _), _)| plugin_id)
        .collect();
    position
        .into_iter()
        .filter(|(plugin_id, _)| on.contains(plugin_id))
        .map(|(plugin_id, (_, pos))| (plugin_id, pos))
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
/// `problem_id`: those enabled by plugin config for the problem, contest,
/// contest problem or plugin-wide, by the rule their own config reads use
/// (`broccoli_server_sdk::types::decides_enabled`), plus, for a contest submission, the plugin that owns the
/// contest's type. Practice submissions (`contest_id: None`) get
/// config-enabled plugins only; a practice submission has no contest whose
/// rules apply.
///
/// The owner comes from the contest's own `contest_type` column, resolved
/// to the type's fleet-wide provider (`registry::TypeProviders`), the
/// plugin judging runs it on, so every replica runs the same owner's hooks. A contest
/// with no type gets no owner, even though its submissions are judged under
/// a fallback type: a fallback is not a choice of format, and must not impose
/// that format's rules on the contest.
///
/// Every hook dispatch site uses this, so `before_submission`,
/// `after_submission` and `after_judging` agree on which plugins run.
pub async fn resource_enablements<C: ConnectionTrait>(
    db: &C,
    owners: &crate::registry::TypeProviders,
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
        let owner = owners.of_contest_type(&contest_type).await;
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

    // The plugin-wide rows decide a namespace no narrower row decides.
    condition = condition.add(plugin_config::Column::Scope.eq(ConfigScope::Plugin.as_str()));
    let rows = plugin_config::Entity::find()
        .filter(condition)
        .all(db)
        .await?;

    Ok(merge_resource_enablements(
        rows.into_iter().filter_map(EnablementRow::of),
    ))
}

#[derive(Debug)]
pub enum HookOutcome {
    Allowed(serde_json::Value),
    Rejected(PluginRefusal),
}

/// The parts of a blocking hook's rejection: its code, message, status and
/// details, before the host confines them to the plugin ([`PluginRefusal`]).
struct RejectDetail {
    code: String,
    message: String,
    status: u16,
    details: Option<serde_json::Value>,
}

fn parse_reject_detail(reason: &str) -> RejectDetail {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(reason) else {
        return RejectDetail {
            code: HostCode::PluginRejected.as_str().into(),
            message: reason.to_string(),
            status: 400,
            details: None,
        };
    };
    RejectDetail {
        code: v
            .get("code")
            .and_then(|c| c.as_str())
            .unwrap_or(HostCode::PluginRejected.as_str())
            .to_string(),
        message: v
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("Request rejected by plugin")
            .to_string(),
        status: v
            .get("status_code")
            .and_then(|s| s.as_u64())
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(400),
        details: v.get("details").cloned(),
    }
}

/// Run `topic`'s hooks as `class` work: a request path dispatches
/// interactive hooks, everything else background ones.
pub async fn dispatch_hooks(
    topic: &str,
    event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
    class: CallClass,
) -> Result<HookOutcome, AppError> {
    dispatch_hooks_inner(
        topic,
        event_payload,
        DispatchScope {
            enabled_plugins,
            hook_registry,
            admission_owner: None,
            class,
        },
        0,
        &mut false,
    )
    .await
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
        DispatchScope {
            enabled_plugins,
            hook_registry,
            admission_owner,
            class: CallClass::Interactive,
        },
        0,
        &mut false,
    )
    .await
}

pub fn dispatch_hooks_background(
    db: common::db::DatabaseConnection,
    topic: String,
    event_payload: serde_json::Value,
    enabled_plugins: Option<ResourceEnablements>,
    hook_registry: SharedHookRegistry,
    event_id: Option<String>,
) {
    tokio::spawn(async move {
        dispatch_hooks_once(
            &db,
            &topic,
            event_payload,
            enabled_plugins.as_ref(),
            &hook_registry,
            event_id,
        )
        .await;
    });
}

/// Dispatch an event whose response no one waits for, as
/// [`CallClass::Background`] calls, at least once fleet-wide per `event_id`
/// (see `EventClaim`): an event recorded already, on any replica, is
/// skipped. Completes when every hook has run, so a caller that awaits it
/// can count the hooks as part of its own work. An event whose claim cannot
/// be recorded now is kept for this replica's next re-scan, and one whose
/// hooks could not answer, or whose dispatch is dropped, is run again by a
/// re-scan ([`redeliver_once`]).
pub async fn dispatch_hooks_once(
    db: &common::db::DatabaseConnection,
    topic: &str,
    event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
    event_id: Option<String>,
) {
    let Some(id) = event_id else {
        deliver(None, topic, event_payload, enabled_plugins, hook_registry).await;
        return;
    };
    match EventClaim::take(db, &id, topic, &event_payload, enabled_plugins).await {
        Ok(Some(claim)) => {
            deliver(
                Some(claim),
                topic,
                event_payload,
                enabled_plugins,
                hook_registry,
            )
            .await
        }
        Ok(None) => {
            tracing::debug!(event_id = %id, topic, "Skipping a background hook event recorded already");
        }
        Err(e) => {
            tracing::warn!(event_id = %id, topic, error = %e, "Recording a hook event failed; the next re-scan records and runs it");
            hook_registry.read().await.keep_unrecorded(Unrecorded {
                event_id: id,
                topic: topic.to_string(),
                payload: event_payload,
                enabled_plugins: enabled_plugins.cloned(),
            });
        }
    }
}

/// Runs an event's hooks under `claim`, then ends the claim: done, or given
/// back when a hook could not answer.
async fn deliver(
    claim: Option<EventClaim>,
    topic: &str,
    event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
) {
    let mut unanswered = false;
    match dispatch_hooks_inner(
        topic,
        event_payload,
        DispatchScope {
            enabled_plugins,
            hook_registry,
            admission_owner: None,
            class: CallClass::Background,
        },
        0,
        &mut unanswered,
    )
    .await
    {
        Ok(HookOutcome::Allowed(_)) => {}
        Ok(HookOutcome::Rejected(refusal)) => {
            tracing::warn!(
                topic,
                code = refusal.code(),
                "Background hook returned Reject (no effect — response already sent)",
            );
        }
        Err(AppError::PluginUnavailable) => unanswered = true,
        Err(e) => {
            tracing::warn!(topic, "Background hook dispatch failed: {e:?}");
        }
    }
    if let Some(claim) = claim {
        claim.settle(unanswered).await;
    }
}

/// One re-scan of the background events: records and runs the events this
/// replica could not record, deletes completed events past the horizon, and
/// runs again each event not done whose lease is free or has lapsed (its
/// hooks could not answer, its dispatch was dropped, its replica died).
pub async fn redeliver_once(
    db: &common::db::DatabaseConnection,
    hook_registry: &SharedHookRegistry,
) -> Result<(), sea_orm::DbErr> {
    let unrecorded = hook_registry.read().await.take_unrecorded();
    for event in unrecorded {
        dispatch_hooks_once(
            db,
            &event.topic,
            event.payload,
            event.enabled_plugins.as_ref(),
            hook_registry,
            Some(event.event_id),
        )
        .await;
    }

    db.execute_raw(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        "DELETE FROM hook_event WHERE completed_at < now() - make_interval(secs => $1)",
        [(COMPLETED_KEPT_SECS as f64).into()],
    ))
    .await?;

    let due = db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "UPDATE hook_event h SET claimed_at = now(), attempts = h.attempts + 1 \
               FROM (SELECT event_id FROM hook_event \
                      WHERE completed_at IS NULL \
                        AND (claimed_at IS NULL \
                             OR claimed_at < now() - make_interval(secs => $1)) \
                      ORDER BY created_at LIMIT $2 \
                      FOR UPDATE SKIP LOCKED) due \
              WHERE h.event_id = due.event_id \
             RETURNING h.event_id, h.topic, h.payload, h.enabled_plugins, h.attempts",
            [(EVENT_LEASE_SECS as f64).into(), REDELIVERY_BATCH.into()],
        ))
        .await?;
    for row in due {
        let id: String = row.try_get("", "event_id")?;
        let topic: String = row.try_get("", "topic")?;
        let payload: serde_json::Value = row.try_get("", "payload")?;
        let enabled: Option<serde_json::Value> = row.try_get("", "enabled_plugins")?;
        let attempt: i32 = row.try_get("", "attempts")?;
        let enabled_plugins = match enabled.map(serde_json::from_value).transpose() {
            Ok(enabled) => enabled,
            Err(e) => {
                tracing::error!(event_id = %id, error = %e, "A recorded hook event's enablements do not parse; running it for global hooks only");
                None
            }
        };
        let claim = EventClaim {
            db: db.clone(),
            id,
            attempt,
            settled: false,
        };
        deliver(
            Some(claim),
            &topic,
            payload,
            enabled_plugins.as_ref(),
            hook_registry,
        )
        .await;
    }
    Ok(())
}

/// Re-scans the background events every [`REDELIVERY_INTERVAL`] until
/// `cancel`; a re-scan in flight when it fires is dropped, and the leases it
/// held are given back.
pub async fn run_redelivery(
    db: common::db::DatabaseConnection,
    hook_registry: SharedHookRegistry,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let mut tick = tokio::time::interval(REDELIVERY_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                tokio::select! {
                    scanned = redeliver_once(&db, &hook_registry) => {
                        if let Err(e) = scanned {
                            tracing::warn!(error = %e, "Hook event re-scan failed");
                        }
                    }
                    _ = cancel.changed() => return,
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return;
                }
            }
        }
    }
}

pub fn dispatch_hooks_background_typed<E: HookEvent + Send + 'static>(
    db: common::db::DatabaseConnection,
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
    dispatch_hooks_background(db, topic, payload, enabled_plugins, hook_registry, event_id);
}

/// What every hook of one dispatch, chained events included, runs with.
#[derive(Clone, Copy)]
struct DispatchScope<'a> {
    enabled_plugins: Option<&'a ResourceEnablements>,
    hook_registry: &'a SharedHookRegistry,
    admission_owner: Option<&'a str>,
    class: CallClass,
}

/// Runs `topic`'s hooks; `unanswered` is set when a hook could not answer
/// (a notify hook's too, though the dispatch goes on without it).
async fn dispatch_hooks_inner(
    topic: &str,
    mut event_payload: serde_json::Value,
    scope: DispatchScope<'_>,
    depth: u8,
    unanswered: &mut bool,
) -> Result<HookOutcome, AppError> {
    let DispatchScope {
        enabled_plugins,
        hook_registry,
        admission_owner,
        class,
    } = scope;
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
        function: String,
        position: i32,
        is_global: bool,
        mode: HookMode,
        hook: Arc<dyn GenericHook<Context = CallOptions>>,
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
                        function: entry.function.clone(),
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
                            function: entry.function.clone(),
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

    // A total order: blocking before notify, global before resource-scoped,
    // then by position, and hooks that tie on all of these (every global
    // hook) by plugin id and function. Every replica runs a topic's hooks in
    // the same order whatever order it registered them in, so the same
    // request gets the same first rejection everywhere.
    dispatch_list.sort_by(|a, b| {
        a.mode
            .cmp(&b.mode)
            .then(b.is_global.cmp(&a.is_global))
            .then(a.position.cmp(&b.position))
            .then_with(|| a.plugin_id.cmp(&b.plugin_id))
            .then_with(|| a.function.cmp(&b.function))
    });

    // A plugin that answered `Stop` is done with this event: its later hooks
    // are skipped, and every other plugin's still run. One plugin can never
    // switch off another's gates.
    let mut stopped: Vec<&str> = Vec::new();
    for entry in &dispatch_list {
        if stopped.contains(&entry.plugin_id.as_str()) {
            continue;
        }
        let generic_event = GenericEvent {
            topic: topic.to_string(),
            payload: event_payload.clone(),
        };

        match entry.mode {
            HookMode::Blocking => {
                // The deadline cancels the guest; a hook that could not
                // answer in time is "try again", never a decision.
                let action = match entry.hook.on_event(hook_call(class), &generic_event).await {
                    Ok(action) => action,
                    Err(e) if is_unavailable(&e) => {
                        tracing::warn!(topic, plugin_id = %entry.plugin_id, "Hook unavailable: {e}");
                        return Err(AppError::PluginUnavailable);
                    }
                    Err(e) => {
                        tracing::error!(topic, "Hook execution error: {e}");
                        return Err(AppError::Internal(format!("Hook execution failed: {e}")));
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
                    HookAction::Stop => stopped.push(&entry.plugin_id),
                    HookAction::Reject(reason) => {
                        let reject = parse_reject_detail(&reason);
                        if reject.code == PLUGIN_RUNTIME_ERROR_CODE {
                            tracing::error!(topic, code = %reject.code, message = %reject.message, "Hook infrastructure error");
                            return Err(AppError::Internal(reject.message));
                        }
                        return Ok(HookOutcome::Rejected(PluginRefusal::new(
                            &entry.plugin_id,
                            reject.status,
                            &reject.code,
                            reject.message,
                            reject.details,
                        )));
                    }
                    HookAction::Chain(events) => {
                        for chained in events {
                            let result = Box::pin(dispatch_hooks_inner(
                                &chained.topic,
                                chained.payload,
                                scope,
                                depth + 1,
                                unanswered,
                            ))
                            .await?;
                            if let HookOutcome::Rejected(_) = result {
                                return Ok(result);
                            }
                        }
                    }
                }
            }
            HookMode::Notify => match entry.hook.on_event(hook_call(class), &generic_event).await {
                Ok(action) => match &action {
                    HookAction::Pass => {}
                    HookAction::Reject(reason) => {
                        tracing::debug!(topic, "Notify hook returned Reject (ignored): {reason}",);
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
                    *unanswered |= is_unavailable(&e);
                    tracing::warn!(topic, "Notify hook error (ignored): {e}");
                }
            },
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
        type Context = CallOptions;

        fn id(&self) -> &str {
            &self.id
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: CallOptions,
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
        type Context = CallOptions;

        fn id(&self) -> &str {
            &self.id
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: CallOptions,
            _event: &GenericEvent,
        ) -> anyhow::Result<GenericHookAction> {
            Err(anyhow::anyhow!("WASM crash"))
        }
    }

    fn register_direct(
        registry: &mut ServerHookRegistry,
        hook: Arc<dyn GenericHook<Context = CallOptions>>,
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
                    function: "on_event".into(),
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
        dispatch_hooks(topic, payload, None, registry, CallClass::Background).await
    }

    /// A registry, as one replica holds it, with `hook` on `after_judging`.
    async fn replica_with(hook: Arc<dyn GenericHook<Context = CallOptions>>) -> SharedHookRegistry {
        let registry = new_shared_registry();
        register_direct(
            &mut *registry.write().await,
            hook,
            HookScope::Global,
            HookMode::Notify,
        );
        registry
    }

    async fn dispatch_once(db: &common::db::DatabaseConnection, registry: &SharedHookRegistry) {
        dispatch_hooks_once(
            db,
            "after_judging",
            serde_json::json!({}),
            None,
            registry,
            Some("after_judging:1:1".to_string()),
        )
        .await;
    }

    /// An event's hooks run once fleet-wide: the replica that dispatched it
    /// has run them when the dispatch returns, and neither another replica
    /// nor the same one after a restart (a fresh registry) runs them again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_fires_once_across_replicas_and_restarts() {
        let (_pg, db) = crate::test_support::postgres().await;
        let hooks: Vec<Arc<MockHook>> = (0..3)
            .map(|_| {
                Arc::new(MockHook::new(
                    "observer",
                    "after_judging",
                    GenericHookAction::Pass,
                ))
            })
            .collect();
        for hook in &hooks {
            let registry = replica_with(hook.clone()).await;
            dispatch_once(&db, &registry).await;
        }
        assert_eq!(
            hooks.iter().map(|h| h.calls()).collect::<Vec<_>>(),
            [1, 0, 0],
            "the first replica ran it before returning; no other replica or restart did"
        );
        // The claim the others read is the database's, not a process's.
        let claims = db
            .query_one_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT COUNT(*)::bigint AS n FROM hook_event WHERE event_id = 'after_judging:1:1'",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "n")
            .unwrap();
        assert_eq!(claims, 1);
    }

    /// Counts its calls; the first signals `started` and never completes.
    struct StallingHook {
        id: String,
        topics: Vec<String>,
        call_count: AtomicUsize,
        started: tokio::sync::Notify,
    }

    #[async_trait]
    impl GenericHook for StallingHook {
        type Context = CallOptions;

        fn id(&self) -> &str {
            &self.id
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: CallOptions,
            _event: &GenericEvent,
        ) -> anyhow::Result<GenericHookAction> {
            if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                self.started.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(HookAction::Pass)
        }
    }

    /// Two replicas racing on one event run its hooks once: while one
    /// replica's dispatch runs, the other's is skipped. A dispatch dropped
    /// before it completes gives the event back, so it is dispatched again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_racing_replica_is_skipped_and_a_dropped_dispatch_gives_the_event_back() {
        let (_pg, db) = crate::test_support::postgres().await;
        let stalling = Arc::new(StallingHook {
            id: "observer".into(),
            topics: vec!["after_judging".into()],
            call_count: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
        });
        let first = replica_with(stalling.clone()).await;
        let other = Arc::new(MockHook::new(
            "observer",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let second = replica_with(other.clone()).await;

        let running = tokio::spawn({
            let db = db.clone();
            async move { dispatch_once(&db, &first).await }
        });
        stalling.started.notified().await;
        dispatch_once(&db, &second).await;
        assert_eq!(other.calls(), 0, "skipped while the first replica runs it");

        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        wait_until_released(&db, "after_judging:1:1").await;
        dispatch_once(&db, &second).await;
        assert_eq!(other.calls(), 0, "a duplicate dispatch still skips it");
        redeliver_once(&db, &second).await.unwrap();
        assert_eq!(other.calls(), 1, "the dropped dispatch gave it back");
    }

    /// Returns once the claim on `event_id` is released (its lease given
    /// back, not completed). Polls the row, the state the re-scan reads.
    async fn wait_until_released(db: &common::db::DatabaseConnection, event_id: &str) {
        loop {
            let released = db
                .query_one_raw(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DbBackend::Postgres,
                    "SELECT (claimed_at IS NULL AND completed_at IS NULL) AS released \
                       FROM hook_event WHERE event_id = $1",
                    [event_id.into()],
                ))
                .await
                .unwrap()
                .is_some_and(|row| row.try_get::<bool>("", "released").unwrap());
            if released {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn execute(db: &common::db::DatabaseConnection, sql: &str) {
        db.execute_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap();
    }

    async fn rows(db: &common::db::DatabaseConnection) -> i64 {
        db.query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM hook_event",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap()
    }

    /// An event whose claim could not be recorded (the database refused the
    /// insert) is kept by the replica and recorded and dispatched by its next
    /// re-scan; once it ran, no re-scan runs it again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_whose_claim_failed_is_redelivered() {
        let (_pg, db) = crate::test_support::postgres().await;
        let hook = Arc::new(MockHook::new(
            "observer",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let registry = replica_with(hook.clone()).await;
        execute(&db, "ALTER TABLE hook_event RENAME TO hook_event_away").await;
        dispatch_once(&db, &registry).await;
        assert_eq!(hook.calls(), 0, "not dispatched without a claim");
        execute(&db, "ALTER TABLE hook_event_away RENAME TO hook_event").await;

        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(hook.calls(), 1, "the re-scan recorded and dispatched it");
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(hook.calls(), 1, "and never again");
    }

    /// Answers unavailable on its first call, then passes.
    struct FlakyHook {
        topics: Vec<String>,
        call_count: AtomicUsize,
    }

    #[async_trait]
    impl GenericHook for FlakyHook {
        type Context = CallOptions;

        fn id(&self) -> &str {
            "flaky"
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: CallOptions,
            _event: &GenericEvent,
        ) -> anyhow::Result<GenericHookAction> {
            if self.call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(PluginError::Unavailable {
                    plugin_id: "flaky".into(),
                    func_name: "on_event".into(),
                }
                .into());
            }
            Ok(HookAction::Pass)
        }
    }

    /// An event one of whose hooks could not answer (the plugin was busy,
    /// its SQL met an outage) is given back and run again by the next
    /// re-scan, notify hooks included; once every hook answered it is done.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_a_hook_could_not_answer_is_redelivered() {
        let (_pg, db) = crate::test_support::postgres().await;
        let flaky = Arc::new(FlakyHook {
            topics: vec!["after_judging".into()],
            call_count: AtomicUsize::new(0),
        });
        let registry = replica_with(flaky.clone()).await;
        dispatch_once(&db, &registry).await;
        assert_eq!(flaky.call_count.load(Ordering::SeqCst), 1);

        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(flaky.call_count.load(Ordering::SeqCst), 2, "run again");
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(flaky.call_count.load(Ordering::SeqCst), 2, "and then done");
    }

    /// A completed event never fires again, even once its claim is old; an
    /// event whose dispatch never finished (its replica died) is run again
    /// once its lease has lapsed, from what the row recorded.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_an_unfinished_event_is_redelivered_after_its_lease() {
        let (_pg, db) = crate::test_support::postgres().await;
        let hook = Arc::new(MockHook::new(
            "observer",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let registry = replica_with(hook.clone()).await;
        dispatch_once(&db, &registry).await;
        execute(
            &db,
            "INSERT INTO hook_event (event_id, topic, payload) \
             VALUES ('after_judging:2:1', 'after_judging', '{}')",
        )
        .await;
        execute(
            &db,
            "UPDATE hook_event SET claimed_at = claimed_at - interval '1 day'",
        )
        .await;
        assert_eq!(hook.calls(), 1);

        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(hook.calls(), 2, "only the unfinished event ran again");
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(hook.calls(), 2);
    }

    /// A completed event's row is deleted once it is older than the purge
    /// horizon; an unfinished one is kept, however old.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn completed_events_are_purged_past_the_horizon() {
        let (_pg, db) = crate::test_support::postgres().await;
        let registry = new_shared_registry();
        dispatch_once(&db, &registry).await;
        execute(
            &db,
            "INSERT INTO hook_event (event_id, topic, payload, claimed_at) \
             VALUES ('after_judging:2:1', 'after_judging', '{}', now())",
        )
        .await;
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(rows(&db).await, 2, "a fresh completed row is kept");

        execute(
            &db,
            "UPDATE hook_event SET created_at = created_at - interval '30 days', \
                    claimed_at = claimed_at - interval '30 days', \
                    completed_at = completed_at - interval '30 days'",
        )
        .await;
        // The unfinished row would be redelivered; keep it claimed so the
        // re-scan leaves it alone and only the purge acts.
        execute(
            &db,
            "UPDATE hook_event SET claimed_at = now() WHERE completed_at IS NULL",
        )
        .await;
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(rows(&db).await, 1, "the old completed row is purged");
    }

    /// A runtime shutting down with a dispatch in flight exits: dropping the
    /// dispatch gives its claim back without blocking on the database.
    #[test]
    fn a_dispatch_in_flight_does_not_hold_up_shutdown() {
        let outer = tokio::runtime::Runtime::new().unwrap();
        let (pg, _db, url) = outer.block_on(crate::test_support::postgres_with_url());

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let stalling = Arc::new(StallingHook {
            id: "observer".into(),
            topics: vec!["after_judging".into()],
            call_count: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
        });
        runtime.block_on(async {
            let db = crate::database::init_db(&url).await.unwrap();
            let registry = replica_with(stalling.clone()).await;
            tokio::spawn(async move { dispatch_once(&db, &registry).await });
            stalling.started.notified().await;
        });

        let (done, exited) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            drop(runtime);
            let _ = done.send(());
        });
        // A bound on a hang, not a wait for state: the drop either returns
        // or never does.
        exited
            .recv_timeout(std::time::Duration::from_secs(120))
            .expect("the runtime shut down with a dispatch in flight");
        outer.block_on(async move { drop(pg) });
    }

    #[tokio::test]
    async fn admission_effects_can_only_be_created_or_changed_by_the_contest_owner() {
        let event = broccoli_server_sdk::types::BeforeSubmissionEvent {
            attempt_id: "attempt-1".into(),
            attempt_timeout_ms: 60_000,
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
            (Some("owner"), "z-other", admitted.clone(), true),
            (Some("owner"), "z-other", payload.clone(), true),
            (
                Some("owner"),
                "z-other",
                serde_json::json!("invalid event"),
                false,
            ),
            (
                Some("owner"),
                "z-other",
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
            for plugin in ["owner", "z-other"] {
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
            .chain([(Some("owner"), "z-other", HookAction::Stop, true)]);
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
                "z-other",
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
        let problem_row = EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "main".to_string(),
            scope: ConfigScope::Problem,
            enabled: Some(true),
            position: 5,
        };
        let cp_row = EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "main".to_string(),
            scope: ConfigScope::ContestProblem,
            enabled: None,
            position: 10,
        };

        let forward = merge_resource_enablements([problem_row.clone(), cp_row.clone()]);
        let reverse = merge_resource_enablements([cp_row, problem_row]);

        assert_eq!(forward, reverse);
        assert_eq!(forward.get("cooldown"), Some(&10));
    }

    #[test]
    fn merge_resource_enablements_specific_disable_overrides_inherited_enable() {
        // An explicit `false` at the more specific scope must win over an
        // inherited `true`, in either arrival order.
        let problem_row = EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "main".to_string(),
            scope: ConfigScope::Problem,
            enabled: Some(true),
            position: 5,
        };
        let cp_row = EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "main".to_string(),
            scope: ConfigScope::ContestProblem,
            enabled: Some(false),
            position: 10,
        };

        let forward = merge_resource_enablements([problem_row.clone(), cp_row.clone()]);
        let reverse = merge_resource_enablements([cp_row, problem_row]);

        assert_eq!(forward, reverse);
        assert!(!forward.contains_key("cooldown"));
    }

    #[test]
    fn merge_resource_enablements_true_wins_within_a_scope() {
        // Two namespaces of the same plugin at the same scope: the plugin runs
        // when either namespace is on, and position is the min across the
        // scope's namespaces.
        let a = EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "a".to_string(),
            scope: ConfigScope::Problem,
            enabled: Some(false),
            position: 3,
        };
        let b = EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "b".to_string(),
            scope: ConfigScope::Problem,
            enabled: Some(true),
            position: 7,
        };

        let forward = merge_resource_enablements([a.clone(), b.clone()]);
        let reverse = merge_resource_enablements([b, a]);

        assert_eq!(forward, reverse);
        assert_eq!(forward.get("cooldown"), Some(&3));
    }

    /// The plugin-wide row decides a namespace no narrower row decides, as
    /// it does for the plugin's own config reads, and a narrower row that
    /// says decides over it.
    #[test]
    fn merge_resource_enablements_reads_the_plugin_wide_row_last() {
        let row = |scope, enabled| EnablementRow {
            plugin_id: "cooldown".to_string(),
            namespace: "main".to_string(),
            scope,
            enabled,
            position: 0,
        };
        let on = merge_resource_enablements([
            row(ConfigScope::Plugin, Some(true)),
            row(ConfigScope::Problem, None),
        ]);
        assert!(on.contains_key("cooldown"));
        let off = merge_resource_enablements([
            row(ConfigScope::Plugin, Some(true)),
            row(ConfigScope::Contest, Some(false)),
        ]);
        assert!(!off.contains_key("cooldown"));
    }

    #[tokio::test]
    async fn a_blocking_hooks_rejection_carries_its_code_under_the_plugin_id() {
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
        let HookOutcome::Rejected(refusal) = result else {
            panic!("Expected Rejected, got {result:?}");
        };
        assert_eq!(refusal.code(), "rejector.NO");
        assert_eq!(refusal.status().as_u16(), 429);
        assert_eq!(hook.calls(), 1);
    }

    /// A plugin's rejection never speaks as the host: a host code and a
    /// status the host owns ("sign in again", "busy, retrying") reach the
    /// client as the plugin's own code on a 400.
    /// Global hooks tie on everything but their plugin, so the plugin id
    /// orders them: the same two rejecting gates answer the same way however
    /// they were registered.
    #[tokio::test]
    async fn hooks_that_tie_run_in_plugin_id_order_whatever_the_registration_order() {
        for order in [["a-gate", "b-gate"], ["b-gate", "a-gate"]] {
            let registry = new_shared_registry();
            {
                let mut r = registry.write().await;
                for id in order {
                    let code = format!("{}_SAYS_NO", id.to_uppercase().replace('-', "_"));
                    let reject = serde_json::json!({"code": code, "status_code": 409});
                    register_direct(
                        &mut r,
                        Arc::new(MockHook::new(
                            id,
                            "test",
                            HookAction::Reject(reject.to_string()),
                        )),
                        HookScope::Global,
                        HookMode::Blocking,
                    );
                }
            }
            let result = dispatch_global(&registry, "test", serde_json::json!({}))
                .await
                .unwrap();
            let HookOutcome::Rejected(refusal) = result else {
                panic!("Expected Rejected, got {result:?}");
            };
            assert_eq!(refusal.code(), "a-gate.A_GATE_SAYS_NO", "{order:?}");
        }
    }

    #[tokio::test]
    async fn a_rejection_naming_a_host_condition_is_the_plugins_own() {
        let registry = new_shared_registry();
        for (code, status) in [("TOKEN_INVALID", 401), ("PLUGIN_UNAVAILABLE", 503)] {
            let hook = Arc::new(MockHook::new(
                "spoofer",
                "test",
                HookAction::Reject(
                    serde_json::json!({"code": code, "message": "x", "status_code": status})
                        .to_string(),
                ),
            ));
            let mut r = registry.write().await;
            r.unregister_plugin("spoofer");
            register_direct(&mut r, hook, HookScope::Global, HookMode::Blocking);
            drop(r);
            let result = dispatch_global(&registry, "test", serde_json::json!({}))
                .await
                .unwrap();
            let HookOutcome::Rejected(refusal) = result else {
                panic!("Expected Rejected, got {result:?}");
            };
            assert_eq!(refusal.code(), format!("spoofer.{code}"));
            assert_eq!(refusal.status().as_u16(), 400);
        }
    }

    /// `Stop` ends only the stopping plugin's part of the dispatch: its own
    /// later hooks are skipped, and every other plugin's gates still run, so
    /// one plugin can never switch off another's.
    #[tokio::test]
    async fn a_stop_skips_only_the_stopping_plugins_later_hooks() {
        let registry = new_shared_registry();
        let stop = Arc::new(MockHook::new("a-stopper", "test", HookAction::Stop));
        let own_later = Arc::new(MockHook::new(
            "a-stopper",
            "test",
            HookAction::Reject("OWN".into()),
        ));
        let gate = Arc::new(MockHook::new(
            "b-gate",
            "test",
            HookAction::Reject(r#"{"code":"GATE","status_code":429}"#.into()),
        ));
        {
            let mut r = registry.write().await;
            for hook in [stop.clone(), own_later.clone(), gate.clone()] {
                register_direct(&mut r, hook, HookScope::Global, HookMode::Blocking);
            }
        }

        let result = dispatch_global(&registry, "test", serde_json::json!({}))
            .await
            .unwrap();
        let HookOutcome::Rejected(refusal) = result else {
            panic!("Expected Rejected, got {result:?}");
        };
        assert_eq!(refusal.code(), "b-gate.GATE");
        assert_eq!(stop.calls(), 1);
        assert_eq!(own_later.calls(), 0);
        assert_eq!(gate.calls(), 1);
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
        let blocking = Arc::new(MockHook::new(
            "blocker",
            "test",
            HookAction::Reject("RAN".into()),
        ));
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
        assert!(matches!(result, HookOutcome::Rejected(_)));
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
        let hook = Arc::new(MockHook::new(
            "plugin_a",
            "test",
            HookAction::Reject("RAN".into()),
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
        assert!(matches!(result, HookOutcome::Rejected(_)));

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
            HookOutcome::Rejected(refusal) => {
                let (_, body) = refusal.into_parts();
                assert_eq!(body.code, "limiter.LIMIT");
                let details = body.details.expect("details should be Some");
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
            HookOutcome::Rejected(refusal) => {
                let (_, body) = refusal.into_parts();
                assert_eq!(body.code, "rejector.NO");
                assert!(body.details.is_none(), "details should be None when absent");
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
            HookOutcome::Rejected(refusal) => {
                let (_, body) = refusal.into_parts();
                assert_eq!(body.code, "rejector.PLUGIN_REJECTED");
                assert_eq!(body.message, "plain text rejection");
                assert!(body.details.is_none());
            }
            other => panic!("Expected Rejected, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn resource_scoped_hook_fires_when_plugin_is_in_enablements() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "cooldown",
            "test",
            HookAction::Reject("RAN".into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Resource,
            HookMode::Blocking,
        );

        let mut enabled = HashMap::new();
        enabled.insert("cooldown".to_string(), 0);
        let result = dispatch_hooks(
            "test",
            serde_json::json!({}),
            Some(&enabled),
            &registry,
            CallClass::Background,
        )
        .await
        .unwrap();
        assert!(matches!(result, HookOutcome::Rejected(_)));
        assert_eq!(hook.calls(), 1);
    }

    #[tokio::test]
    async fn resource_scoped_hook_skipped_when_no_enablements() {
        let registry = new_shared_registry();
        let hook = Arc::new(MockHook::new(
            "cooldown",
            "test",
            HookAction::Reject("RAN".into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Resource,
            HookMode::Blocking,
        );

        let result = dispatch_hooks(
            "test",
            serde_json::json!({}),
            None,
            &registry,
            CallClass::Background,
        )
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
        let hook = Arc::new(MockHook::new(
            "cooldown",
            "test",
            HookAction::Reject("RAN".into()),
        ));
        register_direct(
            &mut *registry.write().await,
            hook.clone(),
            HookScope::Resource,
            HookMode::Blocking,
        );

        let mut enabled = HashMap::new();
        enabled.insert("other-plugin".to_string(), 0);
        let result = dispatch_hooks(
            "test",
            serde_json::json!({}),
            Some(&enabled),
            &registry,
            CallClass::Background,
        )
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

    /// A hook whose plugin cannot answer before its deadline.
    struct BusyHook {
        topics: Vec<String>,
    }

    #[async_trait]
    impl GenericHook for BusyHook {
        type Context = CallOptions;

        fn id(&self) -> &str {
            "busy"
        }

        fn topics(&self) -> &[String] {
            &self.topics
        }

        async fn on_event(
            &self,
            _ctx: CallOptions,
            _event: &GenericEvent,
        ) -> anyhow::Result<GenericHookAction> {
            Err(PluginError::Unavailable {
                plugin_id: "busy".into(),
                func_name: "gate".into(),
            }
            .into())
        }
    }

    #[tokio::test]
    async fn an_unavailable_blocking_hook_is_a_503_not_a_rejection() {
        let registry = new_shared_registry();
        registry.write().await.hooks.insert(
            "test".into(),
            vec![HookEntry {
                plugin_id: "busy".into(),
                function: "on_event".into(),
                scope: HookScope::Global,
                mode: HookMode::Blocking,
                hook: Arc::new(BusyHook {
                    topics: vec!["test".into()],
                }),
            }],
        );
        let result = dispatch_hooks(
            "test",
            serde_json::json!({}),
            None,
            &registry,
            CallClass::Interactive,
        )
        .await;
        assert!(
            matches!(result, Err(AppError::PluginUnavailable)),
            "{result:?}"
        );
    }

    #[tokio::test]
    async fn an_unavailable_notify_hook_is_skipped() {
        let registry = new_shared_registry();
        registry.write().await.hooks.insert(
            "test".into(),
            vec![HookEntry {
                plugin_id: "busy".into(),
                function: "on_event".into(),
                scope: HookScope::Global,
                mode: HookMode::Notify,
                hook: Arc::new(BusyHook {
                    topics: vec!["test".into()],
                }),
            }],
        );
        let result = dispatch_hooks(
            "test",
            serde_json::json!({}),
            None,
            &registry,
            CallClass::Background,
        )
        .await;
        assert!(matches!(result, Ok(HookOutcome::Allowed(_))), "{result:?}");
    }
}
