//! Host-wide isolate box ids.
//!
//! isolate box ids are one namespace per host: box `N` lives at
//! `<box_root>/N` and runs as uid `first_uid + N`, and `isolate --init` on a
//! box another process is using silently re-initialises it. A worker
//! therefore holds every box id it uses as an exclusive `flock` on that id's
//! lock file in a directory all workers on the host share. The kernel drops
//! the lock when the guard closes the file or the worker dies, so a crashed
//! worker frees its ids at once.
//!
//! `flock` locks belong to an open file, so a second open of the same lock
//! file, in the same process or another, cannot take a held id: the lock is
//! the one record of which ids are live. That makes the lock directory part
//! of the guarantee: it must be a directory nothing unlinks under a running
//! worker, because a new lock file at the same path is a new inode and its
//! lock would be free while the old one is still held. Reserving an id
//! refreshes its lock file's mtime, so an age-based temp-file cleaner leaves
//! the ids in use alone; the shipped deployment templates point workers at a
//! persistent volume instead. An operation takes all its ids at
//! once or none, so two workers that each hold part of what they need never
//! wait on each other.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Process-local counter for shared-channel directory names.
static NEXT_CHANNEL_SEQ: AtomicU32 = AtomicU32::new(0);

/// Next value for the process-local shared-channel directory sequence.
pub(super) fn next_channel_seq() -> u32 {
    NEXT_CHANNEL_SEQ.fetch_add(1, Ordering::Relaxed)
}

/// How long an operation waits for enough free box ids before it fails.
const RESERVE_WAIT: Duration = Duration::from_secs(300);

/// How often a waiting operation looks again for ids other workers freed.
/// Ids this worker frees wake it at once.
const RESERVE_POLL: Duration = Duration::from_millis(100);

/// Why an operation got no box ids.
#[derive(Debug, thiserror::Error)]
pub enum BoxIdError {
    #[error("operation needs {needed} sandbox boxes but this host provides {available}")]
    TooMany { needed: usize, available: u32 },
    #[error("no {needed} free sandbox boxes on this host after waiting {waited:?}")]
    Exhausted { needed: usize, waited: Duration },
    #[error("box id lock dir {}: {source}", dir.display())]
    LockDir {
        dir: PathBuf,
        source: std::io::Error,
    },
}

/// The box ids `0..count` of this host, shared through lock files in `dir`.
#[derive(Debug)]
pub struct BoxIds {
    dir: PathBuf,
    count: u32,
    wait: Duration,
    freed: Notify,
    /// Where the next scan starts: the lowest id this process freed since its
    /// last reservation, or the id after that reservation. Ids this process
    /// cycles through are found first, without trying the ids other workers
    /// hold; the scan still wraps over every id.
    hint: AtomicU32,
}

impl BoxIds {
    /// Box ids `0..count` locked in `dir`, which is created if missing. Fails
    /// when `dir` cannot hold lock files, so a misconfigured worker refuses
    /// to start instead of sharing ids it cannot lock.
    pub fn open(dir: impl Into<PathBuf>, count: u32) -> Result<Arc<Self>, BoxIdError> {
        Self::with_wait(dir, count, RESERVE_WAIT)
    }

    fn with_wait(
        dir: impl Into<PathBuf>,
        count: u32,
        wait: Duration,
    ) -> Result<Arc<Self>, BoxIdError> {
        let dir = dir.into();
        let lock_dir_error = |source| BoxIdError::LockDir {
            dir: dir.clone(),
            source,
        };
        std::fs::create_dir_all(&dir).map_err(lock_dir_error)?;
        let probe = dir.join("probe.lock");
        let file = open_lock(&probe).map_err(lock_dir_error)?;
        // A held probe is another worker's startup probe, not an error.
        if let Err(e) = try_lock(&file)
            && e.kind() != std::io::ErrorKind::WouldBlock
        {
            return Err(lock_dir_error(e));
        }
        Ok(Arc::new(Self {
            dir,
            count,
            wait,
            freed: Notify::new(),
            hint: AtomicU32::new(0),
        }))
    }

    /// The default ids for tests and the mock sandbox: the first `count` ids
    /// in the process temp dir.
    pub fn in_temp_dir(count: u32) -> Arc<Self> {
        Self::open(std::env::temp_dir().join("broccoli-box-ids"), count)
            .expect("the temp dir holds lock files")
    }

    /// Reserve `n` distinct box ids, waiting for other operations to free
    /// some when too few are free.
    pub(super) async fn reserve(self: &Arc<Self>, n: usize) -> Result<Vec<BoxId>, BoxIdError> {
        if n > self.count as usize {
            return Err(BoxIdError::TooMany {
                needed: n,
                available: self.count,
            });
        }
        let deadline = tokio::time::Instant::now() + self.wait;
        loop {
            // Registered before looking, so an id freed in between wakes us.
            let freed = self.freed.notified();
            tokio::pin!(freed);
            freed.as_mut().enable();
            if let Some(ids) = self.try_reserve(n)? {
                return Ok(ids);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(BoxIdError::Exhausted {
                    needed: n,
                    waited: self.wait,
                });
            }
            let next_look = (now + RESERVE_POLL).min(deadline);
            tokio::select! {
                _ = freed => {}
                _ = tokio::time::sleep_until(next_look) => {}
            }
        }
    }

