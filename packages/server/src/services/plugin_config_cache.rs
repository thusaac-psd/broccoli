//! Per-server cache of `plugin_config` point reads (the `config:read` host
//! function and the evaluator's checker config), kept correct across replicas
//! by a database-side change counter.
//!
//! The counter is the sequence `plugin_config_epoch`. A deferred constraint
//! trigger on `plugin_config` advances it once per changed row as the writing
//! transaction commits, and announces [`CHANGED`] on the claim channel. A
//! sequence takes no row lock, so concurrent config writers never serialise or
//! deadlock on it, and a statement that changes no rows fires no trigger.
//!
//! Every server learns of changes without an extra statement:
//!
//! - the notification, which every claim fiber already listens for. Delivery
//!   on one listening session follows commit order, so the config change is
//!   seen before the queued-submission wake of any submission committed after
//!   it;
//! - the claim statement itself, which returns the counter with every unit it
//!   claims. A unit can only be claimed after its submission committed, so a
//!   config change committed before a submission is seen before that
//!   submission is judged on every replica, whether or not its notification
//!   arrived;
//! - a (re)connect of the listening session, during which notifications may
//!   have been lost.
//!
//! Any of these that reports a change clears the cache. An entry is valid only
//! for the cache generation it was read in, so a read racing a clear is never
//! served afterwards.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use dashmap::DashMap;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

use crate::entity::plugin_config;

/// Payload of a config-change notification on the claim channel.
pub const CHANGED: &str = "config";

/// The sequence counting committed `plugin_config` changes. SQL that reads it
/// (the claim statement) selects `last_value` from it.
pub const EPOCH_SEQUENCE: &str = "plugin_config_epoch";

type Key = (String, String, String);

/// One cached read: the generation it was read in and the row, if any.
type Entry = (u64, Option<Arc<plugin_config::Model>>);

/// See the module docs. Cheap to clone; clones share entries.
#[derive(Clone)]
pub struct PluginConfigCache {
    inner: Arc<Inner>,
}

struct Inner {
    /// Every read goes to the database: a server whose claim fiber is off
    /// hears of no changes.
    bypass: bool,
    /// The counter value last reported by the database. Any other value means
    /// a change.
    epoch: AtomicI64,
    /// Bumped on every clear; entries from an older generation never hit.
    generation: AtomicU64,
    entries: DashMap<Key, Entry>,
}

impl Default for PluginConfigCache {
    fn default() -> Self {
        Self::with_bypass(false)
    }
}

impl PluginConfigCache {
    fn with_bypass(bypass: bool) -> Self {
        Self {
            inner: Arc::new(Inner {
                bypass,
                epoch: AtomicI64::new(i64::MIN),
                generation: AtomicU64::new(0),
                entries: DashMap::new(),
            }),
        }
    }

    /// A cache that stores nothing, for a server that does not listen for
    /// changes.
    pub fn uncached() -> Self {
        Self::with_bypass(true)
    }

    /// Forget every entry.
    pub fn clear(&self) {
        self.inner.generation.fetch_add(1, Ordering::AcqRel);
        self.inner.entries.clear();
    }

    /// The database reports the change counter at `epoch`. Any value other
    /// than the last one reported (higher, or lower after a restore) clears
    /// the cache.
    pub fn observe_epoch(&self, epoch: i64) {
        if self.inner.epoch.swap(epoch, Ordering::AcqRel) != epoch {
            self.clear();
        }
    }

    /// Handle a claim-channel payload; true when it was a config change.
    pub fn observe_notification(&self, payload: &str) -> bool {
        let changed = payload == CHANGED;
        if changed {
            self.clear();
        }
        changed
    }

    /// Drop one entry, for a writer on this server that reads its own write
    /// before the change notification arrives.
    pub fn forget(&self, scope: &str, ref_id: &str, namespace: &str) {
        self.inner
            .entries
            .remove(&(scope.to_owned(), ref_id.to_owned(), namespace.to_owned()));
    }

