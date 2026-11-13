//! Per-server cache of `plugin_config` point reads (the `config:read` host
//! function and the evaluator's checker config), kept correct across replicas
//! by a database-side version stamp.
//!
//! A statement trigger on `plugin_config` bumps the one-row
//! `plugin_config_epoch` in the transaction that changes it, and announces the
//! new version on the claim channel. Every server learns the current version
//! without an extra statement:
//!
//! - the notification, which every claim fiber already listens for;
//! - the claim pass's in-flight count, which reads the version too. A unit is
//!   claimed only by a pass that started after its submission committed, so a
//!   config change committed before a submission is visible to its judging on
//!   every replica, even if the notification is late.
//!
//! A cache miss reads the row together with the version in one statement, the
//! same one round trip it always cost. An entry serves hits only while the
//! version it was read at is the newest one known.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use dashmap::DashMap;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

use crate::entity::plugin_config;

/// Payload prefix of a config-change notification on the claim channel.
pub const CHANGED_PREFIX: &str = "config:";

type Key = (String, String, String);

/// One cached read: the version it was read at and the row, if any.
type Entry = (i64, Option<Arc<plugin_config::Model>>);

/// See the module docs. Cheap to clone; clones share entries.
#[derive(Clone, Default)]
pub struct PluginConfigCache {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    /// Every read goes to the database: a server whose claim fiber is off
    /// hears of no changes.
    bypass: bool,
    /// The newest `plugin_config_epoch.version` this server has seen.
    epoch: AtomicI64,
    entries: DashMap<Key, Entry>,
}

impl PluginConfigCache {
    /// A cache that stores nothing, for a server that does not listen for
    /// changes.
    pub fn uncached() -> Self {
        Self {
            inner: Arc::new(Inner {
                bypass: true,
                ..Inner::default()
            }),
        }
    }

    /// Record that the database is at `version` or later. Entries read at an
    /// older version stop serving hits.
    pub fn observe(&self, version: i64) {
        let previous = self.inner.epoch.fetch_max(version, Ordering::AcqRel);
        if version > previous {
            self.inner
                .entries
                .retain(|_, (read_at, _)| *read_at >= version);
        }
    }

    /// Parse a claim-channel payload; true when it was a config change.
    pub fn observe_notification(&self, payload: &str) -> bool {
        match payload
            .strip_prefix(CHANGED_PREFIX)
            .and_then(|v| v.parse().ok())
        {
            Some(version) => {
                self.observe(version);
                true
            }
            None => false,
        }
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
        let epoch = self.inner.epoch.load(Ordering::Acquire);
        if !self.inner.bypass
            && let Some(entry) = self.inner.entries.get(&key)
            && entry.0 == epoch
        {
            return Ok(entry.1.clone());
        }

        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT e.version, to_jsonb(c) AS row FROM plugin_config_epoch e \
                 LEFT JOIN plugin_config c ON c.scope = $1 AND c.ref_id = $2 AND c.namespace = $3",
                [scope.into(), ref_id.into(), namespace.into()],
            ))
            .await?
            .ok_or_else(|| DbErr::RecordNotFound("plugin_config_epoch".into()))?;
        let version: i64 = row.try_get("", "version")?;
        let json: Option<serde_json::Value> = row.try_get("", "row")?;
        let model = json
            .map(serde_json::from_value::<plugin_config::Model>)
            .transpose()
            .map_err(|e| DbErr::Custom(format!("malformed plugin_config row: {e}")))?
            .map(Arc::new);

        self.observe(version);
        // A newer version seen meanwhile means this read may be stale already.
        if !self.inner.bypass && self.inner.epoch.load(Ordering::Acquire) == version {
            self.inner.entries.insert(key, (version, model.clone()));
        }
        Ok(model)
    }
}

const FUNCTION: &str = "broccoli_plugin_config_changed";

/// Creates the version row and the trigger that bumps it. Boot runs this under
/// the boot lock, after the schema exists. The function is `SECURITY DEFINER`
/// so a write under the plugin role can bump a row that role cannot write.
pub async fn install_plugin_config_epoch<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
    let channel = crate::services::judging::CLAIM_CHANNEL;
    db.execute_unprepared(
        "CREATE TABLE IF NOT EXISTS plugin_config_epoch (version bigint NOT NULL); \
         INSERT INTO plugin_config_epoch SELECT 0 WHERE NOT EXISTS (SELECT 1 FROM plugin_config_epoch)",
    )
    .await?;
    db.execute_unprepared(&format!(
        "CREATE OR REPLACE FUNCTION {FUNCTION}() RETURNS trigger LANGUAGE plpgsql \
         SECURITY DEFINER SET search_path = public AS $$ \
         DECLARE v bigint; \
         BEGIN \
           UPDATE plugin_config_epoch SET version = version + 1 RETURNING version INTO v; \
           PERFORM pg_notify('{channel}', '{CHANGED_PREFIX}' || v); \
           RETURN NULL; \
         END $$"
    ))
    .await?;
    let exists = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT 1 FROM pg_trigger WHERE tgname = $1",
            [FUNCTION.into()],
        ))
        .await?
        .is_some();
    if !exists {
        db.execute_unprepared(&format!(
            "CREATE TRIGGER {FUNCTION} AFTER INSERT OR UPDATE OR DELETE OR TRUNCATE \
             ON plugin_config FOR EACH STATEMENT EXECUTE FUNCTION {FUNCTION}()"
        ))
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(cache: &PluginConfigCache, version: i64) {
        cache
            .inner
            .entries
            .insert(("plugin".into(), "p".into(), "ns".into()), (version, None));
    }

    #[test]
    fn a_newer_version_evicts_entries_read_before_it() {
        let cache = PluginConfigCache::default();
        cache.observe(3);
        entry(&cache, 3);
        cache.observe(3);
        assert_eq!(cache.inner.entries.len(), 1);
        cache.observe(4);
        assert!(cache.inner.entries.is_empty());
    }

    #[test]
    fn an_old_or_foreign_notification_changes_nothing() {
        let cache = PluginConfigCache::default();
        cache.observe(5);
        entry(&cache, 5);
        assert!(cache.observe_notification("config:4"));
        assert!(!cache.observe_notification("queued"));
        assert!(!cache.observe_notification("freed:server-1"));
        assert_eq!(cache.inner.entries.len(), 1);
        assert!(cache.observe_notification("config:6"));
        assert!(cache.inner.entries.is_empty());
    }
}
