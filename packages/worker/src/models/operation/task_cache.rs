use std::collections::HashMap;

use async_trait::async_trait;
use sea_orm::{ConnectionTrait, DbBackend, EntityTrait, Statement};

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait TaskCacheStore: Send + Sync {
    async fn get(&self, cache_key: &str) -> Result<Option<HashMap<String, String>>, String>;

    async fn put(
        &self,
        cache_key: &str,
        outputs: HashMap<String, String>,
    ) -> Result<TaskCachePutOutcome, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskCachePutOutcome {
    Inserted,
    AlreadyExists,
}

/// Client-side bound on one task-cache round-trip (a primary-key read or an
/// insert). Every caller treats a cache error as a miss, so giving up early
/// only costs a recompute, whereas a read hung on a vanished database host
/// would wedge the worker for good while it keeps heartbeating `in_flight=1`.
const TASK_CACHE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

async fn bounded<T>(
    step: &'static str,
    fut: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    common::deadline::bounded(step, TASK_CACHE_TIMEOUT, fut)
        .await
        .unwrap_or_else(|| Err(format!("{step} timed out after {TASK_CACHE_TIMEOUT:?}")))
}

pub struct DatabaseTaskCacheStore {
    db: common::db::DatabaseConnection,
}

impl DatabaseTaskCacheStore {
    pub fn new(db: common::db::DatabaseConnection) -> Self {
        Self { db }
    }

    pub async fn ensure_table(db: &common::db::DatabaseConnection) -> Result<(), String> {
        use sea_orm::{ConnectionTrait, Schema};
        let backend = db.get_database_backend();
        let schema = Schema::new(backend);
        let mut create_stmt = schema.create_table_from_entity(task_cache::Entity);
        create_stmt.if_not_exists();

        if let Err(e) = db.execute(&create_stmt).await {
            if common::storage::database::is_concurrent_create_race(&e) {
                tracing::debug!(error = %e, "concurrent task_cache create race; treating as success");
                return Ok(());
            }
            return Err(format!("Failed to ensure task_cache table: {e}"));
        }
        Ok(())
    }
}

#[async_trait]
impl TaskCacheStore for DatabaseTaskCacheStore {
    async fn get(&self, cache_key: &str) -> Result<Option<HashMap<String, String>>, String> {
        let result = bounded("task_cache lookup", async {
            task_cache::Entity::find_by_id(cache_key.to_string())
                .one(&self.db)
                .await
                .map_err(|e| format!("task_cache lookup failed: {e}"))
        })
        .await?;

        match result {
            Some(model) => {
                let outputs: HashMap<String, String> = serde_json::from_value(model.outputs)
                    .map_err(|e| format!("Failed to deserialize cached outputs: {e}"))?;
                Ok(Some(outputs))
            }
            None => Ok(None),
        }
    }

    async fn put(
        &self,
        cache_key: &str,
        outputs: HashMap<String, String>,
    ) -> Result<TaskCachePutOutcome, String> {
        let outputs_json = serde_json::to_value(&outputs)
            .map_err(|e| format!("Failed to serialize outputs: {e}"))?;

        bounded("task_cache insert", async {
            // `created_at` is the database clock, the platform's wall time.
            let result = self
                .db
                .execute_raw(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "INSERT INTO task_cache (cache_key, outputs, created_at) \
                     VALUES ($1, $2, now()) ON CONFLICT (cache_key) DO NOTHING",
                    [cache_key.into(), outputs_json.into()],
                ))
                .await;

            match result {
                Ok(done) => Ok(task_cache_put_outcome(done.rows_affected())),
                Err(e) => Err(format!("task_cache insert failed: {e}")),
            }
        })
        .await
    }
}

fn task_cache_put_outcome(rows_affected: u64) -> TaskCachePutOutcome {
    if rows_affected == 0 {
        TaskCachePutOutcome::AlreadyExists
    } else {
        TaskCachePutOutcome::Inserted
    }
}

/// How many entries [`MemoizedTaskCacheStore`] keeps. An entry is a cache key
/// and a few output hashes, so the memo stays well under a megabyte.
const MEMO_ENTRIES: std::num::NonZeroUsize = std::num::NonZeroUsize::new(4096).unwrap();

/// A task cache that answers repeat lookups from process memory.
///
/// Entries are immutable: the key covers the target, the toolchain, the argv
/// and every input byte, and an insert never replaces a stored key. So an entry
/// read or written once stays valid, and later lookups of its key skip the
/// round trip to `inner`. Misses are not memoized, since another worker may
/// store the key at any time.
///
/// The memo never vouches for blobs. Restoring a hit materializes each output
/// through the file cacher, which fetches it from the blob store when the local
/// copy is gone. When that fails too (evicted locally and deleted in the store),
/// the step runs, and storing its fresh outputs replaces the memoized entry.
pub struct MemoizedTaskCacheStore<S> {
    inner: S,
    memo: std::sync::Mutex<lru::LruCache<String, HashMap<String, String>>>,
}

