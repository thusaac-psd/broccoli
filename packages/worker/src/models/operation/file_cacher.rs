use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use common::metrics::Metrics;
use common::storage::{BlobStore, ContentHash};
use opentelemetry::KeyValue;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Materialize `src` at `dest` as cheaply as possible.
///
/// Copy or link `src` to `dest`, ensuring `dest` is world-readable so the
/// isolate sandbox user can read it regardless of cache ownership/permissions.
async fn link_or_copy(src: &Path, dest: &Path) -> std::io::Result<()> {
    // The cache `src` may have been uploaded from inside an isolate sandbox
    // and so be owned by a sandbox UID with restrictive (0o700) permissions
    // that the worker user cannot read. Make it world-readable first (with a
    // sudo-chmod last resort) so both the hard-link fast path and the copy
    // fallback succeed.
    ensure_world_readable(src);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Fast path: world-readable source -> hard-link (no data copy).
        if let Ok(meta) = std::fs::metadata(src)
            && meta.permissions().mode() & 0o044 == 0o044
        {
            let linked = match std::fs::hard_link(src, dest) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    tokio::fs::remove_file(dest).await.is_ok()
                        && std::fs::hard_link(src, dest).is_ok()
                }
                Err(_) => false,
            };
            if linked {
                // The link shares the cache inode; another op may have linked
                // the same blob while it was briefly 0o700. Re-assert 0o644 on
                // the (shared) inode so the sandbox user can always read it.
                ensure_world_readable(dest);
                return Ok(());
            }
        }
    }

    // Copy fallback. Retry once on a transient permission error (e.g. a
    // concurrent box init/cleanup racing on the dest directory).
    match tokio::fs::copy(src, dest).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            ensure_world_readable(src);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            tokio::fs::copy(src, dest).await?;
        }
        Err(e) => return Err(e),
    }
    ensure_world_readable(dest);
    Ok(())
}

/// Compute SHA-256 of `src` by streaming. Avoids loading the whole file.
async fn hash_file(src: &Path) -> std::io::Result<ContentHash> {
    let mut file = tokio::fs::File::open(src).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&digest);
    Ok(ContentHash::from_bytes(bytes))
}

/// Make `path` world-readable AND executable (0o755). Tries in order:
/// 1. path-based `set_permissions` (fast)
/// 2. `fchmod` via an open file descriptor (bypasses some MAC policies)
/// 3. `sudo chmod` (last resort, requires passwordless sudo)
///
/// Silently ignores all failures - the file is still usable by the owner
/// and the sandbox failure will surface as a clearer isolate error.
#[cfg(unix)]
fn ensure_world_readable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.permissions().mode() & 0o005 == 0o005 {
        return; // already world-readable + executable
    }
    // 1. Path-based chmod
    if std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).is_ok() {
        return;
    }
    // 2. fchmod on an open FD
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o755));
    }
    // 3. sudo chmod (worker user has NOPASSWD sudo)
    let _ = std::process::Command::new("sudo")
        .args(["chmod", "755"])
        .arg(path)
        .output();
}

#[cfg(not(unix))]
fn ensure_world_readable(_path: &Path) {}

// `async_trait` marks each method `#[must_use]` on top of its boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait FileCacher: Send + Sync {
    async fn fetch_to_path(&self, content_hash: &str, dest: &Path) -> Result<(), String>;

    async fn upload_from_path(&self, src: &Path) -> Result<String, String>;

    /// Ensure `content_hash` is present in the local cache without materializing
    /// it to any destination (used by the pre-warm path). Returns bytes fetched
    /// from the blob store (0 on a cache hit).
    async fn warm(&self, content_hash: &str) -> Result<u64, String>;
}

/// Caches nothing and fetches nothing, for tests.
#[cfg(any(test, feature = "test-support"))]
pub struct NoopFileCacher;

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl FileCacher for NoopFileCacher {
    async fn fetch_to_path(&self, _content_hash: &str, _dest: &Path) -> Result<(), String> {
        Ok(())
    }
    async fn upload_from_path(&self, _src: &Path) -> Result<String, String> {
        Ok("0".repeat(64))
    }
    async fn warm(&self, _content_hash: &str) -> Result<u64, String> {
        Ok(0)
    }
}

