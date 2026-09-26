use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use tracing::{info, warn};

/// Number of disjoint slots the host's 0..1000 isolate box-id space is split
/// into - i.e. the maximum number of worker processes that can share a host
/// without box-id collisions. Each worker claims exactly one slot for life.
const BOX_ID_SLOTS: u32 = 16;
/// Box ids available to a single worker (slot width).
const BOX_IDS_PER_SLOT: u32 = 1000 / BOX_ID_SLOTS;

/// Process-local counter for shared-channel directory names. Kept separate from
/// box-id allocation so channel-dir uniqueness and box-id reservation never
/// perturb each other.
static NEXT_CHANNEL_SEQ: AtomicU32 = AtomicU32::new(0);

/// Fallback counter used only when the whole slot is momentarily saturated (more
/// concurrent environments than the slot can hold); see `allocate_box_id`.
static SATURATION_FALLBACK: AtomicU32 = AtomicU32::new(0);

/// Box-id offsets (within this process's slot) currently reserved by a live
/// environment. Guards allocation: a box id is never reissued while its
/// [`BoxId`] guard is still alive. See [`allocate_box_id`].
static IN_USE: Mutex<BTreeSet<u32>> = Mutex::new(BTreeSet::new());

/// Directory holding the per-slot `flock` files. Set once at startup from
/// `[worker] box_slot_lock_dir` via [`configure_slot_lock_dir`]; unset means the
/// process temp dir. Must be SHARED by every worker on a host - see that config
/// field's doc comment for why a per-container `/tmp` defeats the whole scheme.
static SLOT_LOCK_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Record the configured slot-lock directory. Call once, before the first
/// environment is created: the slot is claimed lazily on first use, so a later
/// call cannot change the slot already claimed. Returns `false` if a directory
/// was already set.
pub fn configure_slot_lock_dir(dir: std::path::PathBuf) -> bool {
    SLOT_LOCK_DIR.set(dir).is_ok()
}

fn slot_lock_dir() -> std::path::PathBuf {
    SLOT_LOCK_DIR
        .get()
        .cloned()
        .unwrap_or_else(std::env::temp_dir)
}

/// First box id of this worker process's slot, lazily claimed on first use.
///
/// isolate box ids are a host-wide 0..1000 namespace, and `isolate --init` on
/// an idle, already-initialized box silently *re-initializes* it (verified:
/// exit 0). So two worker processes that pick the same box id clobber each
/// other's sandbox during setup - the corruption only surfaces later as
/// "box currently in use by another process" -> a spurious SystemError. Pure
/// retry can't fix the clobber-during-setup window (there is no error to retry
/// on). Instead each worker claims a disjoint slot via an advisory file lock
/// (released automatically by the kernel when the process dies) and only ever
/// allocates box ids inside `[base, base + BOX_IDS_PER_SLOT)`, making
/// cross-worker collisions impossible for up to `BOX_ID_SLOTS` workers/host.
static BOX_SLOT_BASE: std::sync::LazyLock<u32> =
    std::sync::LazyLock::new(|| claim_box_id_slot_in(&slot_lock_dir()) * BOX_IDS_PER_SLOT);

