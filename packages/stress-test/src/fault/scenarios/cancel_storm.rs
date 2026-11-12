use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use common::cancel::{RedisCancelChecker, cancel_batch_key};
use hdrhistogram::Histogram;
use tokio::sync::Mutex;

use super::{Scenario, ScenarioContext, ScenarioOutcome};
use crate::fault::transcript::{EventKind, Severity, Transcript};

pub struct CancelStorm {
    pub batch_count: usize,
    pub readers: usize,
    pub key_ttl_secs: u64,
}

impl Default for CancelStorm {
    fn default() -> Self {
        Self {
            batch_count: 1000,
            readers: 64,
            key_ttl_secs: 21600,
        }
    }
}

struct PhaseTotals {
    hits: u64,
    misses: u64,
    errors: u64,
    elapsed_ms: u128,
    hist: Histogram<u64>,
}

#[async_trait]
impl Scenario for CancelStorm {
    fn name(&self) -> &'static str {
        "cancel_storm"
    }

    async fn run(&self, ctx: &ScenarioContext) -> anyhow::Result<ScenarioOutcome> {
        let mut transcript = Transcript::new(self.name())
            .with_param("batch_count", self.batch_count.into())
            .with_param("readers", self.readers.into())
            .with_param("key_ttl_secs", self.key_ttl_secs.into());

        transcript.record(
            EventKind::Setup,
            Severity::Info,
            "scenario started; preparing batch ids",
        );

        // Half the batch_ids will be primed with a cancel key (even indices),
        // half will not (odd indices). Readers probe every id; we then assert
        // that hits/misses match the priming pattern exactly.
        let batch_ids: Vec<String> = (0..self.batch_count)
            .map(|i| format!("storm-{i:06}"))
            .collect();
        let primed_indices: Vec<bool> = (0..self.batch_count).map(|i| i % 2 == 0).collect();
        let expected_hits_per_reader = primed_indices.iter().filter(|p| **p).count() as u64;
        let expected_misses_per_reader = primed_indices.iter().filter(|p| !**p).count() as u64;

        // Phase 1: prime half the batch keys.
        let client = redis::Client::open(ctx.redis_url.as_str())?;
        let prime_start = Instant::now();
        {
            let mut conn = client.get_multiplexed_async_connection().await?;
            for chunk in batch_ids
                .iter()
                .zip(primed_indices.iter())
                .filter(|(_, primed)| **primed)
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
                .chunks(500)
            {
                let mut pipe = redis::pipe();
                for id in chunk {
                    pipe.cmd("SET")
                        .arg(cancel_batch_key(id))
                        .arg("1")
                        .arg("EX")
                        .arg(self.key_ttl_secs);
                }
                let _: () = pipe.query_async(&mut conn).await?;
            }
        }
        let prime_ms = prime_start.elapsed().as_millis();
        transcript.record_with(
            EventKind::Inject,
            Severity::Info,
            "primed half of cancel batch keys",
            [
                ("prime_ms".into(), serde_json::json!(prime_ms as u64)),
                (
                    "primed_count".into(),
                    serde_json::json!(expected_hits_per_reader),
                ),
                (
                    "unprimed_count".into(),
                    serde_json::json!(expected_misses_per_reader),
                ),
            ]
            .into_iter()
            .collect(),
        );

        // Phase 2: readers probe every id via RedisCancelChecker (the same
        // helper workers use in prod). Mixed hit/miss traffic, with batch_id
        // primed half the time and a fresh task_id every probe so the op key
        // never collides with the batch key path.
        let phase_with_keys =
            run_probe_phase(&ctx.redis_url, &batch_ids, &primed_indices, self.readers).await?;
        transcript.record_with(
            EventKind::Observe,
            Severity::Info,
            "phase 1 probes completed (keys primed)",
            phase_fields(&phase_with_keys).into_iter().collect(),
        );

        // Phase 3: clear every primed key and re-probe. All probes must miss.
        // This validates the toggle behavior - workers must observe the
        // cancellation lift, not just the initial assertion.
        let del_start = Instant::now();
        {
            let mut conn = client.get_multiplexed_async_connection().await?;
            for chunk in batch_ids
                .iter()
                .zip(primed_indices.iter())
                .filter(|(_, primed)| **primed)
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
                .chunks(500)
            {
                let mut pipe = redis::pipe();
                for id in chunk {
                    pipe.cmd("DEL").arg(cancel_batch_key(id));
                }
                let _: () = pipe.query_async(&mut conn).await?;
            }
        }
        let del_ms = del_start.elapsed().as_millis();
        transcript.record_with(
            EventKind::Recover,
            Severity::Info,
            "cleared all primed cancel keys",
            [("del_ms".into(), serde_json::json!(del_ms as u64))]
                .into_iter()
                .collect(),
        );

        let all_unprimed = vec![false; self.batch_count];
        let phase_without_keys =
            run_probe_phase(&ctx.redis_url, &batch_ids, &all_unprimed, self.readers).await?;
        transcript.record_with(
            EventKind::Observe,
            Severity::Info,
            "phase 2 probes completed (keys cleared)",
            phase_fields(&phase_without_keys).into_iter().collect(),
        );

        // Assertions.
        let p1_expected_hits = expected_hits_per_reader * self.readers as u64;
        let p1_expected_misses = expected_misses_per_reader * self.readers as u64;
        let p2_expected_misses = self.batch_count as u64 * self.readers as u64;

        let p1_ok = phase_with_keys.errors == 0
            && phase_with_keys.hits == p1_expected_hits
            && phase_with_keys.misses == p1_expected_misses;
        let p2_ok = phase_without_keys.errors == 0
            && phase_without_keys.hits == 0
            && phase_without_keys.misses == p2_expected_misses;
        let passed = p1_ok && p2_ok;

        if p1_ok {
            transcript.record(
                EventKind::Assertion,
                Severity::Info,
                format!(
                    "phase 1: {} hits and {} misses match priming pattern",
                    p1_expected_hits, p1_expected_misses
                ),
            );
        } else {
            transcript.record(
                EventKind::Assertion,
                Severity::Error,
                format!(
                    "phase 1: expected {} hits and {} misses, got {} hits, {} misses, {} errors",
                    p1_expected_hits,
                    p1_expected_misses,
                    phase_with_keys.hits,
                    phase_with_keys.misses,
                    phase_with_keys.errors
                ),
            );
        }
        if p2_ok {
            transcript.record(
                EventKind::Assertion,
                Severity::Info,
                format!(
                    "phase 2: all {} probes miss after DEL (cancellation lifted)",
                    p2_expected_misses
                ),
            );
        } else {
            transcript.record(
                EventKind::Assertion,
                Severity::Error,
                format!(
                    "phase 2: expected 0 hits and {} misses, got {} hits, {} misses, {} errors",
                    p2_expected_misses,
                    phase_without_keys.hits,
                    phase_without_keys.misses,
                    phase_without_keys.errors
                ),
            );
        }

        transcript.set_summary("prime_ms", serde_json::json!(prime_ms as u64));
        transcript.set_summary("del_ms", serde_json::json!(del_ms as u64));
        transcript.set_summary(
            "phase1",
            serde_json::Value::Object(serde_json::Map::from_iter(phase_fields(&phase_with_keys))),
        );
        transcript.set_summary(
            "phase2",
            serde_json::Value::Object(serde_json::Map::from_iter(phase_fields(
                &phase_without_keys,
            ))),
        );

        transcript.record(EventKind::Summary, Severity::Info, "scenario complete");
        transcript.finish(passed);

        Ok(ScenarioOutcome { passed, transcript })
    }
}

