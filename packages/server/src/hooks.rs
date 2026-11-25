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
/// take it elsewhere: longer than a dispatch's hooks run, so a live dispatch
/// keeps it, and short enough that the event of a replica that died runs
/// again soon.
const EVENT_LEASE_SECS: i64 = 600;

/// How long a settled event's row (completed or dead-lettered) is kept: a
/// duplicate dispatch of it within this is skipped.
const SETTLED_KEPT_SECS: i64 = 7 * 24 * 3600;

/// How many events one re-scan claims.
const REDELIVERY_BATCH: i64 = 16;

/// How often a replica re-scans for events to run again.
pub const REDELIVERY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// How many events whose claim could not be recorded a replica keeps for
/// its next re-scan; past this the oldest are dropped, loudly.
const UNRECORDED_MAX: usize = 10_000;

/// The wait before the second attempt of an event its first left
/// unfinished; each later wait is twice the one before, up to
/// [`BACKOFF_MAX_SECS`].
const BACKOFF_BASE_SECS: i64 = 10;

/// The longest wait between two attempts of an event.
const BACKOFF_MAX_SECS: i64 = 3600;

/// How many attempts an event has to reach every hook plugin it is for; one
/// that has not after this many is dead-lettered.
pub const MAX_EVENT_ATTEMPTS: i32 = 12;

/// The wait before the next attempt of an event whose `attempts` attempts
/// left it unfinished.
pub(crate) fn backoff_secs(attempts: i32) -> i64 {
    common::retry::doubling_backoff(
        attempts.max(0) as u32,
        std::time::Duration::from_secs(BACKOFF_BASE_SECS as u64),
        std::time::Duration::from_secs(BACKOFF_MAX_SECS as u64),
    )
    .as_secs() as i64
}

/// A background event this replica could not record (the database refused
/// the claim), kept for its next re-scan to record and run.
struct Unrecorded {
    event_id: String,
    topic: String,
    payload: serde_json::Value,
    enabled_plugins: Option<ResourceEnablements>,
}

/// The dead letter message id of hook event `event_id`.
pub(crate) fn dlq_message_id(event_id: &str) -> String {
    format!("hook_event:{event_id}")
}

/// The hook event a dead letter message id names, if it names one.
pub(crate) fn event_of_dlq_message(message_id: &str) -> Option<&str> {
    message_id.strip_prefix("hook_event:")
}

/// Gives dead-lettered hook event `event_id` a fresh run: its attempts
/// start over and it is due now, so the re-scan delivers it to the hook
/// plugins still owed it (the delivered ones are not called again). Its dead
/// letter stays open until the event completes. `false` when the event is
/// not dead (gone, completed, or revived already).
pub(crate) async fn revive_dead_event<C: sea_orm::ConnectionTrait>(
    conn: &C,
    event_id: &str,
) -> Result<bool, sea_orm::DbErr> {
    Ok(conn
        .execute_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "UPDATE hook_event SET dead_at = NULL, attempts = 0, next_attempt_at = now(), \
                    claimed_at = NULL, claim_token = NULL \
              WHERE event_id = $1 AND dead_at IS NOT NULL AND completed_at IS NULL",
            [event_id.into()],
        ))
        .await?
        .rows_affected()
        == 1)
}