    /// The `plugin_config` row for this key, from the cache while it is
    /// current, else in one statement from `db`.
    pub async fn get<C: ConnectionTrait>(
        &self,
        db: &C,
        scope: &str,
        ref_id: &str,
        namespace: &str,
    ) -> Result<Option<Arc<plugin_config::Model>>, DbErr> {
        let key = (scope.to_owned(), ref_id.to_owned(), namespace.to_owned());
        let generation = self.inner.generation.load(Ordering::Acquire);
        if !self.inner.bypass
            && let Some(entry) = self.inner.entries.get(&key)
            && entry.0 == generation
        {
            return Ok(entry.1.clone());
        }

        let json: Option<serde_json::Value> = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT to_jsonb(c) AS row FROM plugin_config c \
                 WHERE c.scope = $1 AND c.ref_id = $2 AND c.namespace = $3",
                [scope.into(), ref_id.into(), namespace.into()],
            ))
            .await?
            .map(|row| row.try_get("", "row"))
            .transpose()?;
        let model = json
            .map(serde_json::from_value::<plugin_config::Model>)
            .transpose()
            .map_err(|e| DbErr::Custom(format!("malformed plugin_config row: {e}")))?
            .map(Arc::new);

        if !self.inner.bypass {
            // Tagged with the generation the read started in: a clear that
            // raced the read makes it miss from then on.
            self.inner.entries.insert(key, (generation, model.clone()));
        }
        Ok(model)
    }
}

const FUNCTION: &str = "broccoli_plugin_config_changed";

/// Creates the change counter and its triggers. Boot runs this under the boot
/// lock, after the schema exists.
///
/// The row trigger is a deferred constraint trigger, so the counter advances
/// as the writing transaction commits rather than when it first writes: a
/// reader can only see it advance once the change is about to be visible.
/// `TRUNCATE` fires no row trigger, so a statement trigger covers it. Only
/// the host writes `plugin_config`; plugins have no privilege on it or on the
/// counter.
pub async fn install_plugin_config_epoch<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
    let channel = crate::services::judging::CLAIM_CHANNEL;
    db.execute_unprepared(&format!(
        "CREATE SEQUENCE IF NOT EXISTS {EPOCH_SEQUENCE}; \
         SELECT nextval('{EPOCH_SEQUENCE}') WHERE NOT (SELECT is_called FROM {EPOCH_SEQUENCE})"
    ))
    .await?;
    db.execute_unprepared(&format!(
        "CREATE OR REPLACE FUNCTION {FUNCTION}() RETURNS trigger LANGUAGE plpgsql AS $$ \
         BEGIN \
           PERFORM nextval('{EPOCH_SEQUENCE}'); \
           PERFORM pg_notify('{channel}', '{CHANGED}'); \
           RETURN NULL; \
         END $$"
    ))
    .await?;
    for (name, ddl) in [
        (
            FUNCTION.to_string(),
            format!(
                "CREATE CONSTRAINT TRIGGER {FUNCTION} AFTER INSERT OR UPDATE OR DELETE \
                 ON plugin_config DEFERRABLE INITIALLY DEFERRED \
                 FOR EACH ROW EXECUTE FUNCTION {FUNCTION}()"
            ),
        ),
        (
            format!("{FUNCTION}_truncate"),
            format!(
                "CREATE TRIGGER {FUNCTION}_truncate AFTER TRUNCATE ON plugin_config \
                 FOR EACH STATEMENT EXECUTE FUNCTION {FUNCTION}()"
            ),
        ),
    ] {
        let exists = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM pg_trigger WHERE tgname = $1",
                [name.into()],
            ))
            .await?
            .is_some();
        if !exists {
            db.execute_unprepared(&ddl).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cache: &PluginConfigCache) {
        let generation = cache.inner.generation.load(Ordering::Acquire);
        cache.inner.entries.insert(
            ("plugin".into(), "p".into(), "ns".into()),
            (generation, None),
        );
    }

    #[test]
    fn any_change_of_the_counter_clears_the_cache_even_a_step_back() {
        let cache = PluginConfigCache::default();
        cache.observe_epoch(3);
        entry(&cache);
        cache.observe_epoch(3);
        assert_eq!(cache.inner.entries.len(), 1);
        cache.observe_epoch(4);
        assert!(cache.inner.entries.is_empty());
        entry(&cache);
        cache.observe_epoch(1);
        assert!(
            cache.inner.entries.is_empty(),
            "a restored counter still invalidates"
        );
    }

    #[test]
    fn only_a_config_notification_clears_the_cache() {
        let cache = PluginConfigCache::default();
        entry(&cache);
        assert!(!cache.observe_notification("queued"));
        assert!(!cache.observe_notification("freed:server-1"));
        assert_eq!(cache.inner.entries.len(), 1);
        assert!(cache.observe_notification(CHANGED));
        assert!(cache.inner.entries.is_empty());
    }

    #[test]
    fn an_entry_from_before_a_clear_never_hits() {
        let cache = PluginConfigCache::default();
        entry(&cache);
        cache.inner.generation.fetch_add(1, Ordering::AcqRel);
        let key = ("plugin".to_string(), "p".to_string(), "ns".to_string());
        let stale = cache.inner.entries.get(&key).expect("still stored");
        assert_ne!(stale.0, cache.inner.generation.load(Ordering::Acquire));
    }
}