async fn run_probe_phase(
    redis_url: &str,
    batch_ids: &[String],
    primed: &[bool],
    readers: usize,
) -> anyhow::Result<PhaseTotals> {
    let hist = Arc::new(Mutex::new(Histogram::<u64>::new(3)?));
    let hits = Arc::new(Mutex::new(0u64));
    let misses = Arc::new(Mutex::new(0u64));
    let errors = Arc::new(Mutex::new(0u64));

    let mut handles = Vec::with_capacity(readers);
    let phase_start = Instant::now();
    for reader_idx in 0..readers {
        let url = redis_url.to_string();
        let batch_ids = batch_ids.to_vec();
        let primed = primed.to_vec();
        let hist = Arc::clone(&hist);
        let hits = Arc::clone(&hits);
        let misses = Arc::clone(&misses);
        let errors = Arc::clone(&errors);
        handles.push(tokio::spawn(async move {
            let checker = RedisCancelChecker::new(&url)?;
            let stride = batch_ids.len().div_ceil(64).max(1);
            let start = reader_idx * stride % batch_ids.len();
            for offset in 0..batch_ids.len() {
                let idx = (start + offset) % batch_ids.len();
                let batch_id = &batch_ids[idx];
                let task_id = format!("op-{reader_idx:04}-{offset:06}");
                let expect_hit = primed[idx];

                let t = Instant::now();
                let cancelled = checker.is_cancelled(Some(batch_id), &task_id).await;
                let micros = t.elapsed().as_micros() as u64;
                hist.lock().await.record(micros).ok();

                match cancelled {
                    Ok(true) if expect_hit => *hits.lock().await += 1,
                    Ok(false) if !expect_hit => *misses.lock().await += 1,
                    Ok(true) => {
                        // Saw cancellation where none was primed; record as miss-expected error.
                        *errors.lock().await += 1;
                    }
                    Ok(false) => {
                        // Missed a primed cancel key; record as hit-expected error.
                        *errors.lock().await += 1;
                    }
                    Err(_) => *errors.lock().await += 1,
                }
            }
            Ok::<_, anyhow::Error>(())
        }));
    }

    let mut join_errors = 0u64;
    for h in handles {
        match h.await {
            Ok(Ok(())) => {}
            Ok(Err(_)) | Err(_) => join_errors += 1,
        }
    }
    let elapsed_ms = phase_start.elapsed().as_millis();
    let hist = hist.lock().await.clone();
    let hits = *hits.lock().await;
    let misses = *misses.lock().await;
    let errors = *errors.lock().await + join_errors;

    // Classify probes that returned the wrong polarity as errors. A reader that
    // saw a hit where it expected a miss (or vice versa) is recorded above; the
    // hits/misses counters only increment on agreement, so the totals add up:
    //   hits + misses + errors == readers * batch_count
    Ok(PhaseTotals {
        hits,
        misses,
        errors,
        elapsed_ms,
        hist,
    })
}