/// Claim the lowest free box-id slot for the lifetime of this process using a
/// non-blocking `flock` on a per-slot lock file. The locked fd is intentionally
/// leaked so the lock is held until the process exits, at which point the kernel
/// releases it and the slot becomes available to a restarted worker.
fn claim_box_id_slot_in(dir: &std::path::Path) -> u32 {
    use std::os::unix::io::IntoRawFd;
    if let Err(e) = std::fs::create_dir_all(dir) {
        warn!(dir = %dir.display(), error = %e, "Cannot create isolate box-id slot lock dir");
    }
    for slot in 0..BOX_ID_SLOTS {
        let path = dir.join(format!("broccoli-box-slot-{slot}.lock"));
        let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            // Lock file: only ever `flock`-ed, never written, so truncation is a
            // no-op - stated explicitly to satisfy `suspicious_open_options`.
            .truncate(false)
            .open(&path)
        else {
            continue;
        };
        let fd = file.into_raw_fd();
        // SAFETY: `fd` is a freshly opened, owned file descriptor.
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            // Intentionally leak `fd`: the advisory lock must be held for the
            // whole process lifetime, so the descriptor must never be closed.
            info!(
                slot,
                base = slot * BOX_IDS_PER_SLOT,
                lock_dir = %dir.display(),
                "Claimed isolate box-id slot"
            );
            return slot;
        }
        // Slot owned by another process - close and try the next one.
        // SAFETY: `fd` is owned here and not used after this close.
        unsafe { libc::close(fd) };
    }
    warn!(
        slots = BOX_ID_SLOTS,
        "No free isolate box-id slot (more workers than slots on this host); \
         falling back to slot 0 — box-id collisions are possible"
    );
    0
}

/// A box id reserved for the lifetime of one environment.
///
/// Allocation records the id's offset in [`IN_USE`]; `Drop` clears it. Because
/// the environment (see `EnvironmentList`) owns its `BoxId`, the reservation is
/// released on EVERY exit path - normal cleanup, early `?` returns, and a panic
/// that unwinds the environment map - with no manual release call to forget.
#[derive(Debug)]
pub(super) struct BoxId {
    /// Offset within this process's slot; the key held in [`IN_USE`].
    offset: u32,
    /// Rendered host-wide box id (`BOX_SLOT_BASE + offset`) handed to isolate.
    id: String,
    /// A saturation-fallback id is not a real reservation (its offset may be
    /// shared with a live environment), so its `Drop` must not clear the offset.
    reserved: bool,
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
        if self.reserved {
            in_use().remove(&self.offset);
        }
    }
}

fn in_use() -> std::sync::MutexGuard<'static, BTreeSet<u32>> {
    IN_USE.lock().unwrap_or_else(|e| e.into_inner())
}

/// Reserve the lowest free box id in this worker's slot.
///
/// Reserving the *lowest-free* offset (not a lapping round-robin counter)
/// guarantees a box id is never reissued while its environment still holds it.
/// The previous round-robin `NEXT_BOX_ID % BOX_IDS_PER_SLOT` handed a still-live
/// box to a new op every `BOX_IDS_PER_SLOT` allocations, so `isolate --init`
/// clobbered the running sandbox and produced permanent wrong verdicts (missing
/// source -> CompilationError, lost output -> WrongAnswer, pipe deadlock ->
/// TimeLimitExceeded) that never self-heal because they are terminal, not
/// SystemError.
pub(super) fn allocate_box_id() -> BoxId {
    let base = *BOX_SLOT_BASE;
    let mut in_use = in_use();
    match (0..BOX_IDS_PER_SLOT).find(|o| !in_use.contains(o)) {
        Some(offset) => {
            in_use.insert(offset);
            BoxId {
                offset,
                id: (base + offset).to_string(),
                reserved: true,
            }
        }
        None => {
            // Every id in the slot is live at once: more concurrent environments
            // than the slot holds. This is a capacity limit (lower per-worker
            // concurrency or widen the slot), not something we can resolve here,
            // so fall back to a wrapping id and log it rather than block forever.
            let offset = SATURATION_FALLBACK.fetch_add(1, Ordering::Relaxed) % BOX_IDS_PER_SLOT;
            warn!(
                slot_width = BOX_IDS_PER_SLOT,
                "isolate box-id slot saturated; falling back to a possibly-colliding id"
            );
            BoxId {
                offset,
                id: (base + offset).to_string(),
                reserved: false,
            }
        }
    }
}