/// One dispatch's lease on a background event, fleet-wide: its row of
/// `hook_event`, which records the event (topic, payload, enablements)
/// before its hooks run, and a token that fences every write the dispatch
/// makes to it. Delivery is at least once per hook plugin, and once per
/// plugin while every write lands:
///
/// - a duplicate dispatch of the event, on any replica and after a restart
///   too, is skipped while the row exists;
/// - each hook plugin whose hooks answered is recorded delivered
///   (`hook_event_delivery`), renewing the lease, and a later attempt calls
///   only the plugins not recorded;
/// - once every hook plugin the event is for has it delivered, the event is
///   complete; otherwise (a plugin could not answer, or is not loaded here)
///   its lease is given back and its next attempt backed off
///   ([`backoff_secs`]), until [`MAX_EVENT_ATTEMPTS`] dead-letters it;
/// - a write under a lease another dispatch has since taken lands nowhere;
/// - a dispatch dropped before it ends gives its lease back.
///
/// Plugins take a hook event as idempotent.
struct EventClaim {
    db: common::db::DatabaseConnection,
    id: String,
    topic: String,
    token: uuid::Uuid,
    /// The attempt this lease is.
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
        let token = uuid::Uuid::new_v4();
        let inserted = db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "INSERT INTO hook_event (event_id, topic, payload, enabled_plugins, claimed_at, \
                                         claim_token, attempts) \
                 VALUES ($1, $2, $3, $4, now(), $5, 1) ON CONFLICT DO NOTHING",
                [
                    id.into(),
                    topic.into(),
                    payload.clone().into(),
                    enabled.into(),
                    token.into(),
                ],
            ))
            .await?
            .rows_affected();
        Ok((inserted == 1).then(|| Self {
            db: db.clone(),
            id: id.to_string(),
            topic: topic.to_string(),
            token,
            attempt: 1,
            settled: false,
        }))
    }

    /// The hook plugins that have the event delivered.
    async fn delivered(&self) -> Result<Vec<String>, sea_orm::DbErr> {
        let rows = self
            .db
            .query_all_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT plugin_id FROM hook_event_delivery \
                  WHERE event_id = $1 AND delivered_at IS NOT NULL",
                [self.id.clone().into()],
            ))
            .await?;
        rows.iter()
            .map(|row| row.try_get::<String>("", "plugin_id"))
            .collect()
    }

    /// Records that `plugin_id`'s hooks answered the event, renewing the
    /// lease. `false` when the lease is no longer this dispatch's (it
    /// lapsed and another took it): nothing is recorded, and the dispatch
    /// stops.
    async fn mark_delivered(&self, plugin_id: &str) -> Result<bool, sea_orm::DbErr> {
        let row = self
            .db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "WITH held AS (UPDATE hook_event SET claimed_at = now() \
                                WHERE event_id = $1 AND claim_token = $2 \
                                  AND completed_at IS NULL AND dead_at IS NULL \
                               RETURNING event_id), \
                      marked AS (INSERT INTO hook_event_delivery \
                                        (event_id, plugin_id, delivered_at) \
                                 SELECT event_id, $3, now() FROM held \
                                 ON CONFLICT (event_id, plugin_id) DO UPDATE \
                                   SET delivered_at = COALESCE(hook_event_delivery.delivered_at, \
                                                               now())) \
                 SELECT COUNT(*)::bigint AS held FROM held",
                [self.id.clone().into(), self.token.into(), plugin_id.into()],
            ))
            .await?;
        Ok(row
            .map(|r| r.try_get::<i64>("", "held"))
            .transpose()?
            .unwrap_or(0)
            == 1)
    }

    /// Records that `plugin_id`'s hooks failed the event, for `why`: it
    /// stays owed the event, and its last error is kept for the record (the
    /// dead letter names it). `false` when the lease is no longer this
    /// dispatch's, as for [`Self::mark_delivered`].
    async fn mark_failed(&self, plugin_id: &str, why: &str) -> Result<bool, sea_orm::DbErr> {
        let row = self
            .db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "WITH held AS (UPDATE hook_event SET claimed_at = now() \
                                WHERE event_id = $1 AND claim_token = $2 \
                                  AND completed_at IS NULL AND dead_at IS NULL \
                               RETURNING event_id), \
                      marked AS (INSERT INTO hook_event_delivery \
                                        (event_id, plugin_id, last_error, failed_at) \
                                 SELECT event_id, $3, $4, now() FROM held \
                                 ON CONFLICT (event_id, plugin_id) DO UPDATE \
                                   SET last_error = EXCLUDED.last_error, \
                                       failed_at = EXCLUDED.failed_at \
                                 WHERE hook_event_delivery.delivered_at IS NULL) \
                 SELECT COUNT(*)::bigint AS held FROM held",
                [
                    self.id.clone().into(),
                    self.token.into(),
                    plugin_id.into(),
                    why.into(),
                ],
            ))
            .await?;
        Ok(row
            .map(|r| r.try_get::<i64>("", "held"))
            .transpose()?
            .unwrap_or(0)
            == 1)
    }

    /// Ends a lease another dispatch has since taken: nothing is written,
    /// since the event is that dispatch's now.
    async fn lost(mut self) {
        self.settled = true;
    }

    /// Ends the lease with the event complete: every hook plugin it is for
    /// has it delivered. An event an operator revived from the dead letter
    /// queue resolves its dead letter as it completes.
    async fn complete(mut self) {
        self.settled = true;
        let completed = self
            .db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "WITH done AS (UPDATE hook_event SET completed_at = now(), claimed_at = NULL \
                                WHERE event_id = $1 AND claim_token = $2 \
                                  AND completed_at IS NULL \
                               RETURNING event_id) \
                 UPDATE dead_letter_message SET resolved = TRUE, resolved_at = now() \
                  WHERE message_id = $3 AND NOT resolved AND EXISTS (SELECT 1 FROM done)",
                [
                    self.id.clone().into(),
                    self.token.into(),
                    dlq_message_id(&self.id).into(),
                ],
            ))
            .await;
        if let Err(e) = completed {
            tracing::warn!(event_id = %self.id, error = %e, "Completing a hook event failed; its delivered plugins are not called again");
        }
    }

    /// Ends the lease with the event unfinished (`why`): its next attempt
    /// is backed off, or, after [`MAX_EVENT_ATTEMPTS`], the event is
    /// dead-lettered, loudly and in the dead letter queue.
    async fn retry_later(mut self, why: String) {
        self.settled = true;
        if self.attempt >= MAX_EVENT_ATTEMPTS {
            self.dead_letter(why).await;
            return;
        }
        let released = self
            .db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "UPDATE hook_event SET claimed_at = NULL, last_error = $3, \
                        next_attempt_at = now() + make_interval(secs => $4), \
                        history = history || jsonb_build_array(jsonb_build_object( \
                            'attempt', attempts, 'error', $3::text, 'timestamp', now())) \
                  WHERE event_id = $1 AND claim_token = $2 AND completed_at IS NULL",
                [
                    self.id.clone().into(),
                    self.token.into(),
                    why.clone().into(),
                    (backoff_secs(self.attempt) as f64).into(),
                ],
            ))
            .await;
        match released {
            Ok(_) => tracing::info!(
                event_id = %self.id,
                attempt = self.attempt,
                reason = %why,
                "A hook event did not reach every hook plugin; it is retried later"
            ),
            Err(e) => {
                tracing::warn!(event_id = %self.id, error = %e, "Giving back a hook event lease failed; it lapses instead")
            }
        }
    }

    /// Marks the event dead and files it in the dead letter queue, as one
    /// transaction: a dead event is always one an administrator sees.
    async fn dead_letter(&self, why: String) {
        match self.try_dead_letter(&why).await {
            Ok(false) => {}
            Ok(true) => tracing::error!(
                event_id = %self.id,
                topic = %self.topic,
                attempts = self.attempt,
                reason = %why,
                "A hook event did not reach every hook plugin after its last attempt; it is dead-lettered"
            ),
            Err(e) => {
                tracing::warn!(event_id = %self.id, error = %e, "Dead-lettering a hook event failed; its lease lapses and it is tried again")
            }
        }
    }

    /// Whether this lease dead-lettered the event (it was still this
    /// dispatch's).
    async fn try_dead_letter(&self, why: &str) -> Result<bool, sea_orm::DbErr> {
        use sea_orm::TransactionTrait;
        let txn = self.db.begin().await?;
        let dead = txn
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "UPDATE hook_event SET claimed_at = NULL, last_error = $3, dead_at = now(), \
                        history = history || jsonb_build_array(jsonb_build_object( \
                            'attempt', attempts, 'error', $3::text, 'timestamp', now())) \
                  WHERE event_id = $1 AND claim_token = $2 AND completed_at IS NULL \
                    AND dead_at IS NULL \
                 RETURNING payload, history, \
                   (SELECT COALESCE(jsonb_object_agg(d.plugin_id, d.last_error), '{}'::jsonb) \
                      FROM hook_event_delivery d \
                     WHERE d.event_id = $1 AND d.delivered_at IS NULL \
                       AND d.last_error IS NOT NULL) AS failures",
                [self.id.clone().into(), self.token.into(), why.into()],
            ))
            .await?;
        let Some(row) = dead else {
            return Ok(false);
        };
        let payload: serde_json::Value = row.try_get("", "payload")?;
        let history: serde_json::Value = row.try_get("", "history")?;
        let failures: serde_json::Value = row.try_get("", "failures")?;
        let retry_history: Vec<common::retry::RetryAttempt> = serde_json::from_value(history)
            .map_err(|e| sea_orm::DbErr::Custom(format!("a hook event's history: {e}")))?;
        let envelope = common::dlq::DlqEnvelope {
            message_id: dlq_message_id(&self.id),
            message_type: common::DlqMessageType::HookEvent,
            submission_id: None,
            payload: serde_json::json!({
                "event_id": self.id,
                "topic": self.topic,
                "payload": payload,
                "attempts": self.attempt,
                "failures": failures,
            }),
            error_code: common::dlq::DlqErrorCode::MaxRetriesExceeded,
            error_message: why.to_string(),
            retry_history,
        };
        crate::dlq::DlqService::new(&txn)
            .send_to_dlq(&envelope)
            .await?;
        txn.commit().await?;
        Ok(true)
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
        let token = self.token;
        drop(runtime.spawn(async move {
            let released = db
                .execute_raw(sea_orm::Statement::from_sql_and_values(
                    sea_orm::DbBackend::Postgres,
                    "UPDATE hook_event SET claimed_at = NULL \
                      WHERE event_id = $1 AND claim_token = $2 AND completed_at IS NULL",
                    [id.clone().into(), token.into()],
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
    /// What the fleet has enabled, and which hooks each plugin declares:
    /// a background event is complete only once every hook plugin it is
    /// for, loaded here or not, has it delivered.
    lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>,
}

impl Default for ServerHookRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerHookRegistry {
    pub fn new() -> Self {
        Self::for_fleet(Default::default())
    }

    /// A registry whose background events are owed to the hook plugins
    /// `lifecycle` records as enabled.
    pub fn for_fleet(lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>) -> Self {
        Self {
            hooks: HashMap::new(),
            unrecorded: Default::default(),
            lifecycle,
        }
    }

    /// The hook plugins a `topic` event with `enabled_plugins` is for: the
    /// ones enabled fleet-wide that declare a hook it runs, and the ones
    /// registered here that it runs.
    fn owed(
        &self,
        topic: &str,
        enabled_plugins: Option<&ResourceEnablements>,
    ) -> std::collections::BTreeSet<String> {
        let mut owed = self.lifecycle.hook_plugins(topic, enabled_plugins);
        owed.extend(self.run_here(topic, enabled_plugins));
        owed
    }

    /// The plugins registered here whose hooks a `topic` event with
    /// `enabled_plugins` runs.
    fn run_here(
        &self,
        topic: &str,
        enabled_plugins: Option<&ResourceEnablements>,
    ) -> std::collections::BTreeSet<String> {
        self.hooks
            .get(topic)
            .into_iter()
            .flatten()
            .filter(|entry| match entry.scope {
                HookScope::Global => true,
                HookScope::Resource => {
                    enabled_plugins.is_some_and(|e| e.contains_key(&entry.plugin_id))
                }
            })
            .map(|entry| entry.plugin_id.clone())
            .collect()
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

/// A shared registry for a server whose fleet `lifecycle` follows.
pub fn shared_registry_for(
    lifecycle: Arc<crate::plugin_lifecycle::PluginLifecycle>,
) -> SharedHookRegistry {
    Arc::new(RwLock::new(ServerHookRegistry::for_fleet(lifecycle)))
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
            only: None,
        },
        0,
        &mut Answers::default(),
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
            only: None,
        },
        0,
        &mut Answers::default(),
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

/// Runs an event's hooks under `claim`, each hook plugin it is owed to on
/// its own, in plugin id order: a plugin's hooks run only if it does not
/// have the event yet, and its failure (an error, a trap, no answer) holds
/// back no other plugin, since a background event's rejections and stops
/// change nothing. Each plugin whose hooks answered is recorded delivered,
/// each that failed keeps its error and stays owed. The event completes once
/// every hook plugin it is owed to has it; one that failed, or is not loaded
/// here, leaves it for a later attempt.
async fn deliver(
    mut claim: Option<EventClaim>,
    topic: &str,
    event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
) {
    let delivered: std::collections::BTreeSet<String> = match &claim {
        Some(held) => match held.delivered().await {
            Ok(delivered) => delivered.into_iter().collect(),
            Err(e) => {
                tracing::warn!(event_id = %held.id, error = %e, "Reading a hook event's deliveries failed");
                if let Some(claim) = claim {
                    claim
                        .retry_later(format!("reading its deliveries failed: {e}"))
                        .await;
                }
                return;
            }
        },
        None => Default::default(),
    };
    let here = hook_registry.read().await.run_here(topic, enabled_plugins);
    let mut failures: std::collections::BTreeMap<String, String> = Default::default();
    for plugin_id in here.iter().filter(|id| !delivered.contains(*id)) {
        let failure = deliver_to(
            topic,
            event_payload.clone(),
            enabled_plugins,
            hook_registry,
            plugin_id,
        )
        .await;
        let Some(held) = &claim else {
            continue;
        };
        let recorded = match &failure {
            None => held.mark_delivered(plugin_id).await,
            Some(why) => held.mark_failed(plugin_id, why).await,
        };
        match recorded {
            Ok(true) => {}
            Ok(false) => {
                if let Some(claim) = claim.take() {
                    tracing::info!(event_id = %claim.id, "A hook event's lease passed to another dispatch; this one stops");
                    claim.lost().await;
                }
                return;
            }
            Err(e) => {
                if let Some(claim) = claim.take() {
                    tracing::warn!(event_id = %claim.id, plugin_id, error = %e, "Recording a hook event's delivery failed");
                    claim
                        .retry_later(format!("recording its delivery to {plugin_id} failed: {e}"))
                        .await;
                }
                return;
            }
        }
        if let Some(why) = failure {
            failures.insert(plugin_id.clone(), why);
        }
    }
    let Some(claim) = claim else {
        return;
    };
    let owed = hook_registry.read().await.owed(topic, enabled_plugins);
    let pending: Vec<String> = owed
        .iter()
        .filter(|id| {
            !delivered.contains(*id) && !(here.contains(*id) && !failures.contains_key(*id))
        })
        .map(|id| match failures.get(id) {
            Some(why) => format!("{id} ({why})"),
            None => format!("{id} (not loaded on this replica)"),
        })
        .collect();
    if pending.is_empty() {
        claim.complete().await;
    } else {
        claim
            .retry_later(format!("not yet delivered to {}", pending.join(", ")))
            .await;
    }
}

/// Runs `plugin_id`'s hooks on a background event; why they failed, or
/// `None` when every one of them answered.
async fn deliver_to(
    topic: &str,
    event_payload: serde_json::Value,
    enabled_plugins: Option<&ResourceEnablements>,
    hook_registry: &SharedHookRegistry,
    plugin_id: &str,
) -> Option<String> {
    let mut answers = Answers::default();
    let dispatched = dispatch_hooks_inner(
        topic,
        event_payload,
        DispatchScope {
            enabled_plugins,
            hook_registry,
            admission_owner: None,
            class: CallClass::Background,
            only: Some(plugin_id),
        },
        0,
        &mut answers,
    )
    .await;
    // The hook the dispatch stopped at answered unless it failed.
    answers.settle();
    match dispatched {
        Ok(HookOutcome::Rejected(refusal)) => tracing::debug!(
            topic,
            plugin_id,
            code = refusal.code(),
            "Background hook returned Reject (ignored)"
        ),
        Err(e) => tracing::warn!(topic, plugin_id, "Background hook dispatch failed: {e:?}"),
        Ok(HookOutcome::Allowed(_)) => {}
    }
    if answers.answered().contains(plugin_id) {
        None
    } else {
        Some(
            answers
                .failure
                .unwrap_or_else(|| "its hooks did not all answer".to_string()),
        )
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
        "DELETE FROM hook_event \
          WHERE COALESCE(completed_at, dead_at) < now() - make_interval(secs => $1)",
        [(SETTLED_KEPT_SECS as f64).into()],
    ))
    .await?;

    // Due by `next_attempt_at`, earliest first: an event that keeps failing
    // waits longer each time, so it never holds back the events behind it.
    let due = db
        .query_all_raw(sea_orm::Statement::from_sql_and_values(
            sea_orm::DbBackend::Postgres,
            "UPDATE hook_event h SET claimed_at = now(), attempts = h.attempts + 1, \
                                     claim_token = gen_random_uuid() \
               FROM (SELECT event_id FROM hook_event \
                      WHERE completed_at IS NULL AND dead_at IS NULL \
                        AND next_attempt_at <= now() \
                        AND (claimed_at IS NULL \
                             OR claimed_at < now() - make_interval(secs => $1)) \
                      ORDER BY next_attempt_at, created_at LIMIT $2 \
                      FOR UPDATE SKIP LOCKED) due \
              WHERE h.event_id = due.event_id \
             RETURNING h.event_id, h.topic, h.payload, h.enabled_plugins, h.attempts, \
                       h.claim_token",
            [(EVENT_LEASE_SECS as f64).into(), REDELIVERY_BATCH.into()],
        ))
        .await?;
    for row in due {
        let id: String = row.try_get("", "event_id")?;
        let topic: String = row.try_get("", "topic")?;
        let payload: serde_json::Value = row.try_get("", "payload")?;
        let enabled: Option<serde_json::Value> = row.try_get("", "enabled_plugins")?;
        let attempt: i32 = row.try_get("", "attempts")?;
        let token: uuid::Uuid = row.try_get("", "claim_token")?;
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
            topic: topic.clone(),
            token,
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
    /// The one plugin whose hooks on the dispatched event run (not its
    /// chained events'): a background event is delivered to each plugin it
    /// is owed to on its own, so one plugin's failure never holds back
    /// another's delivery.
    only: Option<&'a str>,
}

/// Which plugins' hooks answered one dispatch. A plugin answered when every
/// one of its hooks on the dispatched event ran and none, nor any hook of an
/// event it chained, failed (could not answer, or answered with an error, a
/// notify hook's too); a plugin whose hook the dispatch stopped before (a
/// rejection or an error ended it) did not. The first failure's reason is
/// kept, for the record of why the plugin does not have the event.
#[derive(Default)]
struct Answers {
    /// The dispatched event's hooks per plugin.
    hooks: HashMap<String, usize>,
    /// The hooks per plugin that answered.
    answered: HashMap<String, usize>,
    /// The plugin of the dispatched event's hook running now.
    running: Option<String>,
    /// Whether a hook failed since `running` started.
    running_unanswered: bool,
    /// Why the first hook that failed did.
    failure: Option<String>,
}

impl Answers {
    /// A hook (the running one, or one of an event it chained) failed, for
    /// `why`, which may be the plugin's own text: it is kept as a guest line
    /// is (`plugin_core::guest_log::plugin_text`), escaped and capped.
    fn failed(&mut self, why: impl std::fmt::Display) {
        self.running_unanswered = true;
        if self.failure.is_none() {
            self.failure = Some(plugin_core::guest_log::plugin_text(&why.to_string()).into_owned());
        }
    }

    /// The dispatched event's next hook, of `plugin_id`, starts: the one
    /// before it ends.
    fn start(&mut self, plugin_id: &str) {
        self.settle();
        self.running = Some(plugin_id.to_string());
    }

    /// Ends the running hook: it answered unless it could not.
    fn settle(&mut self) {
        if let Some(plugin_id) = self.running.take()
            && !std::mem::take(&mut self.running_unanswered)
        {
            *self.answered.entry(plugin_id).or_default() += 1;
        }
    }

    /// The plugins every hook of which answered.
    fn answered(&self) -> std::collections::BTreeSet<String> {
        self.hooks
            .iter()
            .filter(|(id, hooks)| self.answered.get(id.as_str()) == Some(hooks))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// Runs `topic`'s hooks, recording in `answers` which plugins' hooks
/// answered (a notify hook that could not answer counts too, though the
/// dispatch goes on without it).
async fn dispatch_hooks_inner(
    topic: &str,
    mut event_payload: serde_json::Value,
    scope: DispatchScope<'_>,
    depth: u8,
    answers: &mut Answers,
) -> Result<HookOutcome, AppError> {
    let DispatchScope {
        enabled_plugins,
        hook_registry,
        admission_owner,
        class,
        only,
    } = scope;
    let only = only.filter(|_| depth == 0);
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

        if let Some(only) = only {
            list.retain(|entry| entry.plugin_id == only);
        }
        list
    };
    if depth == 0 {
        for entry in &dispatch_list {
            *answers.hooks.entry(entry.plugin_id.clone()).or_default() += 1;
        }
    }

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
        if depth == 0 {
            answers.start(&entry.plugin_id);
        }
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
                        answers.failed(&e);
                        // What stopped it (the plugin, or the database it
                        // needed) keeps its own code and Retry-After.
                        return Err(match e.downcast::<PluginError>() {
                            // Between loads: the plugin is busy, not gone.
                            Ok(PluginError::NotLoaded(_)) | Err(_) => AppError::PluginUnavailable,
                            Ok(plugin) => AppError::from(plugin),
                        });
                    }
                    Err(e) => {
                        answers.failed(format!("{e:#}"));
                        return Err(AppError::PluginFailed {
                            plugin_id: entry.plugin_id.clone(),
                            what: "A hook failed",
                            text: format!("{topic}: {e:#}"),
                        });
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
                            answers.failed(&reject.message);
                            return Err(AppError::PluginFailed {
                                plugin_id: entry.plugin_id.clone(),
                                what: "A hook's runtime failed",
                                text: format!("{topic}: {}", reject.message),
                            });
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
                                answers,
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
                    // The dispatch goes on without it; a background event
                    // keeps the plugin owed and runs it again.
                    answers.failed(&e);
                    plugin_core::guest_log::about(
                        &entry.plugin_id,
                        "A notify hook failed",
                        &format!("{topic}: {e:#}"),
                    );
                }
            },
        }
    }

    if depth == 0 {
        answers.settle();
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
        assert_eq!(
            flaky.call_count.load(Ordering::SeqCst),
            1,
            "not before its backoff"
        );
        due_now(&db).await;
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

    /// Makes every unfinished event's next attempt due now, as if its
    /// backoff had passed.
    async fn due_now(db: &common::db::DatabaseConnection) {
        execute(db, "UPDATE hook_event SET next_attempt_at = now()").await;
    }

    async fn scalar<T: sea_orm::TryGetable>(db: &common::db::DatabaseConnection, sql: &str) -> T {
        db.query_one_raw(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            sql.to_string(),
        ))
        .await
        .unwrap()
        .expect("a row")
        .try_get::<T>("", "v")
        .unwrap()
    }

    /// Answers unavailable on every call.
    struct DownHook {
        id: String,
        topics: Vec<String>,
        call_count: AtomicUsize,
    }

    impl DownHook {
        fn new(id: &str) -> Self {
            Self {
                id: id.into(),
                topics: vec!["after_judging".into()],
                call_count: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl GenericHook for DownHook {
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
            Err(PluginError::Unavailable {
                plugin_id: self.id.clone(),
                func_name: "on_event".into(),
            }
            .into())
        }
    }

    /// A re-run of an event calls only the hook plugins that do not have it
    /// delivered: a plugin whose hooks answered is never called again for
    /// it, whatever another plugin did.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rerun_calls_only_the_plugins_not_yet_delivered() {
        let (_pg, db) = crate::test_support::postgres().await;
        let answered = Arc::new(MockHook::new(
            "observer",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let flaky = Arc::new(FlakyHook {
            topics: vec!["after_judging".into()],
            call_count: AtomicUsize::new(0),
        });
        let registry = replica_with(answered.clone()).await;
        register_direct(
            &mut *registry.write().await,
            flaky.clone(),
            HookScope::Global,
            HookMode::Notify,
        );
        dispatch_once(&db, &registry).await;
        assert_eq!(
            (answered.calls(), flaky.call_count.load(Ordering::SeqCst)),
            (1, 1)
        );
        let delivered: String = scalar(
            &db,
            "SELECT string_agg(plugin_id, ',' ORDER BY plugin_id) AS v FROM hook_event_delivery \
              WHERE delivered_at IS NOT NULL",
        )
        .await;
        assert_eq!(delivered, "observer");

        due_now(&db).await;
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(
            (answered.calls(), flaky.call_count.load(Ordering::SeqCst)),
            (1, 2),
            "only the plugin that had not answered ran again"
        );
        let completed: bool =
            scalar(&db, "SELECT completed_at IS NOT NULL AS v FROM hook_event").await;
        assert!(completed, "complete once both have it");
    }

    #[test]
    fn the_backoff_doubles_from_its_base_up_to_its_cap() {
        assert_eq!(backoff_secs(1), BACKOFF_BASE_SECS);
        assert_eq!(backoff_secs(2), 2 * BACKOFF_BASE_SECS);
        assert_eq!(backoff_secs(3), 4 * BACKOFF_BASE_SECS);
        assert_eq!(backoff_secs(MAX_EVENT_ATTEMPTS), BACKOFF_MAX_SECS);
        assert_eq!(backoff_secs(i32::MAX), BACKOFF_MAX_SECS);
    }

    /// An event a hook plugin never answers is retried each time later than
    /// the last, and after its last attempt is dead-lettered: it leaves the
    /// re-scan and stands in the dead letter queue as a `hook_event`, with
    /// each attempt and why it failed in its retry history and the plugin's
    /// last error in its payload. Retrying the dead letter gives the event a
    /// fresh run, and the event completing resolves the dead letter.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_never_answered_backs_off_and_is_dead_lettered_at_its_cap() {
        let (_pg, db) = crate::test_support::postgres().await;
        let down = Arc::new(DownHook::new("down"));
        let registry = replica_with(down.clone()).await;
        dispatch_once(&db, &registry).await;
        let wait: f64 = scalar(
            &db,
            "SELECT EXTRACT(EPOCH FROM next_attempt_at - now())::float8 AS v FROM hook_event",
        )
        .await;
        assert!(
            wait > 0.0 && wait <= BACKOFF_BASE_SECS as f64,
            "the second attempt waits the base backoff, waited {wait}"
        );
        let mut waits = vec![];
        for _ in 1..MAX_EVENT_ATTEMPTS {
            due_now(&db).await;
            redeliver_once(&db, &registry).await.unwrap();
            waits.push(
                scalar::<Option<f64>>(
                    &db,
                    "SELECT CASE WHEN dead_at IS NULL \
                         THEN EXTRACT(EPOCH FROM next_attempt_at - now())::float8 END AS v \
                       FROM hook_event",
                )
                .await,
            );
        }
        assert_eq!(
            down.call_count.load(Ordering::SeqCst),
            MAX_EVENT_ATTEMPTS as usize
        );
        // The wait after attempt `n` is that attempt's backoff, less the
        // moments since it was set.
        for (attempt, wait) in (2..).zip(&waits[..waits.len() - 1]) {
            let wait = wait.expect("backed off, not dead");
            let backoff = backoff_secs(attempt) as f64;
            assert!(
                wait <= backoff && backoff - wait < 1.0,
                "attempt {attempt} waits {backoff}s, waited {wait}"
            );
        }
        assert_eq!(waits.last(), Some(&None), "dead after the last attempt");

        let (dead, attempts): (bool, i32) = (
            scalar(&db, "SELECT dead_at IS NOT NULL AS v FROM hook_event").await,
            scalar(&db, "SELECT attempts AS v FROM hook_event").await,
        );
        assert!(dead);
        assert_eq!(attempts, MAX_EVENT_ATTEMPTS);
        let letter: String = scalar(
            &db,
            "SELECT direction || ' ' || message_id AS v FROM dead_letter_message WHERE NOT resolved",
        )
        .await;
        assert_eq!(letter, "hook_event hook_event:after_judging:1:1");
        let (why, retries, last_error): (String, i32, String) = (
            scalar(&db, "SELECT error_message AS v FROM dead_letter_message").await,
            scalar(&db, "SELECT retry_count AS v FROM dead_letter_message").await,
            scalar(
                &db,
                "SELECT payload -> 'failures' ->> 'down' AS v FROM dead_letter_message",
            )
            .await,
        );
        assert!(why.starts_with("not yet delivered to down ("), "{why}");
        assert_eq!(
            retries, MAX_EVENT_ATTEMPTS,
            "every attempt is in its history"
        );
        assert!(why.contains(&last_error), "{why} names {last_error}");

        due_now(&db).await;
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(
            down.call_count.load(Ordering::SeqCst),
            MAX_EVENT_ATTEMPTS as usize,
            "a dead event is never run again"
        );

        // Retried from the dead letter queue, it runs again once the plugin
        // answers, and completing resolves the dead letter.
        assert!(revive_dead_event(&db, "after_judging:1:1").await.unwrap());
        assert!(
            !revive_dead_event(&db, "after_judging:1:1").await.unwrap(),
            "a revived event is not dead"
        );
        let up = Arc::new(MockHook::new(
            "down",
            "after_judging",
            GenericHookAction::Pass,
        ));
        redeliver_once(&db, &replica_with(up.clone()).await)
            .await
            .unwrap();
        assert_eq!(up.calls(), 1);
        let (completed, resolved): (bool, bool) = (
            scalar(&db, "SELECT completed_at IS NOT NULL AS v FROM hook_event").await,
            scalar(&db, "SELECT resolved AS v FROM dead_letter_message").await,
        );
        assert!(completed && resolved);
    }

    /// One owed plugin's failure holds back no other: on a background event
    /// each plugin's hooks run on their own in the same attempt, so a plugin
    /// sorted after one that traps still gets the event at once. The plugin
    /// whose hook trapped is not counted delivered: it keeps its error and
    /// is run again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failing_hook_plugin_holds_back_no_other_and_stays_owed() {
        let (_pg, db) = crate::test_support::postgres().await;
        let failing: Arc<dyn GenericHook<Context = CallOptions>> = Arc::new(FailingHook {
            id: "a-traps".into(),
            topics: vec!["after_judging".into()],
        });
        let after = Arc::new(MockHook::new(
            "b-answers",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let registry = new_shared_registry();
        for (hook, mode) in [
            (failing.clone(), HookMode::Blocking),
            (
                after.clone() as Arc<dyn GenericHook<Context = CallOptions>>,
                HookMode::Notify,
            ),
        ] {
            register_direct(&mut *registry.write().await, hook, HookScope::Global, mode);
        }
        dispatch_once(&db, &registry).await;
        assert_eq!(after.calls(), 1, "delivered in the same attempt");
        let (delivered, failed): (String, String) = (
            scalar(
                &db,
                "SELECT string_agg(plugin_id, ',') AS v FROM hook_event_delivery \
                  WHERE delivered_at IS NOT NULL",
            )
            .await,
            scalar(
                &db,
                "SELECT plugin_id || ': ' || last_error AS v FROM hook_event_delivery \
                  WHERE delivered_at IS NULL",
            )
            .await,
        );
        assert_eq!(delivered, "b-answers");
        assert!(
            failed.starts_with("a-traps: ") && failed.contains("WASM crash"),
            "{failed}"
        );
        let completed: bool =
            scalar(&db, "SELECT completed_at IS NOT NULL AS v FROM hook_event").await;
        assert!(!completed, "the trapped plugin is still owed it");

        due_now(&db).await;
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(after.calls(), 1, "the delivered plugin is not called again");
    }

    /// Events that keep failing never starve newer ones: the re-scan takes
    /// events by when their next attempt is due, so a batch's worth of old
    /// events backed off leaves a fresh event the first taken.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn events_that_keep_failing_do_not_starve_newer_ones() {
        let (_pg, db) = crate::test_support::postgres().await;
        let hook = Arc::new(MockHook::new(
            "observer",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let registry = replica_with(hook.clone()).await;
        execute(
            &db,
            &format!(
                "INSERT INTO hook_event (event_id, topic, payload, created_at, attempts, \
                                         next_attempt_at) \
                 SELECT 'stuck:' || n, 'after_judging', '{{}}', now() - interval '1 day', 5, \
                        now() + interval '1 hour' \
                   FROM generate_series(1, {}) n",
                REDELIVERY_BATCH * 2
            ),
        )
        .await;
        execute(
            &db,
            "INSERT INTO hook_event (event_id, topic, payload) \
             VALUES ('fresh', 'after_judging', '{}')",
        )
        .await;
        redeliver_once(&db, &registry).await.unwrap();
        assert_eq!(hook.calls(), 1);
        let fresh: bool = scalar(
            &db,
            "SELECT completed_at IS NOT NULL AS v FROM hook_event WHERE event_id = 'fresh'",
        )
        .await;
        assert!(fresh, "the fresh event ran ahead of the backed-off ones");
    }

    /// Blocks its first call until `release`, then passes.
    struct GatedHook {
        call_count: AtomicUsize,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        topics: Vec<String>,
    }

    #[async_trait]
    impl GenericHook for GatedHook {
        type Context = CallOptions;

        fn id(&self) -> &str {
            "observer"
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
                self.release.notified().await;
            }
            Ok(HookAction::Pass)
        }
    }

    /// A dispatch whose lease lapsed and passed to another records nothing
    /// when it ends: the event stays as the dispatch that holds it now left
    /// it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_dispatch_that_lost_its_lease_records_nothing() {
        let (_pg, db) = crate::test_support::postgres().await;
        let gated = Arc::new(GatedHook {
            call_count: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            topics: vec!["after_judging".into()],
        });
        let first = replica_with(gated.clone()).await;
        let running = tokio::spawn({
            let db = db.clone();
            async move { dispatch_once(&db, &first).await }
        });
        gated.started.notified().await;

        // Its lease lapses; another replica takes the event, and its hook
        // plugin cannot answer.
        execute(
            &db,
            "UPDATE hook_event SET claimed_at = claimed_at - interval '1 day'",
        )
        .await;
        let down = Arc::new(DownHook::new("observer"));
        redeliver_once(&db, &replica_with(down.clone()).await)
            .await
            .unwrap();
        assert_eq!(down.call_count.load(Ordering::SeqCst), 1);

        gated.release.notify_one();
        running.await.unwrap();
        let (completed, delivered, attempts): (bool, i64, i32) = (
            scalar(&db, "SELECT completed_at IS NOT NULL AS v FROM hook_event").await,
            scalar(
                &db,
                "SELECT COUNT(*)::bigint AS v FROM hook_event_delivery \
                  WHERE delivered_at IS NOT NULL",
            )
            .await,
            scalar(&db, "SELECT attempts AS v FROM hook_event").await,
        );
        assert!(!completed, "the stale dispatch did not complete it");
        assert_eq!(delivered, 0, "nor record it delivered");
        assert_eq!(attempts, 2);
    }

    /// An event is complete only once every hook plugin enabled fleet-wide
    /// that it is for has it delivered: a replica that does not load one
    /// delivers what it loads and gives the event back, and a replica that
    /// loads the rest completes it without calling the first plugin again.
    /// A disabled plugin is owed nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_event_waits_for_an_enabled_hook_plugin_this_replica_does_not_load() {
        use crate::plugin_lifecycle::{Governs, HookTopic, PluginLifecycle};
        let (_pg, db) = crate::test_support::postgres().await;
        let lifecycle = Arc::new(PluginLifecycle::default());
        let hooks_after_judging = Governs {
            hooks: vec![HookTopic {
                topic: "after_judging".into(),
                scope: HookScope::Global,
            }],
            ..Governs::default()
        };
        for (id, enabled) in [("observer", true), ("elsewhere", true), ("off", false)] {
            lifecycle.set_desired(id, enabled);
            lifecycle.set_governs(id, hooks_after_judging.clone());
        }
        let replica = |hook: Arc<MockHook>| {
            let registry = shared_registry_for(lifecycle.clone());
            async move {
                register_direct(
                    &mut *registry.write().await,
                    hook,
                    HookScope::Global,
                    HookMode::Notify,
                );
                registry
            }
        };
        let observer = Arc::new(MockHook::new(
            "observer",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let elsewhere = Arc::new(MockHook::new(
            "elsewhere",
            "after_judging",
            GenericHookAction::Pass,
        ));
        let here = replica(observer.clone()).await;
        dispatch_once(&db, &here).await;
        let (completed, why): (bool, String) = (
            scalar(&db, "SELECT completed_at IS NOT NULL AS v FROM hook_event").await,
            scalar(&db, "SELECT last_error AS v FROM hook_event").await,
        );
        assert!(!completed, "the event is owed to a plugin not loaded here");
        assert_eq!(
            why,
            "not yet delivered to elsewhere (not loaded on this replica)"
        );

        let there = replica(elsewhere.clone()).await;
        due_now(&db).await;
        redeliver_once(&db, &there).await.unwrap();
        assert_eq!((observer.calls(), elsewhere.calls()), (1, 1));
        let completed: bool =
            scalar(&db, "SELECT completed_at IS NOT NULL AS v FROM hook_event").await;
        assert!(completed, "complete once every enabled hook plugin has it");
    }

    /// The re-scan loop stops when cancelled with a delivery in flight, and
    /// the lease that delivery held is given back for the next re-scan.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_redelivery_loop_stops_with_a_delivery_in_flight() {
        let (_pg, db) = crate::test_support::postgres().await;
        let stalling = Arc::new(StallingHook {
            id: "observer".into(),
            topics: vec!["after_judging".into()],
            call_count: AtomicUsize::new(0),
            started: tokio::sync::Notify::new(),
        });
        execute(
            &db,
            "INSERT INTO hook_event (event_id, topic, payload) \
             VALUES ('after_judging:1:1', 'after_judging', '{}')",
        )
        .await;
        let registry = replica_with(stalling.clone()).await;
        let (cancel, cancelled) = tokio::sync::watch::channel(false);
        let running = tokio::spawn(run_redelivery(db.clone(), registry, cancelled));
        stalling.started.notified().await;
        cancel.send(true).unwrap();
        running.await.expect("the loop returned");
        wait_until_released(&db, "after_judging:1:1").await;
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

    /// A hook that could not answer because the database could not serve
    /// it keeps the database's code: a blocking hook failing as the
    /// database being busy or unreachable answers `DATABASE_BUSY` or
    /// `DATABASE_UNAVAILABLE` (each with its own `Retry-After`), not the
    /// plugin's being busy.
    #[tokio::test]
    async fn an_unavailable_blocking_hook_keeps_the_databases_code() {
        struct DatabaseHook {
            topics: Vec<String>,
            lost: bool,
        }

        #[async_trait]
        impl GenericHook for DatabaseHook {
            type Context = CallOptions;

            fn id(&self) -> &str {
                "db"
            }

            fn topics(&self) -> &[String] {
                &self.topics
            }

            async fn on_event(
                &self,
                _ctx: CallOptions,
                _event: &GenericEvent,
            ) -> anyhow::Result<GenericHookAction> {
                let (plugin_id, func_name) = ("db".to_string(), "gate".to_string());
                Err(if self.lost {
                    PluginError::DatabaseUnavailable {
                        plugin_id,
                        func_name,
                    }
                } else {
                    PluginError::DatabaseBusy {
                        plugin_id,
                        func_name,
                    }
                }
                .into())
            }
        }

        for lost in [false, true] {
            let registry = new_shared_registry();
            registry.write().await.hooks.insert(
                "test".into(),
                vec![HookEntry {
                    plugin_id: "db".into(),
                    function: "on_event".into(),
                    scope: HookScope::Global,
                    mode: HookMode::Blocking,
                    hook: Arc::new(DatabaseHook {
                        topics: vec!["test".into()],
                        lost,
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
            if lost {
                assert!(
                    matches!(result, Err(AppError::DatabaseUnavailable)),
                    "{result:?}"
                );
            } else {
                assert!(matches!(result, Err(AppError::DatabaseBusy)), "{result:?}");
            }
        }
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