fn phase_fields(p: &PhaseTotals) -> Vec<(String, serde_json::Value)> {
    vec![
        ("elapsed_ms".into(), serde_json::json!(p.elapsed_ms as u64)),
        ("hits".into(), serde_json::json!(p.hits)),
        ("misses".into(), serde_json::json!(p.misses)),
        ("errors".into(), serde_json::json!(p.errors)),
        (
            "p50_us".into(),
            serde_json::json!(p.hist.value_at_quantile(0.50)),
        ),
        (
            "p95_us".into(),
            serde_json::json!(p.hist.value_at_quantile(0.95)),
        ),
        (
            "p99_us".into(),
            serde_json::json!(p.hist.value_at_quantile(0.99)),
        ),
        ("max_us".into(), serde_json::json!(p.hist.max())),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    #[tokio::test]
    #[ignore = "requires Docker to start a Redis testcontainer"]
    async fn cancel_storm_validates_hit_miss_and_toggle() {
        let redis = Redis::default()
            .start()
            .await
            .expect("failed to start Redis container");
        let port = redis
            .get_host_port_ipv4(6379)
            .await
            .expect("failed to get Redis port");
        let ctx = ScenarioContext {
            redis_url: format!("redis://127.0.0.1:{port}"),
            db_url: None,
            kill_command: None,
        };
        let scenario = CancelStorm {
            batch_count: 40,
            readers: 4,
            key_ttl_secs: 300,
        };
        let outcome = scenario.run(&ctx).await.expect("scenario failed to run");
        assert!(
            outcome.passed,
            "scenario should pass: {:?}",
            outcome.transcript.summary
        );
    }
}
