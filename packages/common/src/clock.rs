//! The platform clock: the one wall clock every server and worker agrees on.
//!
//! Contest phases, access-token expiry, rate-limit windows, the HTTP `Date`
//! header and every timestamp a node writes are compared with times stamped
//! on other nodes. Reading each node's own wall clock would let nodes
//! disagree by however far their clocks have drifted, which in a lab without
//! NTP is seconds to minutes. So the platform's wall clock is the
//! database's: every node measures its offset to Postgres
//! `clock_timestamp()` and serves time as its own monotonic clock plus that
//! offset.
//!
//! - Reading the clock never touches the network or writes shared memory:
//!   [`PlatformClock::now`] is an `Instant` read and a seqlock read of the
//!   anchor.
//! - Samples go over a connection of their own, never the busy main pool,
//!   and are timed from the moment the query is sent, so a saturated pool
//!   cannot skew them. Every 30 to 60 s a short burst is taken and the
//!   tightest kept; of the recent samples the one with the smallest error
//!   bound wins (half its round trip plus the drift the two oscillators may
//!   have accumulated since).
//! - The clock never runs backwards. A resample that lands behind time
//!   already handed out holds the clock still until it catches up; only a
//!   step of more than [`MAX_HOLD`] (the database host's clock itself was
//!   set back) is followed, with a warning.
//!
//! Durations (deadlines, leases, backoff, metrics) stay on `Instant`: only
//! wall time compared with another node's wall time comes from here. The
//! workspace `clippy.toml` disallows `Utc::now()` and `SystemTime::now()`, so
//! a new comparison cannot quietly read the local clock.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sea_orm::sqlx::{Connection, PgConnection};

/// Resampling period bounds; each wait is drawn uniformly between them so
/// replicas do not sample in lockstep.
const RESAMPLE_MIN: Duration = Duration::from_secs(30);
const RESAMPLE_MAX: Duration = Duration::from_secs(60);

/// Samples kept for choosing the best one: about five minutes of history.
const WINDOW: usize = 8;

/// Samples taken back to back on every (re)sample; the tightest is kept.
const BURST: usize = 3;

/// Assumed worst-case relative drift of this host's monotonic clock against
/// the database host's clock (200 ppm: two uncorrected crystal oscillators).
const DRIFT_PPM: i64 = 200;

/// A backwards correction up to this size is absorbed by holding the clock
/// still; a larger one is followed.
pub const MAX_HOLD: Duration = Duration::from_secs(1);

/// Local wall clock offsets above this are logged as a warning at boot.
const WARN_LOCAL_SKEW: Duration = Duration::from_secs(1);

/// After a small step back the clock holds at the latest time handed out plus
/// this, so a reader that was mid-read during the switch cannot have been
/// handed more.
const HOLD_MARGIN_US: i64 = 100;

/// Bound on one sample, so a stalled connection cannot stall a resample.
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);

const CLOCK_QUERY: &str = "SELECT (extract(epoch FROM clock_timestamp()) * 1000000)::bigint";

/// One reading of the database clock against this process's monotonic clock.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// Local monotonic time at the midpoint of the round trip.
    at: Instant,
    /// Database time at `at`, in epoch microseconds.
    db_us: i64,
    /// Half the round trip: the true database time at `at` is within this.
    half_rtt_us: i64,
}

impl Sample {
    /// The error bound of this sample when read at `now`.
    fn error_us(&self, now: Instant) -> i64 {
        let age_us = now.saturating_duration_since(self.at).as_micros() as i64;
        self.half_rtt_us + age_us * DRIFT_PPM / 1_000_000
    }
}

/// Picks the sample to serve time from: the smallest error bound at `now`.
fn best(samples: &VecDeque<Sample>, now: Instant) -> Option<Sample> {
    samples.iter().copied().min_by_key(|s| s.error_us(now))
}

/// The anchor `now()` reads: platform time is
/// `max(monotonic + offset, hold)`. Written only under the samples lock, read
/// lock-free as a seqlock, so an adopt is seen whole or not at all.
struct Anchor {
    seq: AtomicU64,
    /// Database time minus `base`-relative monotonic time, in microseconds.
    offset_us: AtomicI64,
    /// Time handed out before the last step back, held until the new offset
    /// passes it; `i64::MIN` when there is nothing to hold.
    hold_us: AtomicI64,
}