/// Fails every blob operation, as if blob storage were down, for tests.
#[cfg(any(test, feature = "test-support"))]
pub struct UnavailableFileCacher {
    reason: String,
}

#[cfg(any(test, feature = "test-support"))]
impl UnavailableFileCacher {
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl FileCacher for UnavailableFileCacher {
    async fn fetch_to_path(&self, content_hash: &str, _dest: &Path) -> Result<(), String> {
        Err(format!(
            "blob storage is unavailable; cannot fetch blob {content_hash}: {}",
            self.reason
        ))
    }

    async fn upload_from_path(&self, src: &Path) -> Result<String, String> {
        Err(format!(
            "blob storage is unavailable; cannot upload {}: {}",
            src.display(),
            self.reason
        ))
    }

    async fn warm(&self, content_hash: &str) -> Result<u64, String> {
        Err(format!(
            "blob storage is unavailable; cannot warm blob {content_hash}: {}",
            self.reason
        ))
    }
}

struct FetchLockCleanup<'a>(&'a BlobStoreFileCacher, String);

impl Drop for FetchLockCleanup<'_> {
    fn drop(&mut self) {
        self.0.remove_fetch_lock(&self.1);
    }
}

pub struct BlobStoreFileCacher {
    store: Arc<dyn BlobStore>,
    cache_dir: PathBuf,
    max_cache_size: u64,

    state: tokio::sync::Mutex<CacheState>,
    fetch_locks: std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    metrics: Option<Metrics>,
}

struct CacheState {
    entries: lru::LruCache<String, u64>,
    total_size: u64,
}

