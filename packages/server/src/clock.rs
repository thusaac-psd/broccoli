//! The platform clock: the one wall clock every server replica agrees on.
//!
//! Contest phases, access-token expiry, rate-limit windows, the HTTP `Date`
//! header and every timestamp a server writes are compared with times
//! stamped on other nodes. Reading each replica's own wall clock would let
//! replicas disagree by however far their clocks have drifted, which in a lab
//! without NTP is seconds to minutes. So the platform's wall clock is the
//! database's: every server measures its offset to Postgres
//! `clock_timestamp()` and serves time as its own monotonic clock plus that
//! offset.
//!
//! - Reading the clock never touches the network: [`PlatformClock::now`] is
//!   an `Instant` read and an atomic.
//! - A background task resamples every 30 to 60 s. Of the recent samples the
//!   one with the smallest error bound wins: half its round trip plus the
//!   drift the two oscillators may have accumulated since it was taken. A
//!   sample delayed by a busy pool therefore never displaces a tight one.
//! - The clock never runs backwards. A resample that lands behind time
//!   already handed out holds the clock still until it catches up; only a
//!   step of more than [`MAX_HOLD`] (the database host's clock itself was
//!   set back) is followed, with a warning.
//!
//! Durations (deadlines, leases, backoff, metrics) stay on `Instant`: only
//! wall time compared with another node's wall time comes from here. In the
//! server crate `Utc::now()` and `SystemTime::now()` are disallowed by
//! `clippy.toml`, so a new comparison cannot quietly read the local clock.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

/// Resampling period bounds; each wait is drawn uniformly between them so
/// replicas do not sample in lockstep.
const RESAMPLE_MIN: Duration = Duration::from_secs(30);
const RESAMPLE_MAX: Duration = Duration::from_secs(60);

/// Samples kept for choosing the best one: about five minutes of history.
const WINDOW: usize = 8;

/// Samples taken back to back by [`PlatformClock::sync`]; the tightest wins.
const BURST: usize = 3;

/// Assumed worst-case relative drift of this host's monotonic clock against
/// the database host's clock (200 ppm: two uncorrected crystal oscillators).
const DRIFT_PPM: i64 = 200;

/// A backwards correction up to this size is absorbed by holding the clock
/// still; a larger one is followed.
pub const MAX_HOLD: Duration = Duration::from_secs(1);

/// Local wall clock offsets above this are logged as a warning at boot.
const WARN_LOCAL_SKEW: Duration = Duration::from_secs(1);

/// Bound on one sample, so a stalled pool cannot stall a resample forever.
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);

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

    #[cfg(test)]
    fn db_time_at(&self, now: Instant) -> i64 {
        let elapsed = now.saturating_duration_since(self.at).as_micros() as i64;
        self.db_us + elapsed
    }
}

/// Picks the sample to serve time from: the smallest error bound at `now`.
fn best(samples: &VecDeque<Sample>, now: Instant) -> Option<Sample> {
    samples.iter().copied().min_by_key(|s| s.error_us(now))
}

struct Inner {
    db: DatabaseConnection,
    /// Fixed reference for turning `Instant`s into integers.
    base: Instant,
    /// Database time minus `base`-relative monotonic time, in microseconds,
    /// from the sample chosen last. `now()` is one load and one add.
    offset_us: AtomicI64,
    /// The latest time handed out, in epoch microseconds; `now()` never
    /// returns less.
    floor_us: AtomicI64,
    samples: Mutex<VecDeque<Sample>>,
}

impl Inner {
    fn mono_us(&self, at: Instant) -> i64 {
        at.saturating_duration_since(self.base).as_micros() as i64
    }

    fn now_us(&self) -> i64 {
        let estimate = self.mono_us(Instant::now()) + self.offset_us.load(Ordering::Acquire);
        let previous = self.floor_us.fetch_max(estimate, Ordering::AcqRel);
        previous.max(estimate)
    }