    /// Take `n` free ids, the first ones found from the hint on, or none.
    fn try_reserve(self: &Arc<Self>, n: usize) -> Result<Option<Vec<BoxId>>, BoxIdError> {
        let mut locked = Vec::with_capacity(n);
        let start = self.hint.load(Ordering::Relaxed) % self.count.max(1);
        for id in (start..self.count).chain(0..start) {
            if locked.len() == n {
                break;
            }
            let file = open_lock(&self.lock_path(id)).map_err(|source| BoxIdError::LockDir {
                dir: self.dir.clone(),
                source,
            })?;
            if try_lock(&file).is_ok() {
                locked.push((id, file));
            }
        }
        if locked.len() < n {
            // Closing the files frees the partial set without waking waiters,
            // which would only find the same shortage.
            return Ok(None);
        }
        if let Some((last, _)) = locked.last() {
            self.hint.store(last + 1, Ordering::Relaxed);
        }
        Ok(Some(
            locked
                .into_iter()
                .map(|(id, file)| {
                    // A lock file in use is never stale to a temp-dir cleaner.
                    let _ = file.set_modified(std::time::SystemTime::now());
                    BoxId {
                        number: id,
                        id: id.to_string(),
                        _lock: file,
                        ids: self.clone(),
                    }
                })
                .collect(),
        ))
    }

    fn lock_path(&self, id: u32) -> PathBuf {
        self.dir.join(format!("box-{id}.lock"))
    }
}

fn open_lock(path: &Path) -> std::io::Result<File> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        // Only ever `flock`-ed, never written.
        .truncate(false)
        .open(path)
}

fn try_lock(file: &File) -> std::io::Result<()> {
    use std::os::unix::io::AsRawFd;
    // SAFETY: `file` owns a valid descriptor for the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// A box id held for one environment. Dropping it releases the id.
#[derive(Debug)]
pub(super) struct BoxId {
    number: u32,
    id: String,
    /// The locked lock file; closing it releases the id.
    _lock: File,
    ids: Arc<BoxIds>,
}

impl BoxId {
    pub(super) fn as_str(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Display for BoxId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.id)
    }
}

impl Drop for BoxId {
    fn drop(&mut self) {
        self.ids.hint.fetch_min(self.number, Ordering::Relaxed);
        // The lock file closes after this body; a woken waiter that looks
        // before then simply looks again on its next poll.
        self.ids.freed.notify_waiters();
    }
}