impl Anchor {
    /// The offset and hold, consistent with each other, and the monotonic
    /// time read between them.
    fn read(&self, mono_us: impl Fn() -> i64) -> (i64, i64, i64) {
        loop {
            let before = self.seq.load(Ordering::SeqCst);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let offset = self.offset_us.load(Ordering::SeqCst);
            let hold = self.hold_us.load(Ordering::SeqCst);
            let mono = mono_us();
            if self.seq.load(Ordering::SeqCst) == before {
                return (offset, hold, mono);
            }
        }
    }

    /// Publishes the offset and hold `next` computes from the current ones
    /// and the monotonic time read once readers are locked out. Callers
    /// serialise on the samples lock.
    fn update(&self, mono_us: impl Fn() -> i64, next: impl FnOnce(i64, i64, i64) -> (i64, i64)) {
        let seq = self.seq.load(Ordering::SeqCst);
        self.seq.store(seq + 1, Ordering::SeqCst);
        // Every reader that returns the old values read its monotonic time
        // before this point.
        let mono = mono_us();
        let (offset, hold) = next(
            self.offset_us.load(Ordering::SeqCst),
            self.hold_us.load(Ordering::SeqCst),
            mono,
        );
        self.offset_us.store(offset, Ordering::SeqCst);
        self.hold_us.store(hold, Ordering::SeqCst);
        self.seq.store(seq + 2, Ordering::SeqCst);
    }
}

struct Inner {
    /// Where samples come from; `None` for a clock that never resamples.
    url: Option<String>,
    /// The dedicated sampling connection, opened on first use and after a
    /// failure. It bypasses [`crate::db::DatabaseConnection`], which owns NUL
    /// in bound values: it only runs [`CLOCK_QUERY`], which binds none, and a
    /// pooled handle would put pool waits inside the timed round trip.
    conn: tokio::sync::Mutex<Option<PgConnection>>,
    /// Fixed reference for turning `Instant`s into integers.
    base: Instant,
    anchor: Anchor,
    samples: Mutex<VecDeque<Sample>>,
}

impl Inner {
    fn mono_us(&self, at: Instant) -> i64 {
        at.saturating_duration_since(self.base).as_micros() as i64
    }

    fn now_us(&self) -> i64 {
        let (offset, hold, mono) = self.anchor.read(|| self.mono_us(Instant::now()));
        (mono + offset).max(hold)
    }

    /// Serve time from `sample` from now on. Callers hold the samples lock.
    fn adopt(&self, sample: Sample) {
        let offset = sample.db_us - self.mono_us(sample.at);
        self.anchor.update(
            || self.mono_us(Instant::now()),
            |old_offset, old_hold, mono| {
                // The latest time any reader can have been handed, with a
                // margin for a reader whose view of the lock-out lags.
                let handed_out = (mono + old_offset + HOLD_MARGIN_US).max(old_hold);
                let behind_us = handed_out.saturating_sub(mono + offset);
                if behind_us == 0 {
                    (offset, i64::MIN)
                } else if behind_us > MAX_HOLD.as_micros() as i64 {
                    tracing::warn!(
                        behind_ms = behind_us as f64 / 1000.0,
                        "The database clock was set back; the platform clock follows it"
                    );
                    (offset, i64::MIN)
                } else {
                    (offset, handed_out)
                }
            },
        );
    }

    /// Add `sample` to the window and serve time from the best one.
    fn record(&self, sample: Sample) {
        let mut samples = self.samples.lock().expect("clock samples lock");
        if samples.len() == WINDOW {
            samples.pop_front();
        }
        samples.push_back(sample);
        if let Some(best) = best(&samples, Instant::now()) {
            self.adopt(best);
        }
    }

    /// Discard every sample and serve time from `sample`.
    fn reset(&self, sample: Sample) {
        let mut samples = self.samples.lock().expect("clock samples lock");
        samples.clear();
        samples.push_back(sample);
        self.adopt(sample);
    }

