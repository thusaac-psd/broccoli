//! Boot-time clock skew check between this node and the shared infrastructure.
//!
//! Several cross-node mechanisms compare a timestamp written on one machine
//! with a clock read on another: MQ message scores, worker heartbeat
//! staleness, lease steal windows, refresh-token reuse grace. With skewed
//! clocks they misbehave quietly (messages invisible for the skew, leases
//! stolen early, ...). Every server and worker therefore compares its clock
//! with Postgres `clock_timestamp()` and Redis `TIME` at boot: it warns above
//! [`ClockConfig::warn_skew_ms`] and refuses to start above
//! [`ClockConfig::max_skew_ms`].
//!
//! Escape hatch for operators who must start anyway (e.g. a rehearsal on
//! hardware without NTP): set `clock.max_skew_ms = 0`
//! (`BROCCOLI__CLOCK__MAX_SKEW_MS=0`). The warning is still logged.

use std::time::{Duration, Instant};

use sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};
use serde::{Deserialize, Serialize};

/// `[clock]` section shared by the server and the worker.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClockConfig {
    /// Log a warning when this node's clock differs from Postgres or Redis by
    /// more than this many milliseconds.
    #[serde(default = "default_warn_skew_ms")]
    pub warn_skew_ms: u64,
    /// Refuse to start when the skew is certainly above this many
    /// milliseconds (after allowing for the measurement's round-trip). `0`
    /// disables the refusal.
    #[serde(default = "default_max_skew_ms")]
    pub max_skew_ms: u64,
}

fn default_warn_skew_ms() -> u64 {
    1_000
}

/// Above ~5 s every MQ round-trip visibly stalls, and the 10 s cross-node
/// windows (worker staleness, token reuse grace) start to misfire.
fn default_max_skew_ms() -> u64 {
    5_000
}

impl Default for ClockConfig {
    fn default() -> Self {
        Self {
            warn_skew_ms: default_warn_skew_ms(),
            max_skew_ms: default_max_skew_ms(),
        }
    }
}

/// Per-source bound on taking one sample, so a dead dependency cannot stall
/// boot here (boot reports dependency failures elsewhere).
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(3);

/// One comparison of the local clock against a remote clock.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkewSample {
    pub source: &'static str,
    /// `remote - local` at the midpoint of the round-trip, in microseconds.
    pub offset_us: i64,
    /// Half the round-trip: the remote reading could have been taken anywhere
    /// in the round-trip, so the true offset is within this of `offset_us`.
    pub uncertainty_us: i64,
}

impl SkewSample {
    /// Time a remote clock read and compare it with the local clock.
    async fn take<F>(source: &'static str, read_remote_us: F) -> anyhow::Result<Self>
    where
        F: std::future::Future<Output = anyhow::Result<i64>>,
    {
        let local_before = chrono::Utc::now();
        let started = Instant::now();
        let remote_us = tokio::time::timeout(SAMPLE_TIMEOUT, read_remote_us)
            .await
            .map_err(|_| anyhow::anyhow!("timed out reading the {source} clock"))??;
        let rtt_us = started.elapsed().as_micros() as i64;
        let local_mid_us = local_before.timestamp_micros() + rtt_us / 2;
        Ok(Self {
            source,
            offset_us: remote_us - local_mid_us,
            uncertainty_us: rtt_us / 2,
        })
    }

    fn abs_ms(self) -> f64 {
        self.offset_us.unsigned_abs() as f64 / 1000.0
    }
}

/// The verdict for a set of samples, as a pure function so it is testable.
/// Returns the samples to warn about, or the error that refuses boot.
pub fn evaluate(samples: &[SkewSample], config: &ClockConfig) -> anyhow::Result<Vec<SkewSample>> {
    let max_us = (config.max_skew_ms as i64).saturating_mul(1000);
    if config.max_skew_ms > 0
        && let Some(s) = samples
            .iter()
            .find(|s| s.offset_us.abs() - s.uncertainty_us > max_us)
    {
        anyhow::bail!(
            "this node's clock is {:.1} s {} {} (limit {} ms). Cross-node timestamps (MQ \
             visibility, heartbeats, leases, token grace windows) break under this skew. \
             Synchronize every server, worker and the infra host with NTP (for example chrony \
             on the infra host serving the LAN). To start anyway, set clock.max_skew_ms = 0 \
             (BROCCOLI__CLOCK__MAX_SKEW_MS=0).",
            s.abs_ms() / 1000.0,
            if s.offset_us > 0 {
                "behind"
            } else {
                "ahead of"
            },
            s.source,
            config.max_skew_ms,
        );
    }
    let warn_us = (config.warn_skew_ms as i64).saturating_mul(1000);
    Ok(samples
        .iter()
        .copied()
        .filter(|s| s.offset_us.abs() > warn_us)
        .collect())
}

async fn postgres_now_us(db: &DatabaseConnection) -> anyhow::Result<i64> {
    let row = db
        .query_one_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            "SELECT (extract(epoch FROM clock_timestamp()) * 1000000)::bigint AS us",
        ))
        .await?
        .ok_or_else(|| anyhow::anyhow!("clock query returned no row"))?;
    Ok(row.try_get::<i64>("", "us")?)
}