    /// Serve time from `sample` from now on.
    fn adopt(&self, sample: Sample) {
        let offset = sample.db_us - self.mono_us(sample.at);
        let estimate = self.mono_us(Instant::now()) + offset;
        let behind_us = self
            .floor_us
            .load(Ordering::Acquire)
            .saturating_sub(estimate);
        if behind_us > MAX_HOLD.as_micros() as i64 {
            tracing::warn!(
                behind_ms = behind_us as f64 / 1000.0,
                "The database clock was set back; the platform clock follows it"
            );
            self.floor_us.store(estimate, Ordering::Release);
        }
        self.offset_us.store(offset, Ordering::Release);
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
    /// Measure the database clock and build the platform clock on it, with a
    /// background task that keeps resampling for as long as a clone lives.
    /// Logs a warning when this host's own wall clock is off by more than a
    /// second: nothing in the platform depends on it, but log timestamps do.
    pub async fn connect(db: &DatabaseConnection) -> anyhow::Result<Self> {
        let (sample, local_offset_us) = take_burst(db).await?;
        let skew = Duration::from_micros(local_offset_us.unsigned_abs());
        if skew > WARN_LOCAL_SKEW {
            tracing::warn!(
                local_offset_ms = local_offset_us as f64 / 1000.0,
                "This host's clock differs from the database clock. The platform keeps time \
                 by the database clock, so judging is unaffected, but this host's log \
                 timestamps are off by that much"
            );
        }
        let clock = Self::from_sample(db.clone(), sample);
        spawn_resampler(Arc::downgrade(&clock.inner));
        Ok(clock)
    }

    fn from_sample(db: DatabaseConnection, sample: Sample) -> Self {
        let inner = Inner {
            db,
            base: sample.at,
            offset_us: AtomicI64::new(0),
            floor_us: AtomicI64::new(i64::MIN),
            samples: Mutex::new(VecDeque::new()),
        };
        inner.reset(sample);
        Self {
            inner: Arc::new(inner),
        }
    }

    /// A clock anchored to this host's wall clock with no database, for unit
    /// tests that build an `AppState` without one.
    #[cfg(test)]
    pub(crate) fn local_for_tests() -> Self {
        #[allow(clippy::disallowed_methods)]
        let db_us = Utc::now().timestamp_micros();
        Self::from_sample(
            DatabaseConnection::default(),
            Sample {
                at: Instant::now(),
                db_us,
                half_rtt_us: 0,
            },
        )
    }

    /// The platform's current wall time.
    pub fn now(&self) -> DateTime<Utc> {
        DateTime::from_timestamp_micros(self.inner.now_us())
            .expect("platform time is within chrono's range")
    }

    /// Take a fresh burst of samples, discarding older ones, and serve time
    /// from it at once. The background task samples on its own; this is for
    /// a caller that just moved the database clock (the test clock).
    pub async fn sync(&self) -> anyhow::Result<()> {
        let (sample, _) = take_burst(&self.inner.db).await?;
        self.inner.reset(sample);
        Ok(())
    }
}

/// Resample every 30 to 60 s while any clone of the clock lives. A failed
/// sample is logged and the clock keeps extrapolating from the ones it has.
fn spawn_resampler(clock: Weak<Inner>) {
    tokio::spawn(async move {
        loop {
            let wait = rand::random_range(RESAMPLE_MIN..=RESAMPLE_MAX);
            tokio::time::sleep(wait).await;
            let Some(inner) = clock.upgrade() else {
                return;
            };
            match take_sample(&inner.db).await {
                Ok((sample, _)) => inner.record(sample),
                Err(e) => {
                    tracing::warn!(error = %e, "Platform clock: could not sample the database clock")
                }
            }
        }
    });
}

/// [`BURST`] samples back to back; the tightest, and its local wall offset.
async fn take_burst(db: &DatabaseConnection) -> anyhow::Result<(Sample, i64)> {
    let mut best: Option<(Sample, i64)> = None;
    for _ in 0..BURST {
        let taken = take_sample(db).await?;
        if best.is_none_or(|(b, _)| taken.0.half_rtt_us < b.half_rtt_us) {
            best = Some(taken);
        }
    }
    Ok(best.expect("BURST is at least one"))
}

/// One timed read of the database clock. Also returns `database - local wall
/// clock`, for the boot warning.
async fn take_sample(db: &DatabaseConnection) -> anyhow::Result<(Sample, i64)> {
    #[allow(clippy::disallowed_methods)] // The one place the local wall clock is read.
    let local_before = Utc::now();
    let started = Instant::now();
    let read = db.query_one_raw(Statement::from_string(
        DatabaseBackend::Postgres,
        "SELECT (extract(epoch FROM clock_timestamp()) * 1000000)::bigint AS us",
    ));
    let row = tokio::time::timeout(SAMPLE_TIMEOUT, read)
        .await
        .map_err(|_| anyhow::anyhow!("timed out reading the database clock"))??
        .ok_or_else(|| anyhow::anyhow!("database clock query returned no row"))?;
    let db_us: i64 = row.try_get("", "us")?;
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

    fn clock_at(db_us: i64) -> (PlatformClock, Instant) {
        let t = Instant::now();
        let clock = PlatformClock::from_sample(DatabaseConnection::default(), sample(t, db_us, 0));
        (clock, t)
    }

    #[test]
    fn a_small_step_back_holds_the_clock_still_instead_of_reversing_it() {
        let (clock, t) = clock_at(10_000_000_000);
        let before = clock.now();
        clock.inner.reset(sample(t, 10_000_000_000 - 500_000, 0));
        assert!(clock.now() >= before);
        assert!(clock.now() - before < chrono::Duration::milliseconds(100));
    }

    #[test]
    fn a_step_back_beyond_the_hold_limit_is_followed() {
        let (clock, t) = clock_at(10_000_000_000);
        let before = clock.now();
        clock.inner.reset(sample(t, 10_000_000_000 - 60_000_000, 0));
        let after = clock.now();
        assert!(
            before - after > chrono::Duration::seconds(59),
            "{before} -> {after}"
        );
    }

    #[test]
    fn a_forward_correction_takes_effect_at_once() {
        let (clock, t) = clock_at(10_000_000_000);
        clock
            .inner
            .reset(sample(t, 10_000_000_000 + 3_600_000_000, 0));
        assert!(clock.now().timestamp() >= 10_000 + 3_600);
    }

    #[test]
    fn a_sample_extrapolates_by_monotonic_time() {
        let t = Instant::now();
        let s = sample(t, 1_000_000, 0);
        assert_eq!(s.db_time_at(t + Duration::from_millis(1500)), 2_500_000);
    }
}