    /// [`BURST`] samples back to back on the sampling connection; the
    /// tightest, and its offset from this host's wall clock.
    async fn take_burst(&self) -> anyhow::Result<(Sample, i64)> {
        let url = self
            .url
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("this clock has no database to sample"))?;
        let mut conn = self.conn.lock().await;
        let result = async {
            if conn.is_none() {
                *conn = Some(PgConnection::connect(url).await?);
            }
            let c = conn.as_mut().expect("connected above");
            let mut best: Option<(Sample, i64)> = None;
            for _ in 0..BURST {
                let taken = take_sample(c).await?;
                if best.is_none_or(|(b, _)| taken.0.half_rtt_us < b.half_rtt_us) {
                    best = Some(taken);
                }
            }
            anyhow::Ok(best.expect("BURST is at least one"))
        }
        .await;
        if result.is_err() {
            // Reconnect next time; the connection may be broken.
            *conn = None;
        }
        result
    }
}

/// One timed read of the database clock on an open connection. Also returns
/// `database - local wall clock`, for the boot warning.
async fn take_sample(conn: &mut PgConnection) -> anyhow::Result<(Sample, i64)> {
    #[allow(clippy::disallowed_methods)] // The one place the local wall clock is read.
    let local_before = Utc::now();
    let started = Instant::now();
    let read = sea_orm::sqlx::query_scalar::<_, i64>(CLOCK_QUERY).fetch_one(&mut *conn);
    let db_us = tokio::time::timeout(SAMPLE_TIMEOUT, read)
        .await
        .map_err(|_| anyhow::anyhow!("timed out reading the database clock"))??;
    let rtt = started.elapsed();
    let half_rtt_us = rtt.as_micros() as i64 / 2;
    let sample = Sample {
        at: started + rtt / 2,
        db_us,
        half_rtt_us,
    };
    let local_offset_us = db_us - (local_before.timestamp_micros() + half_rtt_us);
    Ok((sample, local_offset_us))
}

/// The database-anchored wall clock. Cheap to clone; clones share samples.
#[derive(Clone)]
pub struct PlatformClock {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for PlatformClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformClock")
            .field("now", &self.now())
            .finish()
    }
}

impl PlatformClock {
    /// Measure the database clock at `database_url` on a connection of its
    /// own and build the platform clock on it, with a background task that
    /// keeps resampling for as long as a clone lives. Logs a warning when this
    /// host's own wall clock is off by more than a second: nothing in the
    /// platform depends on it, but log timestamps do.
    pub async fn connect(database_url: &str) -> anyhow::Result<Self> {
        let inner = Inner::new(Some(database_url.to_owned()));
        let (sample, local_offset_us) = inner.take_burst().await?;
        let skew = Duration::from_micros(local_offset_us.unsigned_abs());
        if skew > WARN_LOCAL_SKEW {
            tracing::warn!(
                local_offset_ms = local_offset_us as f64 / 1000.0,
                "This host's clock differs from the database clock. The platform keeps time \
                 by the database clock, so judging is unaffected, but this host's log \
                 timestamps are off by that much"
            );
        }
        inner.reset(sample);
        let clock = Self {
            inner: Arc::new(inner),
        };
        spawn_resampler(Arc::downgrade(&clock.inner));
        Ok(clock)
    }

    /// A clock anchored to this host's wall clock with no database, for unit
    /// tests that build an `AppState` without one.
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn local_for_tests() -> Self {
        #[allow(clippy::disallowed_methods)]
        let db_us = Utc::now().timestamp_micros();
        let inner = Inner::new(None);
        inner.reset(Sample {
            at: Instant::now(),
            db_us,
            half_rtt_us: 0,
        });
        Self {
            inner: Arc::new(inner),
        }
    }

    /// The platform's current wall time.
    pub fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(self.inner.now_us())
            .expect("platform time is within chrono's range")
    }

    /// Take a fresh burst of samples, discarding older ones, and serve time
    /// from it at once, for a test that just moved the database clock.
    #[cfg(any(test, feature = "test-hooks"))]
    pub async fn sync(&self) -> anyhow::Result<()> {
        let (sample, _) = self.inner.take_burst().await?;
        self.inner.reset(sample);
        Ok(())
    }
}

impl Inner {
    fn new(url: Option<String>) -> Self {
        Self {
            url,
            conn: tokio::sync::Mutex::new(None),
            base: Instant::now(),
            anchor: Anchor {
                seq: AtomicU64::new(0),
                offset_us: AtomicI64::new(0),
                hold_us: AtomicI64::new(i64::MIN),
            },
            samples: Mutex::new(VecDeque::new()),
        }
    }
}