impl<S: TaskCacheStore> MemoizedTaskCacheStore<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            memo: std::sync::Mutex::new(lru::LruCache::new(MEMO_ENTRIES)),
        }
    }

    /// The memo, recovered from poisoning: every update is a single map call.
    fn memo(&self) -> std::sync::MutexGuard<'_, lru::LruCache<String, HashMap<String, String>>> {
        self.memo.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[async_trait]
impl<S: TaskCacheStore> TaskCacheStore for MemoizedTaskCacheStore<S> {
    async fn get(&self, cache_key: &str) -> Result<Option<HashMap<String, String>>, String> {
        if let Some(outputs) = self.memo().get(cache_key) {
            return Ok(Some(outputs.clone()));
        }
        let outputs = self.inner.get(cache_key).await?;
        if let Some(outputs) = &outputs {
            self.memo().put(cache_key.to_string(), outputs.clone());
        }
        Ok(outputs)
    }

    async fn put(
        &self,
        cache_key: &str,
        outputs: HashMap<String, String>,
    ) -> Result<TaskCachePutOutcome, String> {
        let outcome = self.inner.put(cache_key, outputs.clone()).await?;
        self.memo().put(cache_key.to_string(), outputs);
        Ok(outcome)
    }
}

pub struct NoopTaskCacheStore;

#[async_trait]
impl TaskCacheStore for NoopTaskCacheStore {
    async fn get(&self, _cache_key: &str) -> Result<Option<HashMap<String, String>>, String> {
        Ok(None)
    }

    async fn put(
        &self,
        _cache_key: &str,
        _outputs: HashMap<String, String>,
    ) -> Result<TaskCachePutOutcome, String> {
        Ok(TaskCachePutOutcome::Inserted)
    }
}

#[allow(clippy::module_inception)]
pub mod task_cache {
    use sea_orm::entity::prelude::*;
    use serde::{Deserialize, Serialize};

    #[sea_orm::model]
    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize, Deserialize)]
    #[sea_orm(table_name = "task_cache")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub cache_key: String,

        #[sea_orm(column_type = "JsonBinary")]
        pub outputs: serde_json::Value,

        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}
}

const TARGET_TRIPLE: &str = env!("TARGET_TRIPLE");