async fn redis_now_us(client: &redis::Client) -> anyhow::Result<i64> {
    let mut conn = client.get_multiplexed_async_connection().await?;
    let (secs, micros): (i64, i64) = redis::cmd("TIME").query_async(&mut conn).await?;
    Ok(secs * 1_000_000 + micros)
}

/// Compare the local clock with Postgres and (when given) Redis, log the
/// result, and fail when the skew exceeds the configured bound. A source that
/// cannot be sampled is logged and skipped: reachability is not this check's
/// job.
pub async fn check_clock_skew(
    db: &DatabaseConnection,
    redis: Option<&redis::Client>,
    config: &ClockConfig,
) -> anyhow::Result<()> {
    let mut samples = Vec::with_capacity(2);
    let pg = SkewSample::take("Postgres", postgres_now_us(db)).await;
    let rd = match redis {
        Some(client) => Some(SkewSample::take("Redis", redis_now_us(client)).await),
        None => None,
    };
    for result in std::iter::once(pg).chain(rd) {
        match result {
            Ok(sample) => samples.push(sample),
            Err(e) => tracing::warn!(error = %e, "Clock skew check: could not sample a clock"),
        }
    }

    let warn = evaluate(&samples, config)?;
    for s in &samples {
        tracing::info!(
            source = s.source,
            offset_ms = s.offset_us as f64 / 1000.0,
            uncertainty_ms = s.uncertainty_us as f64 / 1000.0,
            "Clock skew check"
        );
    }
    for s in warn {
        tracing::warn!(
            source = s.source,
            offset_ms = s.offset_us as f64 / 1000.0,
            warn_skew_ms = config.warn_skew_ms,
            max_skew_ms = config.max_skew_ms,
            "This node's clock differs from a shared clock by more than the warning \
             threshold; synchronize clocks with NTP"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(offset_ms: i64, uncertainty_ms: i64) -> SkewSample {
        SkewSample {
            source: "Postgres",
            offset_us: offset_ms * 1000,
            uncertainty_us: uncertainty_ms * 1000,
        }
    }

    #[test]
    fn small_skew_passes_silently() {
        let cfg = ClockConfig::default();
        assert!(
            evaluate(&[sample(200, 1), sample(-300, 1)], &cfg)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn moderate_skew_warns_but_starts() {
        let cfg = ClockConfig::default();
        let warn = evaluate(&[sample(-2_500, 1)], &cfg).unwrap();
        assert_eq!(warn.len(), 1);
    }

    #[test]
    fn large_skew_refuses_with_the_escape_hatch_named() {
        let err = evaluate(&[sample(7_200, 1)], &ClockConfig::default()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("7.2 s behind Postgres"), "{msg}");
        assert!(msg.contains("BROCCOLI__CLOCK__MAX_SKEW_MS=0"), "{msg}");
    }

    #[test]
    fn a_slow_round_trip_does_not_cause_a_refusal() {
        // 5.5 s apparent offset, but the reading could be 1 s off either way.
        assert!(evaluate(&[sample(5_500, 1_000)], &ClockConfig::default()).is_ok());
    }

    #[test]
    fn max_zero_disables_the_refusal_but_still_warns() {
        let cfg = ClockConfig {
            max_skew_ms: 0,
            ..ClockConfig::default()
        };
        assert_eq!(evaluate(&[sample(3_600_000, 1)], &cfg).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn take_measures_offset_against_the_remote_reading() {
        let ahead_us = 10_000_000;
        let s = SkewSample::take("Redis", async move {
            Ok(chrono::Utc::now().timestamp_micros() + ahead_us)
        })
        .await
        .unwrap();
        assert!((s.offset_us - ahead_us).abs() < 100_000, "{s:?}");
    }
}