/// Next value for the process-local shared-channel directory sequence.
pub(super) fn next_channel_seq() -> u32 {
    NEXT_CHANNEL_SEQ.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The shared IN_USE set is process-global; serialize the tests that mutate it.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset() {
        in_use().clear();
        SATURATION_FALLBACK.store(0, Ordering::Relaxed);
    }

    // Two workers sharing a lock dir must claim DIFFERENT slots. `flock` locks
    // belong to the open file description, so two independent opens in one
    // test process contend exactly as two worker processes would.
    #[test]
    fn workers_sharing_a_lock_dir_claim_distinct_slots() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = claim_box_id_slot_in(dir.path());
        let second = claim_box_id_slot_in(dir.path());
        assert_ne!(
            first, second,
            "two workers on one host must never share an isolate box-id slot"
        );
    }

    // Pins the mechanism behind the measured failure, so the reason the lock
    // dir must be shared is checked rather than merely asserted. Separate
    // directories stand in for separate containers' private `/tmp`: neither
    // sees the other's lock, both claim slot 0, and both then run contestant
    // code as the same host UID.
    #[test]
    fn workers_with_private_lock_dirs_collide_on_slot_zero() {
        let a = tempfile::tempdir().expect("tempdir");
        let b = tempfile::tempdir().expect("tempdir");
        assert_eq!(claim_box_id_slot_in(a.path()), 0);
        assert_eq!(claim_box_id_slot_in(b.path()), 0);
    }

    #[test]
    fn claiming_creates_a_missing_lock_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("not").join("there").join("yet");
        assert_eq!(claim_box_id_slot_in(&nested), 0);
        assert!(nested.join("broccoli-box-slot-0.lock").exists());
    }

    // A box id handed to a still-running environment must never be reissued to a
    // second environment while the first holds it: `isolate --init` on a live box
    // silently re-initializes it, clobbering the running sandbox and producing
    // permanent wrong verdicts (missing source -> CE, lost output -> WA).
    #[test]
    fn does_not_reissue_a_live_box_id() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // Hold several live ids at once so churn exercises more than one offset
        // (a single held id would only ever bounce between offsets 0 and 1).
        let live: Vec<BoxId> = (0..(BOX_IDS_PER_SLOT / 2))
            .map(|_| allocate_box_id())
            .collect();
        let held: std::collections::HashSet<&str> = live.iter().map(|b| b.as_str()).collect();
        // Churn more than a full slot's worth of allocate/release cycles. A
        // lapping counter would wrap and hand out a held id again; the free-list
        // must always skip every still-held id.
        for _ in 0..(BOX_IDS_PER_SLOT * 2) {
            let tmp = allocate_box_id();
            assert!(
                !held.contains(tmp.as_str()),
                "reissued a box id still held by a live environment: {}",
                tmp.as_str()
            );
        }
    }

    // When the whole slot is live at once, allocation must fall back to a
    // non-reserved id whose Drop leaves the real reservations untouched - a
    // fallback that cleared a shared offset would free a still-live box.
    #[test]
    fn saturation_fallback_does_not_corrupt_reservations() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // Fill every offset in the slot with a real reservation.
        let live: Vec<BoxId> = (0..BOX_IDS_PER_SLOT).map(|_| allocate_box_id()).collect();
        assert_eq!(
            in_use().len() as u32,
            BOX_IDS_PER_SLOT,
            "slot should be full"
        );
        // The next allocation saturates: a fallback id, not a real reservation.
        let fallback = allocate_box_id();
        assert!(
            !fallback.reserved,
            "expected a non-reserved saturation fallback"
        );
        let before = in_use().len();
        drop(fallback);
        assert_eq!(
            in_use().len(),
            before,
            "fallback Drop must not remove a live reservation's offset"
        );
        drop(live);
        assert!(
            in_use().is_empty(),
            "dropping every live id should free the slot"
        );
    }

    // Dropping a `BoxId` frees its offset for reuse (lowest-free reallocation).
    #[test]
    fn releases_on_drop() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let first = allocate_box_id().as_str().to_string();
        // The temporary above is dropped at the end of that statement, freeing
        // its offset; the next allocation must reuse the now-lowest-free id.
        let second = allocate_box_id();
        assert_eq!(second.as_str(), first, "freed box id should be reused");
    }
}
