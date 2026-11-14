//! Integration tests for the Redis-backed cache leader-election layer.
//!
//! Tests 1-4 spin up a real Redis container via `testcontainers` so we can
//! verify the SETNX/heartbeat/CAS-release dance against a live server. Test 5
//! also spins up a Postgres container and exercises the end-to-end concurrent
//! compile claim: when 3 OperationHandlers race on the same cache_key, exactly
//! one runs the underlying sandbox; the other two read the cached output back
//! via DatabaseTaskCacheStore. The cargo test runner only executes any of
//! these when Docker is available.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::storage::BlobStore;
use common::storage::filesystem::FilesystemBlobStore;
use futures::future::join_all;
use sea_orm::Database;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::redis::Redis;
use worker::models::operation::cache_leader::{
    CacheLeaderElector, CacheLeaderOpts, LeaderRole, RedisCacheLeaderElector,
};
use worker::models::operation::file_cacher::BlobStoreFileCacher;
use worker::models::operation::handler::{CompileCache, OperationHandler};
use worker::models::operation::models::{
    Environment, IOConfig, OperationTask, RunOptions, SessionFile, Step, StepCacheConfig, StepKind,
};
use worker::models::operation::sandbox::SandboxManager;
use worker::models::operation::sandbox::error::SandboxError;
use worker::models::operation::sandbox::mock::MockSandboxManager;
use worker::models::operation::task_cache::{DatabaseTaskCacheStore, TaskCacheStore};

async fn redis_url() -> (String, testcontainers::ContainerAsync<Redis>) {
    let container = common::test_util::redis_image()
        .start()
        .await
        .expect("failed to start Redis container");
    let port = container
        .get_host_port_ipv4(6379)
        .await
        .expect("failed to get Redis port");
    (format!("redis://127.0.0.1:{port}"), container)
}

fn opts_short_ttl(ttl_secs: u64) -> CacheLeaderOpts {
    CacheLeaderOpts {
        ttl_secs,
        heartbeat_interval_secs: 1,
    }
}

/// Test 1: out of N concurrent acquirers for the same key, exactly one is leader.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exactly_one_leader_among_concurrent_acquirers() {
    let (url, _container) = redis_url().await;

    let n = 3;
    let mut electors = Vec::new();
    for _ in 0..n {
        let elector = Arc::new(
            RedisCacheLeaderElector::from_url(&url, opts_short_ttl(60)).expect("build elector"),
        );
        electors.push(elector);
    }

    let key = format!("test1-{}", uuid::Uuid::new_v4());
    let mut tasks = Vec::new();
    for elector in &electors {
        let key = key.clone();
        let e = elector.clone();
        tasks.push(tokio::spawn(async move { e.acquire(&key).await }));
    }
    let results = join_all(tasks).await;

    let mut leaders = 0;
    let mut followers = 0;
    let mut leases = Vec::new();
    for r in results {
        let role = r.expect("join task").expect("acquire returned Err");
        match role {
            LeaderRole::Leader(lease) => {
                leaders += 1;
                leases.push(lease);
            }
            LeaderRole::Follower => followers += 1,
        }
    }
    assert_eq!(leaders, 1, "exactly one leader expected");
    assert_eq!(followers, n - 1, "remaining acquirers must be followers");
    drop(leases);
}

/// Test 2: a long-lived leader holds the lock through heartbeats - a concurrent
/// acquire well past the original TTL still sees Follower because heartbeat
/// has extended the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeat_keeps_leadership_past_ttl() {
    let (url, _container) = redis_url().await;

    // TTL = 2s, heartbeat = 1s. After 3s (= 1.5x TTL), heartbeat must have refreshed.
    let leader_elector = Arc::new(
        RedisCacheLeaderElector::from_url(&url, opts_short_ttl(2)).expect("build elector"),
    );
    let challenger = Arc::new(
        RedisCacheLeaderElector::from_url(&url, opts_short_ttl(2)).expect("build elector"),
    );
    let key = format!("test2-{}", uuid::Uuid::new_v4());

    let role = leader_elector.acquire(&key).await.expect("acquire ok");
    let lease = match role {
        LeaderRole::Leader(l) => l,
        LeaderRole::Follower => panic!("first acquirer must be leader"),
    };

    // Wait past one full TTL boundary; heartbeat (1s) must have refreshed by now.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let challenger_role = challenger.acquire(&key).await.expect("acquire ok");
    match challenger_role {
        LeaderRole::Leader(_) => {
            panic!("challenger became leader — heartbeat failed to refresh");
        }
        LeaderRole::Follower => {}
    }
    drop(lease);
}