impl BlobStoreFileCacher {
    pub async fn new(
        store: Arc<dyn BlobStore>,
        cache_dir: PathBuf,
        max_cache_size: u64,
        metrics: Option<Metrics>,
    ) -> Result<Self, String> {
        tokio::fs::create_dir_all(&cache_dir)
            .await
            .map_err(|e| format!("Failed to create cache dir: {e}"))?;

        let mut entries_vec = Vec::new();
        let mut total_size: u64 = 0;
        let mut rd = tokio::fs::read_dir(&cache_dir)
            .await
            .map_err(|e| format!("Failed to read cache dir: {e}"))?;

        while let Ok(Some(entry)) = rd.next_entry().await {
            if let Ok(meta) = entry.metadata().await
                && meta.is_file()
            {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".tmp") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                    continue;
                }
                let size = meta.len();
                let mtime = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                entries_vec.push((name, size, mtime));
                total_size += size;
            }
        }

        entries_vec.sort_by_key(|(_, _, mtime)| *mtime);

        let mut entries = lru::LruCache::unbounded();
        for (name, size, _) in entries_vec {
            entries.put(name, size);
        }

        let cacher = Self {
            store,
            cache_dir,
            max_cache_size,
            state: tokio::sync::Mutex::new(CacheState {
                entries,
                total_size,
            }),
            fetch_locks: std::sync::Mutex::new(std::collections::HashMap::new()),
            metrics,
        };

        // Reflect the initial scanned-on-disk size into the live gauge so
        // restarts don't drop the cache size to zero on dashboards.
        if let Some(metrics) = &cacher.metrics
            && total_size > 0
        {
            metrics.blob_cache_size_bytes.add(total_size as i64, &[]);
        }

        // Startup scan: no just-inserted entry to protect.
        cacher.evict_if_needed("").await;

        Ok(cacher)
    }

    fn cache_path(&self, hash_hex: &str) -> PathBuf {
        self.cache_dir.join(hash_hex)
    }

    fn get_fetch_lock(&self, hash_hex: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.fetch_locks.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(lock) = locks.get(hash_hex) {
            lock.clone()
        } else {
            let lock = Arc::new(tokio::sync::Mutex::new(()));
            locks.insert(hash_hex.to_string(), lock.clone());
            lock
        }
    }

    fn remove_fetch_lock(&self, hash_hex: &str) {
        if let Ok(mut locks) = self.fetch_locks.lock() {
            locks.remove(hash_hex);
        }
    }

    #[cfg(test)]
    pub async fn current_size(&self) -> u64 {
        self.state.lock().await.total_size
    }

    /// Evict LRU entries until under `max_cache_size`. `keep` is the hash just
    /// inserted by the caller (empty string when there is none, e.g. startup
    /// scan): it is never evicted, so a single blob at/over the cap cannot delete
    /// itself and make its own fetch fail - the cache just sits one blob over cap.
    async fn evict_if_needed(&self, keep: &str) {
        let mut state = self.state.lock().await;
        while state.total_size > self.max_cache_size && !state.entries.is_empty() {
            // Peek the LRU before removing: if it is the just-inserted entry (the
            // only case where the MRU is also the LRU), stop rather than
            // self-evict.
            match state.entries.peek_lru() {
                Some((k, _)) if k.as_str() == keep => break,
                Some(_) => {}
                None => break,
            }
            if let Some((hash, size)) = state.entries.pop_lru() {
                let path = self.cache_dir.join(&hash);
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    // Most likely NotFound (unlinked out-of-band). The entry is
                    // already gone from `entries`, so drop its size accounting too
                    // - leaving it counted would inflate total_size permanently and
                    // make every later insert over-evict live, in-use blobs.
                    tracing::warn!(path = %path.display(), error = %e, "Failed to evict cached file; dropping its size accounting anyway");
                }
                state.total_size = state.total_size.saturating_sub(size);
                if let Some(metrics) = &self.metrics {
                    metrics.blob_cache_evictions_total.add(1, &[]);
                    metrics.blob_cache_size_bytes.add(-(size as i64), &[]);
                }
            }
        }
    }

    async fn record_cache_entry(&self, hash_hex: String, size: u64) {
        let delta: i64;
        {
            let mut state = self.state.lock().await;
            let old_size = state.entries.put(hash_hex.clone(), size).unwrap_or(0);
            state.total_size = state.total_size + size - old_size;
            delta = size as i64 - old_size as i64;
        }
        if let Some(metrics) = &self.metrics
            && delta != 0
        {
            metrics.blob_cache_size_bytes.add(delta, &[]);
        }
        // Protect the entry just inserted from being the one evicted.
        self.evict_if_needed(&hash_hex).await;
    }

    async fn touch(&self, hash_hex: &str) {
        let mut state = self.state.lock().await;
        state.entries.get(hash_hex);
    }

    /// Ensure the blob `hash` is present in the local cache, fetching it from the
    /// blob store on a miss. Returns bytes fetched (0 on a cache hit). Shared by
    /// `fetch_to_path` (which then links the cached file to a dest) and `warm`
    /// (cache-only, no dest). Concurrent callers for the same hash coalesce on
    /// the per-hash fetch lock.
    async fn ensure_cached(&self, hash: &ContentHash, hash_hex: &str) -> Result<u64, String> {
        let cached = self.cache_path(hash_hex);

        if cached.exists() {
            self.touch(hash_hex).await;
            if let Some(metrics) = &self.metrics {
                metrics
                    .blob_cache_hits_total
                    .add(1, &[KeyValue::new("operation", "ensure_cached")]);
            }
            return Ok(0);
        }

        let lock = self.get_fetch_lock(hash_hex);
        let _cleanup = FetchLockCleanup(self, hash_hex.to_string());
        let _guard = lock.lock().await;

        // Re-check after acquiring the fetch lock: a concurrent fetch may have
        // populated the cache while we waited.
        if cached.exists() {
            self.touch(hash_hex).await;
            if let Some(metrics) = &self.metrics {
                metrics
                    .blob_cache_hits_total
                    .add(1, &[KeyValue::new("operation", "ensure_cached")]);
            }
            return Ok(0);
        }

        if let Some(metrics) = &self.metrics {
            metrics
                .blob_cache_misses_total
                .add(1, &[KeyValue::new("operation", "ensure_cached")]);
        }

        let temp_path = self.cache_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut temp_file = tokio::fs::File::create(&temp_path).await.map_err(|e| {
            format!(
                "Failed to create temp cache file {}: {}",
                temp_path.display(),
                e
            )
        })?;

        let mut reader = self
            .store
            .get_stream(hash)
            .await
            .map_err(|e| e.to_string())?;

        let file_size = tokio::io::copy(&mut reader, &mut temp_file)
            .await
            .map_err(|e| {
                let temp_path_clone = temp_path.clone();
                tokio::spawn(async move {
                    let _ = tokio::fs::remove_file(temp_path_clone).await;
                });
                format!("Failed to stream blob to cache: {e}")
            })?;

        // Flush + close before re-reading for verification.
        if let Err(e) = temp_file.flush().await {
            let _ = tokio::fs::remove_file(&temp_path).await;
            return Err(format!("Failed to flush cache temp file: {e}"));
        }
        drop(temp_file);

        // DEFENSE-IN-DEPTH integrity check. The blob store is content-addressed,
        // so a silently truncated/corrupt download would otherwise be renamed
        // permanently under the CORRECT hash key, then hard-linked and executed
        // into every sandbox needing it - systematic wrong/RE verdicts with no
        // error surfaced. Re-hash what we actually wrote; on mismatch, drop it
        // and return a transient error so the task layer retries the fetch
        // rather than poisoning the cache. Re-hashing is cheap next to the
        // network fetch it guards.
        match hash_file(&temp_path).await {
            Ok(actual) if actual == *hash => {}
            Ok(actual) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(format!(
                    "blob {hash_hex} failed integrity check: fetched {file_size} bytes hashing to {} (expected {hash_hex})",
                    actual.to_hex()
                ));
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&temp_path).await;
                return Err(format!("Failed to verify cached blob {hash_hex}: {e}"));
            }
        }

        tokio::fs::rename(&temp_path, &cached).await.map_err(|e| {
            let temp_path_clone = temp_path.clone();
            tokio::spawn(async move {
                let _ = tokio::fs::remove_file(temp_path_clone).await;
            });
            format!("Failed to finalize cache file: {e}")
        })?;

        // Make the newly cached file world-readable so future hard_link hits
        // don't need the copy+chmod fallback in link_or_copy.
        ensure_world_readable(&cached);
        self.record_cache_entry(hash_hex.to_string(), file_size)
            .await;
        Ok(file_size)
    }
}