/// Resample every 30 to 60 s while any clone of the clock lives. A failed
/// burst is logged and the clock keeps extrapolating from the samples it has.
fn spawn_resampler(clock: Weak<Inner>) {
    tokio::spawn(async move {
        loop {
            let wait = rand::random_range(RESAMPLE_MIN..=RESAMPLE_MAX);
            tokio::time::sleep(wait).await;
            let Some(inner) = clock.upgrade() else {
                return;
            };
            match inner.take_burst().await {
                Ok((sample, _)) => inner.record(sample),
                Err(e) => {
                    tracing::warn!(error = %e, "Platform clock: could not sample the database clock")
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(at: Instant, db_us: i64, half_rtt_us: i64) -> Sample {
        Sample {
            at,
            db_us,
            half_rtt_us,
        }
    }

    #[test]
    fn the_tightest_round_trip_wins_among_samples_of_the_same_age() {
        let t = Instant::now();
        let samples = VecDeque::from([sample(t, 1, 900), sample(t, 2, 100), sample(t, 3, 500)]);
        assert_eq!(best(&samples, t).unwrap().db_us, 2);
    }

    #[test]
    fn an_old_tight_sample_gives_way_once_its_drift_bound_exceeds_a_fresh_one() {
        let t = Instant::now();
        let old = sample(t, 1, 100);
        let fresh = sample(t + Duration::from_secs(240), 2, 5_000);
        let samples = VecDeque::from([old, fresh]);
        // 60 s after the old sample: 100 us + 12 ms of drift > 5 ms.
        assert_eq!(
            best(&samples, t + Duration::from_secs(300)).unwrap().db_us,
            2
        );
        // At the fresh sample's own time the old one is still tighter.
        let samples = VecDeque::from([old, sample(t + Duration::from_secs(10), 2, 5_000)]);
        assert_eq!(
            best(&samples, t + Duration::from_secs(10)).unwrap().db_us,
            1
        );
    }

    fn clock_at(db_us: i64) -> (Inner, Instant) {
        let inner = Inner::new(None);
        let t = Instant::now();
        inner.reset(sample(t, db_us, 0));
        (inner, t)
    }

    #[test]
    fn a_small_step_back_holds_the_clock_still_instead_of_reversing_it() {
        let (clock, t) = clock_at(10_000_000_000);
        let before = clock.now_us();
        clock.reset(sample(t, 10_000_000_000 - 500_000, 0));
        let after = clock.now_us();
        assert!(after >= before);
        assert!(after - before < 100_000);
    }

    #[test]
    fn a_step_back_beyond_the_hold_limit_is_followed() {
        let (clock, t) = clock_at(10_000_000_000);
        let before = clock.now_us();
        clock.reset(sample(t, 10_000_000_000 - 60_000_000, 0));
        assert!(before - clock.now_us() > 59_000_000);
    }

    #[test]
    fn a_forward_correction_takes_effect_at_once() {
        let (clock, t) = clock_at(10_000_000_000);
        clock.reset(sample(t, 10_000_000_000 + 3_600_000_000, 0));
        assert!(clock.now_us() >= 10_000_000_000 + 3_600_000_000);
    }

    /// Readers racing a stream of small steps back never see time reverse
    /// within one thread: an adopt is seen whole, never offset without hold.
    #[test]
    fn concurrent_readers_never_see_time_reverse_across_adopts() {
        let (clock, t) = clock_at(10_000_000_000);
        let clock = Arc::new(clock);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let (clock, stop) = (clock.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut last = i64::MIN;
                    while !stop.load(Ordering::Relaxed) {
                        let now = clock.now_us();
                        assert!(now >= last, "time went back: {last} -> {now}");
                        last = now;
                    }
                })
            })
            .collect();
        for step in 1..=50_000 {
            // Alternately 0.5 s behind and back on time: each adopt holds.
            let behind = if step % 2 == 0 { 500_000 } else { 0 };
            clock.reset(sample(t, 10_000_000_000 - behind, 0));
        }
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().expect("reader saw time reverse");
        }
    }
}