/// Test 3: when the leader's lease drops, the next acquirer becomes leader.
/// CAS-release is fired asynchronously on drop, so we poll briefly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lease_drop_releases_lock() {
    let (url, _container) = redis_url().await;
    let elector = Arc::new(
        RedisCacheLeaderElector::from_url(&url, opts_short_ttl(60)).expect("build elector"),
    );
    let key = format!("test3-{}", uuid::Uuid::new_v4());

    let role = elector.acquire(&key).await.expect("first acquire ok");
    let lease = match role {
        LeaderRole::Leader(l) => l,
        LeaderRole::Follower => panic!("first must be leader"),
    };
    drop(lease);

    // Poll for up to 5s for the detached release task to fire.
    let mut became_leader = false;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        match elector.acquire(&key).await.expect("acquire ok") {
            LeaderRole::Leader(_) => {
                became_leader = true;
                break;
            }
            LeaderRole::Follower => continue,
        }
    }
    assert!(
        became_leader,
        "next acquirer should become leader after first lease drops"
    );
}

/// Test 4: if the leader process dies without releasing (we simulate by
/// std::mem::forget on the lease), the lock auto-expires after TTL.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ttl_safety_net_releases_after_expiry() {
    let (url, _container) = redis_url().await;
    let leader = Arc::new(
        RedisCacheLeaderElector::from_url(&url, opts_short_ttl(2)).expect("build elector"),
    );
    let challenger = Arc::new(
        // Same short TTL; we don't want our own heartbeat to extend leadership.
        RedisCacheLeaderElector::from_url(&url, opts_short_ttl(2)).expect("build elector"),
    );
    let key = format!("test4-{}", uuid::Uuid::new_v4());

    let role = leader.acquire(&key).await.expect("acquire ok");
    let lease = match role {
        LeaderRole::Leader(l) => l,
        LeaderRole::Follower => panic!("first must be leader"),
    };

    // To validate the TTL safety net (rather than the normal Drop path),
    // we drop our real lease and then write a *ghost* lock directly via
    // raw SET. Because the token is not ours, no heartbeat will extend
    // it, so we observe Redis's own TTL expiry.
    drop(lease);

    // Manually emit a SET with our TTL using the redis crate directly.
    let client = redis::Client::open(url.clone()).expect("redis client");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("redis conn");
    let manual_id = uuid::Uuid::new_v4();
    let manual_key = format!("broccoli:cache-leader:test4-manual-{manual_id}");
    let _: () = redis::cmd("SET")
        .arg(&manual_key)
        .arg("ghost-token")
        .arg("EX")
        .arg(2u64)
        .query_async(&mut conn)
        .await
        .expect("manual SET");

    // Immediately challenging should see Follower (key present, our token != ghost).
    let cache_key = manual_key.trim_start_matches("broccoli:cache-leader:");
    let immediate = challenger.acquire(cache_key).await.expect("acquire ok");
    assert!(
        matches!(immediate, LeaderRole::Follower),
        "ghost lock present → next acquire must be Follower"
    );

    // After TTL elapses (give a 1s slack), next acquire must become leader.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = challenger.acquire(cache_key).await.expect("acquire ok");
    match after {
        LeaderRole::Leader(_) => {}
        LeaderRole::Follower => panic!("ghost lock should have expired after TTL"),
    }
}

/// Counting sandbox decorator - delegates every `SandboxManager` method to an
/// inner `MockSandboxManager`, but bumps a shared atomic on `execute`. Used by
/// test 5 to assert exactly one of the three handlers actually ran the step.
struct CountingSandboxManager {
    inner: MockSandboxManager,
    exec_count: Arc<AtomicUsize>,
}

#[async_trait]
impl SandboxManager for CountingSandboxManager {
    async fn create_sandbox(&self, id: Option<&str>) -> Result<PathBuf, SandboxError> {
        self.inner.create_sandbox(id).await
    }

    async fn remove_sandbox(&self, id: &str) -> Result<(), SandboxError> {
        self.inner.remove_sandbox(id).await
    }