pub fn compute_cache_key(
    toolchain_fingerprint: &str,
    argv: &[String],
    input_files: &[(String, Vec<u8>)],
) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();

    hasher.update(TARGET_TRIPLE.as_bytes());
    hasher.update(b"\0");

    hasher.update(toolchain_fingerprint.as_bytes());
    hasher.update(b"\0");

    let argv_joined = argv.join("\0");
    hasher.update(argv_joined.as_bytes());
    hasher.update(b"\0");

    let mut sorted_inputs: Vec<_> = input_files.iter().collect();
    sorted_inputs.sort_by_key(|(name, _)| name.as_str());

    for (name, content) in sorted_inputs {
        hasher.update(name.as_bytes());
        hasher.update(b"\0");
        hasher.update(content.len().to_string().as_bytes());
        hasher.update(b"\0");
        hasher.update(content);
        hasher.update(b"\0");
    }

    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_deterministic() {
        let argv = vec!["gcc".into(), "-o".into(), "out".into()];
        let files = vec![
            ("main.c".into(), b"int main() {}".to_vec()),
            ("helper.h".into(), b"void help();".to_vec()),
        ];
        let key1 = compute_cache_key("", &argv, &files);
        let key2 = compute_cache_key("", &argv, &files);
        assert_eq!(key1, key2);
    }

    #[test]
    fn cache_key_order_independent() {
        let argv = vec!["gcc".into()];
        let files_a = vec![
            ("a.c".into(), b"aaa".to_vec()),
            ("b.c".into(), b"bbb".to_vec()),
        ];
        let files_b = vec![
            ("b.c".into(), b"bbb".to_vec()),
            ("a.c".into(), b"aaa".to_vec()),
        ];
        assert_eq!(
            compute_cache_key("", &argv, &files_a),
            compute_cache_key("", &argv, &files_b)
        );
    }

    #[test]
    fn cache_key_differs_for_different_content() {
        let argv = vec!["gcc".into()];
        let files1 = vec![("main.c".into(), b"version 1".to_vec())];
        let files2 = vec![("main.c".into(), b"version 2".to_vec())];
        assert_ne!(
            compute_cache_key("", &argv, &files1),
            compute_cache_key("", &argv, &files2)
        );
    }

    #[test]
    fn cache_key_differs_for_different_fingerprints() {
        let argv = vec!["gcc".into()];
        let files = vec![("main.c".into(), b"int main() {}".to_vec())];
        let key_a = compute_cache_key("fingerprint-a", &argv, &files);
        let key_b = compute_cache_key("fingerprint-b", &argv, &files);
        assert_ne!(key_a, key_b);
    }

    #[tokio::test]
    async fn noop_cache_always_misses() {
        let cache = NoopTaskCacheStore;
        assert!(cache.get("any_key").await.unwrap().is_none());
        assert!(cache.put("any_key", HashMap::new()).await.is_ok());
    }

    /// A cache call that never returns (here: blocked behind a table lock with
    /// no server-side statement timeout, standing in for a read hung on a
    /// vanished database host) gives up with an error the caller treats as a
    /// miss, instead of wedging the worker.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hung_cache_call_times_out_as_an_error() {
        use sea_orm::{ConnectionTrait, TransactionTrait};
        use testcontainers::ImageExt;
        use testcontainers::runners::AsyncRunner;

        let container = testcontainers_modules::postgres::Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let db = common::db::connect(
            &url,
            &common::db::PgPoolSpec::new(2, 1),
            &common::db::PgSession::unbounded("task-cache-test"),
        )
        .await
        .expect("connect");
        DatabaseTaskCacheStore::ensure_table(&db)
            .await
            .expect("table");

        let locker = db.begin().await.expect("begin");
        locker
            .execute_unprepared("LOCK TABLE task_cache IN ACCESS EXCLUSIVE MODE")
            .await
            .expect("lock");

        let store = DatabaseTaskCacheStore::new(db.clone());
        let started = std::time::Instant::now();
        let err = store
            .get("k")
            .await
            .expect_err("blocked lookup must time out");
        assert!(err.contains("timed out"), "{err}");
        assert!(started.elapsed() < TASK_CACHE_TIMEOUT + std::time::Duration::from_secs(5));
        locker.rollback().await.expect("rollback");
    }

    /// Answers every lookup with `entry`, counting the lookups that reach it.
    struct CountingStore {
        entry: Option<HashMap<String, String>>,
        gets: std::sync::atomic::AtomicUsize,
    }

    impl CountingStore {
        fn new(entry: Option<HashMap<String, String>>) -> Self {
            Self {
                entry,
                gets: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl TaskCacheStore for CountingStore {
        async fn get(&self, _cache_key: &str) -> Result<Option<HashMap<String, String>>, String> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(self.entry.clone())
        }

        async fn put(
            &self,
            _cache_key: &str,
            _outputs: HashMap<String, String>,
        ) -> Result<TaskCachePutOutcome, String> {
            Ok(TaskCachePutOutcome::AlreadyExists)
        }
    }

    fn outputs(hash: &str) -> HashMap<String, String> {
        HashMap::from([("main".to_string(), hash.to_string())])
    }

    #[tokio::test]
    async fn a_memoized_hit_is_answered_without_the_store() {
        let store = MemoizedTaskCacheStore::new(CountingStore::new(Some(outputs("a"))));
        assert_eq!(store.get("k").await.unwrap(), Some(outputs("a")));
        assert_eq!(store.get("k").await.unwrap(), Some(outputs("a")));
        assert_eq!(
            store.inner.gets.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[tokio::test]
    async fn a_miss_asks_the_store_every_time() {
        let store = MemoizedTaskCacheStore::new(CountingStore::new(None));
        assert_eq!(store.get("k").await.unwrap(), None);
        assert_eq!(store.get("k").await.unwrap(), None);
        assert_eq!(
            store.inner.gets.load(std::sync::atomic::Ordering::Relaxed),
            2
        );
    }

    #[tokio::test]
    async fn a_stored_entry_replaces_the_memoized_one() {
        let store = MemoizedTaskCacheStore::new(CountingStore::new(Some(outputs("old"))));
        assert_eq!(store.get("k").await.unwrap(), Some(outputs("old")));
        store.put("k", outputs("fresh")).await.unwrap();
        assert_eq!(store.get("k").await.unwrap(), Some(outputs("fresh")));
        assert_eq!(
            store.inner.gets.load(std::sync::atomic::Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn task_cache_put_outcome_treats_zero_rows_as_existing_entry() {
        assert_eq!(
            task_cache_put_outcome(0),
            TaskCachePutOutcome::AlreadyExists
        );
        assert_eq!(task_cache_put_outcome(1), TaskCachePutOutcome::Inserted);
    }
}