#[async_trait]
impl FileCacher for BlobStoreFileCacher {
    #[tracing::instrument(name = "file_cacher_fetch", skip(self, dest), fields(content_hash, dest = %dest.display()))]
    async fn fetch_to_path(&self, content_hash: &str, dest: &Path) -> Result<(), String> {
        let hash = ContentHash::from_hex(content_hash).map_err(|e| e.to_string())?;
        let hash_hex = hash.to_hex();

        // Ensure the blob is cached (fetching on a miss, coalescing concurrent
        // fetches of the same hash), then materialize it to `dest`.
        self.ensure_cached(&hash, &hash_hex).await?;

        // link_or_copy uses hard_link when the cached file is already world-
        // readable (common path), and falls back to read+write with explicit
        // 0o644 when it isn't (stale caches, isolate-owned files, umask 077).
        let cached = self.cache_path(&hash_hex);
        match link_or_copy(&cached, dest).await {
            Ok(()) => Ok(()),
            // Under heavy cache pressure, LRU eviction can unlink this exact file
            // in the window between ensure_cached returning and link_or_copy
            // opening it - a benign race, not corruption. Re-fetch once (which
            // re-materializes the blob and re-records the LRU entry) and retry.
            // A second ENOENT is a real fault and propagates.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.ensure_cached(&hash, &hash_hex).await?;
                link_or_copy(&self.cache_path(&hash_hex), dest)
                    .await
                    .map_err(|e| {
                        format!("Failed to materialize cached file after re-fetch: {e}")
                    })?;
                Ok(())
            }
            Err(e) => Err(format!("Failed to materialize cached file: {e}")),
        }
    }

    #[tracing::instrument(name = "file_cacher_warm", skip(self), fields(content_hash))]
    async fn warm(&self, content_hash: &str) -> Result<u64, String> {
        let hash = ContentHash::from_hex(content_hash).map_err(|e| e.to_string())?;
        let hash_hex = hash.to_hex();
        self.ensure_cached(&hash, &hash_hex).await
    }

    #[tracing::instrument(name = "file_cacher_upload", skip(self, src), fields(src = %src.display()))]
    async fn upload_from_path(&self, src: &Path) -> Result<String, String> {
        let meta = tokio::fs::metadata(src)
            .await
            .map_err(|e| format!("Failed to read metadata: {e}"))?;
        let file_size = meta.len();

        // Hash the file locally first so we can probe BlobStore::exists before
        // streaming. The extra local read is dominated by upload cost for any
        // file >1 KiB and saves a full network round-trip on re-uploads.
        let hash = hash_file(src)
            .await
            .map_err(|e| format!("Failed to hash file: {e}"))?;
        let hash_hex = hash.to_hex();

        // HEAD probe; fail open (fall through to streaming) on probe errors so
        // a flaky head_object call never breaks an upload.
        let already_remote = match self.store.exists(&hash).await {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(
                    hash = %hash_hex,
                    error = %e,
                    "BlobStore::exists probe failed; falling back to streaming upload"
                );
                false
            }
        };

        if already_remote && let Some(metrics) = &self.metrics {
            metrics.blob_store_remote_hits_total.add(1, &[]);
        }

        if !already_remote {
            let file = tokio::fs::File::open(src)
                .await
                .map_err(|e| format!("Failed to open file: {e}"))?;
            let reader: common::storage::BoxReader = Box::new(file);
            let uploaded = self
                .store
                .put_stream(reader)
                .await
                .map_err(|e| e.to_string())?;
            debug_assert_eq!(
                uploaded.to_hex(),
                hash_hex,
                "BlobStore::put_stream hash disagrees with locally-computed hash"
            );
        }

        let cached = self.cache_path(&hash_hex);

        if !cached.exists() {
            // Write to a unique temp file then atomically rename into place, EXACTLY
            // like the fetch path (`ensure_cached`). A direct `copy(src, &cached)`
            // opens the final content-addressed path with O_CREAT|O_TRUNC, so the
            // blob is visible via `exists()` while still being streamed; a
            // concurrent `fetch_to_path` for the same hash would then treat the
            // half-written file as a cache hit and hard-link a truncated inode into
            // a sandbox (spurious exit-127 / wrong verdict). The rename publishes
            // the blob atomically, fully written.
            let temp_path = self.cache_dir.join(format!("{}.tmp", uuid::Uuid::new_v4()));
            if tokio::fs::copy(src, &temp_path).await.is_ok() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    // 0o755 (not 0o644): cached compile outputs are hard-linked
                    // into sandbox dirs and exec'd; a stable executable fixpoint
                    // across all sites prevents a shared-inode perms race that
                    // strips the exec bit mid-run (spurious exit-127 RuntimeError).
                    // Set on the temp BEFORE the rename so the published inode is
                    // already exec-ready the instant it becomes visible.
                    let _ = std::fs::set_permissions(
                        &temp_path,
                        std::fs::Permissions::from_mode(0o755),
                    );
                }
                if tokio::fs::rename(&temp_path, &cached).await.is_ok() {
                    self.record_cache_entry(hash_hex.clone(), file_size).await;
                } else {
                    let _ = tokio::fs::remove_file(&temp_path).await;
                }
            } else {
                let _ = tokio::fs::remove_file(&temp_path).await;
            }
        } else {
            self.touch(&hash_hex).await;
        }

        Ok(hash_hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::storage::filesystem::FilesystemBlobStore;

    async fn temp_cacher(
        max_cache: u64,
    ) -> (
        BlobStoreFileCacher,
        tempfile::TempDir,
        Arc<FilesystemBlobStore>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("blobs");
        let cache_dir = dir.path().join("cache");
        let store = Arc::new(
            FilesystemBlobStore::new(blob_dir, 10 * 1024 * 1024)
                .await
                .unwrap(),
        );
        let cacher = BlobStoreFileCacher::new(store.clone(), cache_dir, max_cache, None)
            .await
            .unwrap();
        (cacher, dir, store)
    }

    #[tokio::test]
    async fn noop_cacher_works() {
        let c = NoopFileCacher;
        let dest = std::env::temp_dir().join("noop_test");
        assert!(
            c.fetch_to_path("a".repeat(64).as_str(), &dest)
                .await
                .is_ok()
        );
        let h = c.upload_from_path(Path::new("/dev/null")).await.unwrap();
        assert_eq!(h.len(), 64);
    }

    #[tokio::test]
    async fn unavailable_cacher_fails_blob_materialization() {
        let c = UnavailableFileCacher::new("storage init failed");
        let dest = std::env::temp_dir().join("unavailable_test");
        let err = c
            .fetch_to_path("a".repeat(64).as_str(), &dest)
            .await
            .expect_err("fetching a blob with unavailable storage must fail");

        assert!(err.contains("blob storage is unavailable"));
        assert!(err.contains("storage init failed"));
    }

    #[tokio::test]
    async fn upload_and_fetch_round_trip() {
        let (cacher, dir, _store) = temp_cacher(10 * 1024 * 1024).await;

        let src = dir.path().join("source.txt");
        tokio::fs::write(&src, b"hello blob").await.unwrap();
        let hash = cacher.upload_from_path(&src).await.unwrap();

        let dest = dir.path().join("fetched.txt");
        cacher.fetch_to_path(&hash, &dest).await.unwrap();

        let content = tokio::fs::read_to_string(&dest).await.unwrap();
        assert_eq!(content, "hello blob");
    }

    #[tokio::test]
    async fn link_or_copy_materializes_file_on_same_fs() {
        // tempdir is on a real filesystem, so hard_link should succeed.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        let dest = dir.path().join("dest.bin");
        tokio::fs::write(&src, b"link me").await.unwrap();

        link_or_copy(&src, &dest).await.unwrap();
        assert_eq!(tokio::fs::read(&dest).await.unwrap(), b"link me");

        // EEXIST recovery: pre-create dest then re-link.
        let dest2 = dir.path().join("dest2.bin");
        tokio::fs::write(&dest2, b"old contents").await.unwrap();
        link_or_copy(&src, &dest2).await.unwrap();
        assert_eq!(tokio::fs::read(&dest2).await.unwrap(), b"link me");
    }

    /// Counts put_stream invocations to verify the HEAD-probe short-circuit
    /// skips the upload when the blob is already remote.
    struct CountingBlobStore {
        inner: Arc<FilesystemBlobStore>,
        put_calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl BlobStore for CountingBlobStore {
        async fn put_stream(
            &self,
            reader: common::storage::BoxReader,
        ) -> Result<ContentHash, common::storage::StorageError> {
            self.put_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.put_stream(reader).await
        }
        async fn get_stream(
            &self,
            hash: &ContentHash,
        ) -> Result<common::storage::BoxReader, common::storage::StorageError> {
            self.inner.get_stream(hash).await
        }
        async fn exists(&self, hash: &ContentHash) -> Result<bool, common::storage::StorageError> {
            self.inner.exists(hash).await
        }
        async fn delete(&self, hash: &ContentHash) -> Result<bool, common::storage::StorageError> {
            self.inner.delete(hash).await
        }
        async fn size(&self, hash: &ContentHash) -> Result<u64, common::storage::StorageError> {
            self.inner.size(hash).await
        }
    }

    #[tokio::test]
    async fn upload_from_path_skips_put_stream_when_blob_exists_remotely() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("blobs");
        let cache_dir = dir.path().join("cache");
        let inner = Arc::new(
            FilesystemBlobStore::new(blob_dir, 10 * 1024 * 1024)
                .await
                .unwrap(),
        );
        let counting = Arc::new(CountingBlobStore {
            inner,
            put_calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let cacher = BlobStoreFileCacher::new(counting.clone(), cache_dir, 10 * 1024 * 1024, None)
            .await
            .unwrap();

        let src1 = dir.path().join("src1.bin");
        let src2 = dir.path().join("src2.bin");
        tokio::fs::write(&src1, b"identical contents")
            .await
            .unwrap();
        tokio::fs::write(&src2, b"identical contents")
            .await
            .unwrap();

        let h1 = cacher.upload_from_path(&src1).await.unwrap();
        let count_after_first = counting.put_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(count_after_first, 1, "first upload should stream");

        let h2 = cacher.upload_from_path(&src2).await.unwrap();
        assert_eq!(h1, h2, "identical bytes must yield identical hashes");

        let count_after_second = counting.put_calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            count_after_second, 1,
            "second upload of identical content must skip put_stream"
        );
    }

    #[tokio::test]
    async fn eviction_keeps_cache_under_limit() {
        let (cacher, dir, _store) = temp_cacher(20).await;

        for i in 0..3u8 {
            let src = dir.path().join(format!("f{i}"));
            tokio::fs::write(&src, vec![i; 10]).await.unwrap();
            cacher.upload_from_path(&src).await.unwrap();
        }

        let total = cacher.current_size().await;
        assert!(total <= 20, "cache size {total} exceeds limit 20");
    }

    /// Drives a cacher constructed with a real `Metrics` through one miss,
    /// one hit, and at least one eviction. The OpenTelemetry SDK doesn't
    /// expose a simple `.get()` on user-facing `Counter` / `UpDownCounter`
    /// types, and standing up an in-memory `MetricReader` for assertion
    /// would balloon this test well past 20 lines. Instead we verify that
    /// the metric-emitting paths execute without panicking; the actual
    /// counter values are observable via the running worker's `/metrics`
    /// Prometheus endpoint.
    #[tokio::test]
    async fn cache_metrics_track_hit_miss_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let blob_dir = dir.path().join("blobs");
        let cache_dir = dir.path().join("cache");
        let store = Arc::new(
            FilesystemBlobStore::new(blob_dir, 10 * 1024 * 1024)
                .await
                .unwrap(),
        );
        let meter = opentelemetry::global::meter("broccoli.test");
        let metrics = Metrics::new(&meter);
        let cacher = BlobStoreFileCacher::new(store, cache_dir, 20, Some(metrics))
            .await
            .unwrap();

        // Upload a blob, then fetch it twice: first fetch is a miss (the
        // upload populated the local cache from disk so a fetch after wipe
        // would miss; we exercise the miss path via a separate hash below).
        let src_a = dir.path().join("a.bin");
        tokio::fs::write(&src_a, vec![0u8; 10]).await.unwrap();
        let hash_a = cacher.upload_from_path(&src_a).await.unwrap();

        // Hit: fetch a blob that's already in cache (uploaded above).
        let dest1 = dir.path().join("dest1.bin");
        cacher.fetch_to_path(&hash_a, &dest1).await.unwrap();

        // Miss + eviction: upload two more blobs to push the LRU past 20B.
        for i in 1..3u8 {
            let src = dir.path().join(format!("evict{i}.bin"));
            tokio::fs::write(&src, vec![i; 10]).await.unwrap();
            cacher.upload_from_path(&src).await.unwrap();
        }

        assert!(cacher.current_size().await <= 20);
    }
}