    async fn execute(
        &self,
        box_id: &str,
        argv: Vec<String>,
        run_options: &RunOptions,
    ) -> Result<worker::models::operation::sandbox::ExecutionResult, SandboxError> {
        self.exec_count.fetch_add(1, Ordering::SeqCst);
        self.inner.execute(box_id, argv, run_options).await
    }
}

async fn pg_url() -> (String, testcontainers::ContainerAsync<Postgres>) {
    let container = Postgres::default()
        .start()
        .await
        .expect("failed to start Postgres container");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("failed to get Postgres port");
    // testcontainers-modules Postgres defaults: db=postgres user=postgres password=postgres
    let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
    (url, container)
}

/// Build one `OperationHandler` for test 5 - three of these get the same
/// `task_cache`, the same `BlobStore` Arc (shared across each handler's
/// own `BlobStoreFileCacher`), the same shared `exec_count` (via
/// `CountingSandboxManager`), and the same `toolchain_fingerprint` (so the
/// computed cache_key is identical across handlers). Each handler gets its
/// own `RedisCacheLeaderElector` instance - that's what the real fleet
/// looks like.
async fn build_test_handler(
    redis_url: &str,
    blob_store: Arc<dyn BlobStore>,
    task_cache: Arc<dyn TaskCacheStore>,
    exec_count: Arc<AtomicUsize>,
    mock_base: PathBuf,
    file_cacher_dir: &Path,
) -> OperationHandler {
    let (metrics, _registry) =
        common::observability::init_metrics("broccoli-worker-test-cache-leader");
    let file_cacher = BlobStoreFileCacher::new(
        blob_store,
        file_cacher_dir.to_path_buf(),
        64 * 1024 * 1024,
        None,
    )
    .await
    .expect("build file cacher");
    let sandbox = CountingSandboxManager {
        inner: MockSandboxManager::new(mock_base),
        exec_count,
    };
    let elector = Arc::new(
        RedisCacheLeaderElector::from_url(redis_url, opts_short_ttl(30))
            .expect("build redis elector"),
    );
    OperationHandler::with_compile_cache(
        Box::new(sandbox),
        Arc::new(file_cacher),
        CompileCache {
            store: task_cache,
            leader: elector,
            follower_poll_interval: Duration::from_millis(100),
            follower_max_wait: Duration::from_secs(20),
            // Identical fingerprint across all 3 handlers -> identical cache_key.
            toolchain_fingerprint: worker::toolchain_fingerprint::ToolchainFingerprint::Known(
                "deterministic-test-fingerprint".to_string(),
            )
            .into(),
        },
        metrics,
    )
}