/// The number of boxes an isolate config file provides (`num_boxes`), or
/// isolate's built-in default when the file does not set it.
pub fn isolate_num_boxes(config: &str) -> Result<u32, String> {
    const ISOLATE_DEFAULT_NUM_BOXES: u32 = 1000;
    for line in config.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some((key, value)) = line.split_once('=')
            && key.trim() == "num_boxes"
        {
            return value
                .trim()
                .parse()
                .map_err(|e| format!("invalid num_boxes {:?}: {e}", value.trim()));
        }
    }
    Ok(ISOLATE_DEFAULT_NUM_BOXES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids_of(boxes: &[BoxId]) -> Vec<u32> {
        boxes.iter().map(|b| b.as_str().parse().unwrap()).collect()
    }

    #[tokio::test]
    async fn a_seventy_process_operation_gets_distinct_ids_none_reissued_while_live() {
        let dir = tempfile::tempdir().unwrap();
        let ids = BoxIds::open(dir.path(), 1000).unwrap();

        // 70 contestant processes, the manager and its compile box.
        let first = ids.reserve(72).await.unwrap();
        let second = ids.reserve(72).await.unwrap();

        let mut all = ids_of(&first);
        all.extend(ids_of(&second));
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 144, "a live id was handed out twice");
    }

    #[tokio::test]
    async fn workers_sharing_a_lock_dir_never_share_an_id() {
        let dir = tempfile::tempdir().unwrap();
        // Two openers stand for two worker processes: flock locks belong to
        // the open file, not the process.
        let worker_a = BoxIds::open(dir.path(), 100).unwrap();
        let worker_b = BoxIds::open(dir.path(), 100).unwrap();

        let held_a = worker_a.reserve(10).await.unwrap();
        let held_b = worker_b.reserve(10).await.unwrap();
        let (a, b) = (ids_of(&held_a), ids_of(&held_b));
        assert!(
            a.iter().all(|id| !b.contains(id)),
            "{a:?} and {b:?} overlap"
        );
    }

    #[tokio::test]
    async fn an_operation_needing_more_boxes_than_the_host_has_fails_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let ids = BoxIds::open(dir.path(), 8).unwrap();
        let err = ids.reserve(9).await.unwrap_err();
        assert!(
            matches!(
                err,
                BoxIdError::TooMany {
                    needed: 9,
                    available: 8
                }
            ),
            "{err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn exhaustion_waits_then_fails_without_a_fallback_id() {
        let dir = tempfile::tempdir().unwrap();
        let ids = BoxIds::with_wait(dir.path(), 4, Duration::from_secs(5)).unwrap();
        let held = ids.reserve(3).await.unwrap();

        let started = tokio::time::Instant::now();
        let err = ids.reserve(2).await.unwrap_err();
        assert!(
            matches!(err, BoxIdError::Exhausted { needed: 2, .. }),
            "{err}"
        );
        assert!(started.elapsed() >= Duration::from_secs(5));
        // The failed attempt left the one free id free.
        assert_eq!(ids_of(&ids.reserve(1).await.unwrap()), [3]);
        drop(held);
    }

    #[tokio::test(start_paused = true)]
    async fn a_waiting_operation_takes_the_ids_another_one_frees() {
        let dir = tempfile::tempdir().unwrap();
        let ids = BoxIds::with_wait(dir.path(), 2, Duration::from_secs(3600)).unwrap();
        let held = ids.reserve(2).await.unwrap();

        let waiting = tokio::spawn({
            let ids = ids.clone();
            async move { ids_of(&ids.reserve(2).await.unwrap()) }
        });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());
        drop(held);
        assert_eq!(waiting.await.unwrap(), [0, 1]);
    }

    #[tokio::test]
    async fn a_worker_reuses_the_ids_it_freed_before_trying_others() {
        let dir = tempfile::tempdir().unwrap();
        let other = BoxIds::open(dir.path(), 100).unwrap();
        let ids = BoxIds::open(dir.path(), 100).unwrap();
        let mine = ids.reserve(2).await.unwrap();
        assert_eq!(ids_of(&mine), [0, 1]);
        let theirs = other.reserve(5).await.unwrap();
        assert_eq!(ids_of(&theirs), [2, 3, 4, 5, 6]);

        drop(mine);
        assert_eq!(ids_of(&ids.reserve(2).await.unwrap()), [0, 1]);
        drop(theirs);
    }

    #[tokio::test]
    async fn the_scan_wraps_to_free_ids_below_the_hint() {
        let dir = tempfile::tempdir().unwrap();
        let ids = BoxIds::open(dir.path(), 5).unwrap();
        let other = BoxIds::open(dir.path(), 5).unwrap();
        let theirs = other.reserve(2).await.unwrap();
        let mine = ids.reserve(2).await.unwrap();
        assert_eq!(ids_of(&mine), [2, 3]);
        let last = other.reserve(1).await.unwrap();
        assert_eq!(ids_of(&last), [4]);

        // The other worker frees ids below this one's hint (4, now held).
        drop(theirs);
        assert_eq!(ids_of(&ids.reserve(2).await.unwrap()), [0, 1]);
        drop((mine, last));
    }

    #[test]
    fn an_unusable_lock_dir_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, b"").unwrap();
        assert!(matches!(
            BoxIds::open(&file, 10),
            Err(BoxIdError::LockDir { .. })
        ));
    }

    #[test]
    fn num_boxes_is_read_from_the_isolate_config() {
        let config = "box_root = /var/local/lib/isolate\n# num_boxes = 5\nnum_boxes = 10000\n";
        assert_eq!(isolate_num_boxes(config), Ok(10000));
        assert_eq!(isolate_num_boxes("first_uid = 1\n"), Ok(1000));
        assert!(isolate_num_boxes("num_boxes = many").is_err());
    }

    /// Run by [`a_worker_takes_over_the_ids_of_one_that_exits`] in a child
    /// process: holds `BOX_ID_CHILD_COUNT` ids from `BOX_ID_CHILD_DIR`, prints
    /// them, and keeps them until its stdin closes.
    #[tokio::test]
    #[ignore = "a helper the cross-process test runs in a child process"]
    async fn box_id_child_process() {
        let dir = std::env::var("BOX_ID_CHILD_DIR").unwrap();
        let count: usize = std::env::var("BOX_ID_CHILD_COUNT")
            .unwrap()
            .parse()
            .unwrap();
        let ids = BoxIds::open(dir, 10).unwrap();
        let held = ids.reserve(count).await.unwrap();
        println!("held {:?}", ids_of(&held));
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    #[test]
    fn a_worker_takes_over_the_ids_of_one_that_exits() {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};

        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "models::operation::handler::box_id::tests::box_id_child_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("BOX_ID_CHILD_DIR", dir.path())
            .env("BOX_ID_CHILD_COUNT", "3")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let held = lines
            .by_ref()
            .map(Result::unwrap)
            .find_map(|l| l.split_once("held ").map(|(_, ids)| ids.to_string()))
            .expect("the child reports its ids");
        assert_eq!(held, "[0, 1, 2]");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let ids = BoxIds::open(dir.path(), 10).unwrap();
        let while_alive = rt.block_on(ids.reserve(3)).unwrap();
        assert_eq!(ids_of(&while_alive), [3, 4, 5]);

        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        // A fresh worker scans from id 0, so it finds the exited one's ids.
        let fresh = BoxIds::open(dir.path(), 10).unwrap();
        let after_exit = rt.block_on(fresh.reserve(3)).unwrap();
        assert_eq!(ids_of(&after_exit), [0, 1, 2]);
        drop(while_alive);
    }
}