/// Test 5: end-to-end multi-worker concurrent-compile.
///
/// Three `OperationHandler`s share a real Postgres-backed `task_cache`, a
/// real filesystem `BlobStore`, and a shared `exec_count` atomic; each has
/// its own Redis-backed leader-elector pointed at the same Redis instance.
/// They concurrently `execute()` the same one-step `OperationTask` (a cached
/// "compile" that sleeps 500ms then writes `build.out`). At the end:
///
/// 1. The shared sandbox executed exactly once.
/// 2. All three handlers returned success with the same collected `build.out`.
/// 3. Followers' results match the leader's (i.e., they read the cached
///    blob back rather than producing their own).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn end_to_end_concurrent_compile_runs_once() {
    // Spin up containers concurrently to halve setup latency.
    let ((redis_url, _redis_container), (pg_url_str, _pg_container)) =
        tokio::join!(redis_url(), pg_url());

    // Shared DB. The server's migrations own `task_cache`; this test creates
    // the table from its entity.
    let db: common::db::DatabaseConnection = Database::connect(&pg_url_str)
        .await
        .expect("connect Postgres")
        .into();
    {
        use sea_orm::ConnectionTrait;
        let table = sea_orm::Schema::new(db.get_database_backend())
            .create_table_from_entity(worker::models::operation::task_cache::task_cache::Entity);
        db.execute(&table).await.expect("create task_cache");
    }
    let task_cache: Arc<dyn TaskCacheStore> = Arc::new(DatabaseTaskCacheStore::new(db.clone()));

    // Shared filesystem-backed BlobStore. Per-handler local cache_dirs sit
    // under separate tempdirs to avoid LRU eviction races between handlers.
    let blob_root = tempfile::tempdir().expect("blob tempdir");
    let blob_store: Arc<dyn BlobStore> = Arc::new(
        FilesystemBlobStore::new(blob_root.path().to_path_buf(), 64 * 1024 * 1024)
            .await
            .expect("build filesystem blob store"),
    );

    let exec_count = Arc::new(AtomicUsize::new(0));

    // Each handler gets its own sandbox base_dir and its own file_cacher
    // local cache_dir - the *shared* state is `blob_store`, `task_cache`,
    // and `exec_count`. This mirrors a real fleet: separate worker processes
    // with separate local scratch, coordinating via Redis + the DB cache.
    let cacher_dirs: Vec<tempfile::TempDir> = (0..3)
        .map(|_| tempfile::tempdir().expect("file_cacher tempdir"))
        .collect();
    let mock_bases: Vec<tempfile::TempDir> = (0..3)
        .map(|_| tempfile::tempdir().expect("mock_sandbox tempdir"))
        .collect();

    let mut handlers = Vec::new();
    for i in 0..3 {
        handlers.push(
            build_test_handler(
                &redis_url,
                blob_store.clone(),
                task_cache.clone(),
                exec_count.clone(),
                mock_bases[i].path().to_path_buf(),
                cacher_dirs[i].path(),
            )
            .await,
        );
    }

    // Build the cached operation - one step that "compiles" by sleeping
    // 500ms and producing build.out. The sleep ensures handler B / C's
    // SETNX losses arrive while A is still inside `execute_step`, so the
    // follower path is actually exercised (rather than the trivial path
    // where A finishes microseconds after acquiring and B/C race in to
    // find an already-cached result via try_cache_hit).
    let operation = OperationTask {
        environments: vec![Environment {
            id: "env-1".to_string(),
            files_in: vec![(
                "src.txt".to_string(),
                SessionFile::Content {
                    content: "deterministic-source-bytes".to_string(),
                },
            )],
        }],
        tasks: vec![Step {
            id: "compile".to_string(),
            kind: StepKind::Compile,
            env_ref: "env-1".to_string(),
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 0.5 && echo built > build.out".to_string(),
            ],
            conf: RunOptions::default(),
            io: IOConfig::default(),
            collect: vec!["build.out".to_string()],
            depends_on: vec![],
            cache: Some(StepCacheConfig {
                key_inputs: vec!["src.txt".to_string()],
                outputs: vec!["build.out".to_string()],
            }),
            mounts: vec![],
        }],
        channels: vec![],
        target_worker_id: None,
        evaluate_batch_id: None,
        test_case_id: None,
    };

    // Wrap each handler so the spawn closure can move-borrow it. Operation
    // is small, just clone it per task.
    let handlers: Vec<Arc<OperationHandler>> = handlers.into_iter().map(Arc::new).collect();
    let mut tasks = Vec::new();
    for h in &handlers {
        let h = h.clone();
        let op = operation.clone();
        tasks.push(tokio::spawn(async move { h.execute(&op).await }));
    }
    let results = join_all(tasks).await;

    // 1. All three succeeded.
    let mut successes = 0;
    let mut collected_outputs_per_handler = Vec::new();
    for r in results {
        let op_result = r
            .expect("handler task panicked")
            .expect("OperationHandler::execute returned Err");
        assert!(op_result.success, "handler failed: {op_result:?}");
        successes += 1;
        let step = op_result
            .task_results
            .get("compile")
            .expect("compile step result");
        assert!(step.success, "compile step failed: {step:?}");
        collected_outputs_per_handler.push(step.collected_outputs.clone());
    }
    assert_eq!(successes, 3, "all 3 handlers should succeed");

    // 2. Exactly one sandbox.execute() was called across all 3 handlers -
    //    this is the property leader election exists to guarantee.
    assert_eq!(
        exec_count.load(Ordering::SeqCst),
        1,
        "exactly one of the 3 handlers should have invoked the sandbox",
    );

    // 3. All three handlers returned the same content_hash for build.out
    //    (i.e., followers read back the leader's cached output rather than
    //    producing their own).
    let first = &collected_outputs_per_handler[0];
    let leader_hash = first
        .get("build.out")
        .expect("collected_outputs must contain build.out");
    for outputs in &collected_outputs_per_handler[1..] {
        let h = outputs
            .get("build.out")
            .expect("follower must also surface build.out");
        assert_eq!(
            h, leader_hash,
            "follower's build.out hash must match leader's",
        );
    }
}
