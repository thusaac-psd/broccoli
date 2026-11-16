//! Burst submission scenario with a configurable contest-type mix.
//!
//! Posts N submissions in parallel (default 1000) across a user-defined mix of
//! contest types (e.g. `icpc:70,ioi:30`), one contest + problem per type, then
//! waits for terminal verdicts and reports per-type stats. Implements the §0
//! roadmap item "N=1000 in-flight submissions with a configurable problem-type
//! mix (ICPC, IOI)".
//!
//! Plugin discovery is dynamic: requested types that aren't in
//! `/api/v1/plugins/registries` are dropped with a warning (or hard-failed when
//! `--strict`). If `type_weights` is empty the scenario auto-selects all
//! available contest types with equal weight.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use futures::stream::{FuturesUnordered, StreamExt};
use hdrhistogram::Histogram;
use rand::SeedableRng;
use rand::rngs::StdRng;
use tokio::sync::{Mutex, Semaphore};

use crate::bootstrap::{build_create_contest_request, build_create_problem_request, contest_label};
use crate::client::{AuthCreds, Client};
#[cfg(test)]
use crate::dto::SubmissionStatus;
use crate::dto::{
    AddContestProblemRequest, CreateSubmissionRequest, CreateTestCaseRequest, RegistriesResponse,
    SubmissionFileDto,
};
use crate::fault::transcript::{EventKind, Severity, Transcript};
use crate::scenarios::SCENARIOS;

use super::ScenarioOutcome;

/// Required terminal-rate (fraction in [0, 1]) for the scenario to PASS.
const PASS_TERMINAL_RATE: f64 = 0.95;

/// Configuration for the burst scenario. Built from CLI args in `runner.rs`.
pub struct Burst {
    pub submission_count: usize,
    pub concurrency: usize,
    /// Contest-type weights (e.g. `[("icpc", 70), ("ioi", 30)]`). Empty = auto.
    pub type_weights: Vec<(String, u32)>,
    pub strict: bool,
    pub terminal_deadline_secs: u64,
    pub metrics_url: Option<String>,
    pub fanout_test_cases_per_problem: usize,
    pub baseline_plugin_pool_contention_delta: Option<u64>,
    pub max_plugin_pool_contention_baseline_ratio: f64,
    pub max_plugin_pool_contention_delta: u64,
    pub require_fanout_saturation: bool,
    pub keep_fixtures: bool,
    pub contest_type: Option<String>,
    pub problem_type: Option<String>,

    // Connection params for the client the scenario builds.
    pub server_url: String,
    pub creds: AuthCreds,
    pub out: PathBuf,
}

impl Default for Burst {
    fn default() -> Self {
        Self {
            submission_count: 1000,
            concurrency: 64,
            type_weights: Vec::new(),
            strict: false,
            terminal_deadline_secs: 300,
            metrics_url: None,
            fanout_test_cases_per_problem: 1,
            baseline_plugin_pool_contention_delta: None,
            max_plugin_pool_contention_baseline_ratio: 0.2,
            max_plugin_pool_contention_delta: 10,
            require_fanout_saturation: false,
            keep_fixtures: false,
            contest_type: None,
            problem_type: None,
            server_url: String::new(),
            creds: AuthCreds::Token(String::new()),
            out: PathBuf::from("transcript.json"),
        }
    }
}

/// Parse a `--type-weights` CLI string like `"icpc:70, ioi:30"` into the
/// `(name, weight)` pairs the scenario expects. Empty input returns an empty
/// vec (meaning "auto-pick"). Malformed input is a hard error.
pub fn parse_type_weights(input: &str) -> anyhow::Result<Vec<(String, u32)>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for raw in trimmed.split(',') {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        let (name, weight_str) = entry
            .split_once(':')
            .with_context(|| format!("expected `name:weight`, got `{entry}`"))?;
        let name = name.trim();
        let weight_str = weight_str.trim();
        if name.is_empty() {
            anyhow::bail!("type name is empty in entry `{entry}`");
        }
        let weight: u32 = weight_str
            .parse()
            .with_context(|| format!("invalid weight `{weight_str}` in entry `{entry}`"))?;
        if weight == 0 {
            anyhow::bail!("weight must be > 0 in entry `{entry}`");
        }
        out.push((name.to_string(), weight));
    }
    Ok(out)
}

/// Result of cross-checking the requested mix against the live registries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredMix {
    pub kept: Vec<(String, u32)>,
    pub dropped: Vec<String>,
}

/// Cross-check the requested mix against the live registries. With
/// `requested` empty, returns an equal-weight mix over `available`. With
/// `strict=true` and any requested type missing, returns an error. Otherwise
/// missing types are silently dropped and reported via `FilteredMix.dropped`.
pub fn filter_and_renormalize_weights(
    requested: &[(String, u32)],
    available: &[String],
    strict: bool,
) -> anyhow::Result<FilteredMix> {
    if requested.is_empty() {
        if available.is_empty() {
            anyhow::bail!("no contest types available on the server");
        }
        return Ok(FilteredMix {
            kept: available.iter().map(|t| (t.clone(), 1)).collect(),
            dropped: Vec::new(),
        });
    }

    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for (name, weight) in requested {
        if available.iter().any(|t| t == name) {
            kept.push((name.clone(), *weight));
        } else {
            dropped.push(name.clone());
        }
    }

    if strict && !dropped.is_empty() {
        anyhow::bail!(
            "requested types not registered on server (strict): [{}]",
            dropped.join(", ")
        );
    }

    if kept.is_empty() {
        anyhow::bail!(
            "no requested types matched the server registry; available: [{}]",
            available.join(", ")
        );
    }

    Ok(FilteredMix { kept, dropped })
}

/// Weighted pick by index. Returns the index of the chosen entry in `weights`.
/// `total_weight` must equal `weights.iter().map(|(_, w)| *w as u64).sum()` and
/// be > 0; precomputing it avoids re-summing on every call.
pub fn weighted_pick<R: rand::Rng>(
    rng: &mut R,
    weights: &[(String, u32)],
    total_weight: u64,
) -> usize {
    debug_assert!(total_weight > 0, "total_weight must be > 0");
    debug_assert!(!weights.is_empty(), "weights must be non-empty");
    let pick = rng.random_range(0..total_weight);
    let mut acc: u64 = 0;
    for (i, (_, w)) in weights.iter().enumerate() {
        acc += *w as u64;
        if pick < acc {
            return i;
        }
    }
    unreachable!("pick < total_weight and weights cover total_weight; loop must return")
}

/// Per-type aggregated stats, emitted in the transcript summary.
#[derive(Debug, Default)]
struct TypeStats {
    sent: u64,
    send_failed: u64,
    terminal: u64,
    hist: Option<Histogram<u64>>,
}

fn new_hist() -> Histogram<u64> {
    // Bounds match load.rs: 1ms..10min, 3 sig figs. Terminal latency under a
    // burst exceeds the load.rs upper bound; lift to 30min to be safe.
    Histogram::<u64>::new_with_bounds(1, 1_800_000, 3).expect("static histogram bounds are valid")
}

#[derive(Debug, Clone, Copy)]
struct MetricsSnapshot {
    plugin_pool_contention: u64,
    fanout_saturated: u64,
}

#[derive(Debug, Clone, Copy)]
struct MetricsDelta {
    plugin_pool_contention: u64,
    fanout_saturated: u64,
}

impl MetricsSnapshot {
    async fn scrape(url: &str) -> anyhow::Result<Self> {
        let text = reqwest::get(url).await?.error_for_status()?.text().await?;
        Ok(Self {
            plugin_pool_contention: prometheus_counter_sum(
                &text,
                "broccoli_plugin_pool_contention_total",
            )?,
            fanout_saturated: prometheus_counter_sum(
                &text,
                "broccoli_evaluate_batch_fanout_saturated_total",
            )?,
        })
    }

    fn delta(self, before: &Self) -> MetricsDelta {
        MetricsDelta {
            plugin_pool_contention: self
                .plugin_pool_contention
                .saturating_sub(before.plugin_pool_contention),
            fanout_saturated: self
                .fanout_saturated
                .saturating_sub(before.fanout_saturated),
        }
    }
}

fn prometheus_counter_sum(metrics_text: &str, metric_name: &str) -> anyhow::Result<u64> {
    let mut saw_sample = false;
    let mut sum = 0u64;
    for line in metrics_text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !line.starts_with(metric_name) {
            continue;
        }
        let Some((series, value)) = line.rsplit_once(char::is_whitespace) else {
            continue;
        };
        let series_name = series
            .split_once('{')
            .map(|(name, _)| name)
            .unwrap_or(series)
            .trim();
        if series_name != metric_name {
            continue;
        }
        saw_sample = true;
        let value = value
            .parse::<f64>()
            .map_err(|e| anyhow::anyhow!("invalid Prometheus value for {metric_name}: {e}"))?;
        sum = sum.saturating_add(value as u64);
    }

    if !saw_sample {
        anyhow::bail!("required Prometheus counter {metric_name} is missing");
    }
    Ok(sum)
}

impl Burst {
    pub fn name(&self) -> &'static str {
        "burst"
    }

    pub async fn run(&self) -> anyhow::Result<ScenarioOutcome> {
        let mut transcript = Transcript::new(self.name())
            .with_param("submission_count", self.submission_count.into())
            .with_param("concurrency", self.concurrency.into())
            .with_param("strict", self.strict.into())
            .with_param("terminal_deadline_secs", self.terminal_deadline_secs.into())
            .with_param(
                "fanout_test_cases_per_problem",
                self.fanout_test_cases_per_problem.into(),
            )
            .with_param("keep_fixtures", self.keep_fixtures.into());
        if let Some(metrics_url) = &self.metrics_url {
            transcript
                .params
                .insert("metrics_url".into(), serde_json::json!(metrics_url));
        }

        // ---- Setup: client, registries, type-mix validation. -----------
        let client = match Client::new(self.server_url.clone(), self.creds.clone()).await {
            Ok(c) => c,
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("client construction failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        };

        let registries: RegistriesResponse = match client.list_registries().await {
            Ok(r) => r,
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("list_registries failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        };

        let metrics_before = match self.metrics_url.as_deref() {
            Some(url) => match MetricsSnapshot::scrape(url).await {
                Ok(snapshot) => Some(snapshot),
                Err(e) => {
                    transcript.record(
                        EventKind::Setup,
                        Severity::Error,
                        format!("metrics scrape before burst failed: {e}"),
                    );
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            },
            None => None,
        };

        // Honor --contest-type when --type-weights is absent: it overrides the
        // "auto = all available" expansion to a single-type mix. With explicit
        // --type-weights the override is ignored (the CLI doc-comment promises
        // exactly this), so the user's mix wins.
        let effective_requested: Vec<(String, u32)> = if self.type_weights.is_empty() {
            match self.contest_type.as_deref() {
                Some(t) => vec![(t.to_string(), 1)],
                None => Vec::new(),
            }
        } else {
            self.type_weights.clone()
        };

        let filtered = match filter_and_renormalize_weights(
            &effective_requested,
            &registries.contest_types,
            self.strict,
        ) {
            Ok(f) => f,
            Err(e) => {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("type mix validation failed: {e}"),
                );
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }
        };

        for dropped in &filtered.dropped {
            transcript.record(
                EventKind::Setup,
                Severity::Warn,
                format!("type `{dropped}` requested but not registered; dropping"),
            );
        }

        let mix = filtered.kept;
        transcript.params.insert(
            "type_mix".to_string(),
            serde_json::json!(
                mix.iter()
                    .map(|(n, w)| serde_json::json!({"type": n, "weight": w}))
                    .collect::<Vec<_>>()
            ),
        );

        let problem_type =
            match crate::bootstrap::resolve_problem_type(&registries, self.problem_type.as_deref())
            {
                Ok(problem_type) => problem_type,
                Err(error) => {
                    transcript.record(EventKind::Setup, Severity::Error, error.to_string());
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            };

        // ---- Bootstrap: one contest + one problem per type. ----------------
        let scenario = &SCENARIOS[0]; // ab-cpp-ac
        let mut contest_ids: HashMap<String, i32> = HashMap::new();
        let mut problem_ids: HashMap<String, i32> = HashMap::new();

        for (idx, (ctype, _w)) in mix.iter().enumerate() {
            let contest_req = build_create_contest_request(ctype);
            let contest = match client.create_contest(&contest_req).await {
                Ok(c) => c,
                Err(e) => {
                    transcript.record(
                        EventKind::Setup,
                        Severity::Error,
                        format!("create_contest({ctype}) failed: {e}"),
                    );
                    self.cleanup_contests(&client, &contest_ids).await;
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            };

            let problem_req = build_create_problem_request(scenario, ctype, &problem_type);
            let problem = match client.create_problem(&problem_req).await {
                Ok(p) => p,
                Err(e) => {
                    transcript.record(
                        EventKind::Setup,
                        Severity::Error,
                        format!("create_problem({ctype}) failed: {e}"),
                    );
                    contest_ids.insert(ctype.clone(), contest.id);
                    self.cleanup_contests(&client, &contest_ids).await;
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            };

            let testcase_count = self.fanout_test_cases_per_problem.max(1);
            let testcase_score = (100 / testcase_count as i32).max(1);
            for case_idx in 0..testcase_count {
                let tc_req = CreateTestCaseRequest {
                    input: scenario.test_input.to_string(),
                    expected_output: scenario.test_expected_output.to_string(),
                    score: testcase_score,
                    is_sample: false,
                    position: None,
                    description: None,
                    label: Some(format!("fanout_{}", case_idx + 1)),
                };
                if let Err(e) = client.create_test_case(problem.id, &tc_req).await {
                    transcript.record(
                        EventKind::Setup,
                        Severity::Error,
                        format!("create_test_case({ctype}, #{}) failed: {e}", case_idx + 1),
                    );
                    contest_ids.insert(ctype.clone(), contest.id);
                    self.cleanup_contests(&client, &contest_ids).await;
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            }

            let attach = AddContestProblemRequest {
                problem_id: problem.id,
                label: contest_label(idx),
                position: None,
            };
            if let Err(e) = client.add_problem_to_contest(contest.id, &attach).await {
                transcript.record(
                    EventKind::Setup,
                    Severity::Error,
                    format!("add_problem_to_contest({ctype}) failed: {e}"),
                );
                contest_ids.insert(ctype.clone(), contest.id);
                self.cleanup_contests(&client, &contest_ids).await;
                transcript.finish(false);
                return Ok(ScenarioOutcome {
                    passed: false,
                    transcript,
                });
            }

            contest_ids.insert(ctype.clone(), contest.id);
            problem_ids.insert(ctype.clone(), problem.id);
        }

        transcript.record_with(
            EventKind::Setup,
            Severity::Info,
            "bootstrap complete",
            BTreeMap::from_iter([
                (
                    "contest_ids".into(),
                    serde_json::json!(
                        contest_ids
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                            .collect::<BTreeMap<_, _>>()
                    ),
                ),
                (
                    "problem_ids".into(),
                    serde_json::json!(
                        problem_ids
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                            .collect::<BTreeMap<_, _>>()
                    ),
                ),
                ("problem_type".into(), serde_json::json!(problem_type)),
            ]),
        );

        // ---- Inject: spawn N submissions with bounded concurrency. ---------
        let total_weight: u64 = mix.iter().map(|(_, w)| *w as u64).sum();
        let mut rng = StdRng::seed_from_u64(0xb20cc011_u64);
        // Pre-compute the type assignment for each submission so the inject
        // loop is deterministic and the per-type expected counts are exact.
        let assignments: Vec<usize> = (0..self.submission_count)
            .map(|_| weighted_pick(&mut rng, &mix, total_weight))
            .collect();

        let sub_files: Vec<SubmissionFileDto> = scenario
            .files
            .iter()
            .map(|(filename, content)| SubmissionFileDto {
                filename: (*filename).to_string(),
                content: (*content).to_string(),
            })
            .collect();

        let inject_start = Instant::now();
        let semaphore = Arc::new(Semaphore::new(self.concurrency.max(1)));
        // Use mutex-wrapped vec for ordered collection across spawned tasks.
        let posted: Arc<Mutex<Vec<(i32, usize, Instant)>>> =
            Arc::new(Mutex::new(Vec::with_capacity(self.submission_count)));
        let send_fail_counts: Arc<Mutex<HashMap<usize, u64>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let mut futs = FuturesUnordered::new();
        for type_idx in assignments.iter() {
            // Note: the semaphore is owned by this run() scope and never closed
            // by any other task, so acquire_owned cannot fail here. If a future
            // refactor introduces a close() site, switch to expect() or fold
            // the cleanup into a Drop/scopeguard so contest fixtures aren't
            // leaked on the error path.
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(p) => p,
                Err(e) => {
                    transcript.record(
                        EventKind::Inject,
                        Severity::Error,
                        format!("semaphore closed unexpectedly: {e}"),
                    );
                    self.cleanup_contests(&client, &contest_ids).await;
                    transcript.finish(false);
                    return Ok(ScenarioOutcome {
                        passed: false,
                        transcript,
                    });
                }
            };
            let ctype = &mix[*type_idx].0;
            let contest_id = *contest_ids
                .get(ctype)
                .expect("contest_id for kept type exists");
            let problem_id = *problem_ids
                .get(ctype)
                .expect("problem_id for kept type exists");
            let client = client.clone();
            let req = CreateSubmissionRequest {
                files: sub_files.clone(),
                language: scenario.language.to_string(),
                contest_type: Some(ctype.clone()),
            };
            let posted = Arc::clone(&posted);
            let send_fail_counts = Arc::clone(&send_fail_counts);
            let type_idx = *type_idx;
            futs.push(tokio::spawn(async move {
                let _permit = permit;
                let sent_at = Instant::now();
                let outcome = client
                    .create_contest_submission(contest_id, problem_id, &req)
                    .await;
                match outcome {
                    Ok(s) => {
                        posted.lock().await.push((s.id, type_idx, sent_at));
                    }
                    Err(_) => {
                        *send_fail_counts.lock().await.entry(type_idx).or_insert(0) += 1;
                    }
                }
            }));
        }

        let mut inject_panics: u64 = 0;
        while let Some(join_result) = futs.next().await {
            if let Err(e) = join_result {
                inject_panics += 1;
                tracing::warn!(error = %e, "inject task panicked");
            }
        }
        if inject_panics > 0 {
            transcript.record_with(
                EventKind::Inject,
                Severity::Warn,
                format!("{inject_panics} inject task(s) panicked"),
                BTreeMap::from_iter([("panic_count".into(), serde_json::json!(inject_panics))]),
            );
        }

        let inject_elapsed_ms = inject_start.elapsed().as_millis() as u64;
        let posted = Arc::try_unwrap(posted)
            .map(|m| m.into_inner())
            .unwrap_or_default();
        let send_fail_counts = Arc::try_unwrap(send_fail_counts)
            .map(|m| m.into_inner())
            .unwrap_or_default();
        let total_send_failures: u64 = send_fail_counts.values().sum();

        if total_send_failures > 0 {
            let by_type: BTreeMap<String, u64> = send_fail_counts
                .iter()
                .map(|(idx, n)| (mix[*idx].0.clone(), *n))
                .collect();
            transcript.record_with(
                EventKind::Inject,
                Severity::Warn,
                format!("{total_send_failures} submission POST(s) failed"),
                BTreeMap::from_iter([("send_failures_by_type".into(), serde_json::json!(by_type))]),
            );
        }
        transcript.record_with(
            EventKind::Inject,
            Severity::Info,
            "inject phase complete",
            BTreeMap::from_iter([
                (
                    "inject_elapsed_ms".into(),
                    serde_json::json!(inject_elapsed_ms),
                ),
                ("posted".into(), serde_json::json!(posted.len())),
                ("failed_post".into(), serde_json::json!(total_send_failures)),
            ]),
        );

        // ---- Observe: poll for terminal verdicts. --------------------------
        let mut stats: HashMap<usize, TypeStats> = HashMap::new();
        for (idx, _) in mix.iter().enumerate() {
            stats.insert(idx, TypeStats::default());
        }
        // sent count includes only submissions whose POST succeeded.
        for (_id, type_idx, _t) in &posted {
            stats.get_mut(type_idx).expect("type_idx valid").sent += 1;
        }
        for (type_idx, n) in &send_fail_counts {
            stats.get_mut(type_idx).expect("type_idx valid").send_failed = *n;
        }
        for s in stats.values_mut() {
            s.hist = Some(new_hist());
        }

        let observe_start = Instant::now();
        let deadline = observe_start + Duration::from_secs(self.terminal_deadline_secs);
        let mut pending: HashMap<i32, (usize, Instant)> = posted
            .iter()
            .map(|(id, type_idx, sent_at)| (*id, (*type_idx, *sent_at)))
            .collect();

        while Instant::now() < deadline && !pending.is_empty() {
            // Snapshot ids so we can mutate pending inside the loop body.
            let ids: Vec<i32> = pending.keys().copied().collect();
            for id in ids {
                match client.get_submission(id).await {
                    Ok(resp) => {
                        if resp.status.is_terminal()
                            && let Some((type_idx, sent_at)) = pending.remove(&id)
                        {
                            let latency_ms = sent_at.elapsed().as_millis() as u64;
                            let s = stats.get_mut(&type_idx).expect("type_idx valid");
                            s.terminal += 1;
                            if let Some(h) = s.hist.as_mut() {
                                let _ = h.record(latency_ms.max(1));
                            }
                        }
                    }
                    Err(_) => {
                        // Transient poll failures are tolerated; the loop will
                        // retry next tick. We don't count them per-type to
                        // avoid double-counting against the terminal-rate.
                    }
                }
            }
            if pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        let observe_elapsed_ms = observe_start.elapsed().as_millis() as u64;
        let still_pending = pending.len() as u64;
        let total_posted = posted.len() as u64;
        let total_terminal: u64 = stats.values().map(|s| s.terminal).sum();

        transcript.record_with(
            EventKind::Observe,
            Severity::Info,
            "observe phase complete",
            BTreeMap::from_iter([
                (
                    "observe_elapsed_ms".into(),
                    serde_json::json!(observe_elapsed_ms),
                ),
                ("terminal".into(), serde_json::json!(total_terminal)),
                ("still_pending".into(), serde_json::json!(still_pending)),
            ]),
        );

        // ---- Assertion. -----------------------------------------------------
        // PASS criterion: >=95% of attempted (POST-succeeded) submissions
        // reached terminal within the deadline. Send-failures count against
        // the submission_count budget too - they're indistinguishable from
        // a stuck-pending submission for the user.
        let attempted = self.submission_count as u64;
        let terminal_rate = if attempted > 0 {
            (total_terminal as f64) / (attempted as f64)
        } else {
            0.0
        };
        let mut metrics_required_failed = false;
        let metrics_delta = match (self.metrics_url.as_deref(), metrics_before.as_ref()) {
            (Some(url), Some(before)) => match MetricsSnapshot::scrape(url).await {
                Ok(after) => {
                    let delta = after.delta(before);
                    transcript.record_with(
                        EventKind::Observe,
                        Severity::Info,
                        "metrics delta captured",
                        BTreeMap::from_iter([
                            (
                                "plugin_pool_contention_delta".into(),
                                serde_json::json!(delta.plugin_pool_contention),
                            ),
                            (
                                "fanout_saturated_delta".into(),
                                serde_json::json!(delta.fanout_saturated),
                            ),
                        ]),
                    );
                    Some(delta)
                }
                Err(e) => {
                    transcript.record(
                        EventKind::Assertion,
                        Severity::Error,
                        format!("metrics scrape after burst failed: {e}"),
                    );
                    metrics_required_failed = true;
                    None
                }
            },
            _ => None,
        };

        let mut passed = terminal_rate >= PASS_TERMINAL_RATE;
        if metrics_required_failed {
            passed = false;
        }
        if passed {
            transcript.record(
                EventKind::Assertion,
                Severity::Info,
                format!(
                    "{}/{} ({:.2}%) terminal within {}s — pass threshold {:.0}%",
                    total_terminal,
                    attempted,
                    terminal_rate * 100.0,
                    self.terminal_deadline_secs,
                    PASS_TERMINAL_RATE * 100.0,
                ),
            );
        } else {
            transcript.record(
                EventKind::Assertion,
                Severity::Error,
                format!(
                    "only {}/{} ({:.2}%) terminal within {}s — below pass threshold {:.0}%",
                    total_terminal,
                    attempted,
                    terminal_rate * 100.0,
                    self.terminal_deadline_secs,
                    PASS_TERMINAL_RATE * 100.0,
                ),
            );
        }

        if let Some(delta) = metrics_delta {
            let mut contention_cap = self.max_plugin_pool_contention_delta;
            if let Some(baseline) = self.baseline_plugin_pool_contention_delta {
                let baseline_cap = ((baseline as f64)
                    * self.max_plugin_pool_contention_baseline_ratio)
                    .ceil() as u64;
                contention_cap = contention_cap.min(baseline_cap.max(1));
            }

            if delta.plugin_pool_contention > contention_cap {
                passed = false;
                transcript.record(
                    EventKind::Assertion,
                    Severity::Error,
                    format!(
                        "plugin_pool_contention_total delta {} exceeds cap {}",
                        delta.plugin_pool_contention, contention_cap
                    ),
                );
            } else {
                transcript.record(
                    EventKind::Assertion,
                    Severity::Info,
                    format!(
                        "plugin_pool_contention_total delta {} within cap {}",
                        delta.plugin_pool_contention, contention_cap
                    ),
                );
            }

            if self.require_fanout_saturation && delta.fanout_saturated == 0 {
                passed = false;
                transcript.record(
                    EventKind::Assertion,
                    Severity::Error,
                    "fanout saturation was not observed; burst did not prove semaphore engagement",
                );
            }
        }

        // ---- Recover: tear down fixtures unless keep_fixtures. -------------
        if self.keep_fixtures {
            transcript.record_with(
                EventKind::Recover,
                Severity::Info,
                "keep_fixtures=true; leaving contests in place",
                BTreeMap::from_iter([(
                    "contest_ids".into(),
                    serde_json::json!(
                        contest_ids
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::json!(v)))
                            .collect::<BTreeMap<_, _>>()
                    ),
                )]),
            );
        } else {
            self.cleanup_contests(&client, &contest_ids).await;
            transcript.record(
                EventKind::Recover,
                Severity::Info,
                "deleted scratch contests",
            );
        }

        // ---- Summary. ------------------------------------------------------
        let per_type_summary: BTreeMap<String, serde_json::Value> = stats
            .iter()
            .map(|(idx, s)| {
                let name = mix[*idx].0.clone();
                let hist = s.hist.as_ref();
                let (p50, p95, p99, max) = match hist {
                    Some(h) if !h.is_empty() => (
                        h.value_at_quantile(0.50),
                        h.value_at_quantile(0.95),
                        h.value_at_quantile(0.99),
                        h.max(),
                    ),
                    _ => (0, 0, 0, 0),
                };
                (
                    name,
                    serde_json::json!({
                        "sent": s.sent,
                        "send_failed": s.send_failed,
                        "terminal": s.terminal,
                        "terminal_rate": if s.sent > 0 {
                            (s.terminal as f64) / (s.sent as f64)
                        } else { 0.0 },
                        "latency_ms": {
                            "p50": p50,
                            "p95": p95,
                            "p99": p99,
                            "max": max,
                        }
                    }),
                )
            })
            .collect();

        transcript.set_summary("submission_count", serde_json::json!(attempted));
        transcript.set_summary("posted", serde_json::json!(total_posted));
        transcript.set_summary("send_failures", serde_json::json!(total_send_failures));
        transcript.set_summary("terminal", serde_json::json!(total_terminal));
        transcript.set_summary("still_pending", serde_json::json!(still_pending));
        transcript.set_summary("terminal_rate", serde_json::json!(terminal_rate));
        transcript.set_summary("inject_elapsed_ms", serde_json::json!(inject_elapsed_ms));
        transcript.set_summary("observe_elapsed_ms", serde_json::json!(observe_elapsed_ms));
        transcript.set_summary("per_type", serde_json::json!(per_type_summary));

        transcript.record(EventKind::Summary, Severity::Info, "scenario complete");
        transcript.finish(passed);

        Ok(ScenarioOutcome { passed, transcript })
    }

    async fn cleanup_contests(&self, client: &Client, contest_ids: &HashMap<String, i32>) {
        for (ctype, id) in contest_ids {
            if let Err(e) = client.delete_contest(*id).await {
                tracing::warn!(
                    contest_type = %ctype,
                    contest_id = id,
                    error = %e,
                    "failed to delete scratch contest during cleanup",
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{contest_json, problem_json, submission_json, test_case_json};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};

    use rand::SeedableRng;
    use rand::rngs::StdRng;
    use serde_json::json;
    use wiremock::matchers::{method, path, path_regex};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    #[test]
    fn parse_type_weights_happy_path() {
        let r = parse_type_weights("icpc:70,ioi:30").unwrap();
        assert_eq!(r, vec![("icpc".to_string(), 70), ("ioi".to_string(), 30)]);
    }

    #[test]
    fn parse_type_weights_trims_whitespace() {
        let r = parse_type_weights("  icpc : 70 ,  ioi:30  ").unwrap();
        assert_eq!(r, vec![("icpc".to_string(), 70), ("ioi".to_string(), 30)]);
    }

    #[test]
    fn parse_type_weights_empty_string_returns_empty_vec() {
        let r = parse_type_weights("").unwrap();
        assert!(r.is_empty());
        let r = parse_type_weights("   ").unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn parse_type_weights_missing_colon_errors() {
        let err = parse_type_weights("icpc70").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("expected `name:weight`"), "got: {msg}");
    }

    #[test]
    fn parse_type_weights_non_numeric_weight_errors() {
        let err = parse_type_weights("icpc:lots").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("invalid weight"), "got: {msg}");
    }

    #[test]
    fn parse_type_weights_zero_weight_errors() {
        let err = parse_type_weights("icpc:0").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("weight must be > 0"), "got: {msg}");
    }

    #[test]
    fn parse_type_weights_empty_name_errors() {
        let err = parse_type_weights(":50").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("type name is empty"), "got: {msg}");
    }

    #[test]
    fn parse_type_weights_skips_trailing_comma() {
        let r = parse_type_weights("icpc:70,").unwrap();
        assert_eq!(r, vec![("icpc".to_string(), 70)]);
    }

    #[test]
    fn filter_empty_requested_uses_available_equal_weight() {
        let req: Vec<(String, u32)> = Vec::new();
        let avail = vec!["icpc".to_string(), "ioi".to_string()];
        let f = filter_and_renormalize_weights(&req, &avail, false).unwrap();
        assert_eq!(
            f.kept,
            vec![("icpc".to_string(), 1), ("ioi".to_string(), 1)]
        );
        assert!(f.dropped.is_empty());
    }

    #[test]
    fn filter_empty_requested_with_empty_available_errors() {
        let req: Vec<(String, u32)> = Vec::new();
        let avail: Vec<String> = Vec::new();
        let err = filter_and_renormalize_weights(&req, &avail, false).unwrap_err();
        assert!(format!("{err}").contains("no contest types"));
    }

    #[test]
    fn filter_drops_missing_types_when_not_strict() {
        let req = vec![
            ("icpc".to_string(), 70),
            ("nonsense".to_string(), 30),
            ("ioi".to_string(), 10),
        ];
        let avail = vec!["icpc".to_string(), "ioi".to_string()];
        let f = filter_and_renormalize_weights(&req, &avail, false).unwrap();
        assert_eq!(
            f.kept,
            vec![("icpc".to_string(), 70), ("ioi".to_string(), 10)]
        );
        assert_eq!(f.dropped, vec!["nonsense".to_string()]);
    }

    #[test]
    fn filter_errors_in_strict_mode_when_type_missing() {
        let req = vec![("icpc".to_string(), 70), ("nonsense".to_string(), 30)];
        let avail = vec!["icpc".to_string()];
        let err = filter_and_renormalize_weights(&req, &avail, true).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nonsense"), "got: {msg}");
        assert!(msg.contains("strict"), "got: {msg}");
    }

    #[test]
    fn override_contest_type_with_empty_weights_treated_as_single_entry_mix() {
        // The d711c3a9 fix path: --contest-type X with no --type-weights should
        // be equivalent to --type-weights X:1. Verify by feeding it through
        // filter_and_renormalize_weights the same way run() does.
        let effective_requested = vec![("ioi".to_string(), 1)];
        let avail = vec!["icpc".to_string(), "ioi".to_string()];
        let f = filter_and_renormalize_weights(&effective_requested, &avail, false).unwrap();
        assert_eq!(f.kept, vec![("ioi".to_string(), 1)]);
        assert!(f.dropped.is_empty());
    }

    #[test]
    fn override_contest_type_unregistered_under_strict_errors() {
        // Same path as above but for an unregistered override under --strict;
        // ensures the override participates in the strict policy.
        let effective_requested = vec![("nonsense".to_string(), 1)];
        let avail = vec!["icpc".to_string(), "ioi".to_string()];
        let err = filter_and_renormalize_weights(&effective_requested, &avail, true).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nonsense"), "got: {msg}");
    }

    #[test]
    fn filter_all_missing_non_strict_errors() {
        let req = vec![("foo".to_string(), 70), ("bar".to_string(), 30)];
        let avail = vec!["icpc".to_string()];
        let err = filter_and_renormalize_weights(&req, &avail, false).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("no requested types matched"), "got: {msg}");
    }

    #[test]
    fn weighted_pick_respects_distribution_with_seeded_rng() {
        let mix = vec![("icpc".to_string(), 70), ("ioi".to_string(), 30)];
        let total: u64 = mix.iter().map(|(_, w)| *w as u64).sum();
        let mut rng = StdRng::seed_from_u64(0xDEAD_BEEF);
        let n = 10_000;
        let mut counts = [0u64; 2];
        for _ in 0..n {
            counts[weighted_pick(&mut rng, &mix, total)] += 1;
        }
        let icpc_frac = counts[0] as f64 / n as f64;
        let ioi_frac = counts[1] as f64 / n as f64;
        // 5% tolerance per the spec.
        assert!(
            (icpc_frac - 0.70).abs() < 0.05,
            "icpc fraction {icpc_frac} not within 5% of 0.70"
        );
        assert!(
            (ioi_frac - 0.30).abs() < 0.05,
            "ioi fraction {ioi_frac} not within 5% of 0.30"
        );
    }

    #[test]
    fn weighted_pick_single_entry_always_returns_index_zero() {
        let mix = vec![("only".to_string(), 5)];
        let total: u64 = 5;
        let mut rng = StdRng::seed_from_u64(1);
        for _ in 0..100 {
            assert_eq!(weighted_pick(&mut rng, &mix, total), 0);
        }
    }

    #[test]
    fn prometheus_counter_sum_requires_present_exact_counter() {
        let metrics = r#"
# HELP broccoli_plugin_pool_contention_total total contention events
# TYPE broccoli_plugin_pool_contention_total counter
broccoli_plugin_pool_contention_total{plugin="ioi"} 2
broccoli_plugin_pool_contention_total{plugin="icpc"} 3
broccoli_plugin_pool_contention_total_created 123
"#;

        assert_eq!(
            prometheus_counter_sum(metrics, "broccoli_plugin_pool_contention_total").unwrap(),
            5
        );

        let err = prometheus_counter_sum(metrics, "broccoli_evaluate_batch_fanout_saturated_total")
            .unwrap_err();
        assert!(
            format!("{err}").contains("required Prometheus counter"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn burst_1000_submission_metrics_guard_runs_against_fake_server() {
        let server = MockServer::start().await;
        let metrics_calls = Arc::new(AtomicUsize::new(0));
        let submission_ids = Arc::new(AtomicI32::new(10_000));

        Mock::given(method("GET"))
            .and(path("/api/v1/plugins/registries"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "problem_types": ["batch"],
                "checker_formats": ["exact"],
                "contest_types": ["icpc"],
                "languages": [],
                "evaluators": [],
                "contest_type_handlers": [],
                "hooks": []
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path("/metrics"))
            .respond_with({
                let metrics_calls = Arc::clone(&metrics_calls);
                move |_request: &Request| {
                    let call = metrics_calls.fetch_add(1, Ordering::SeqCst);
                    let (contention, fanout) = if call == 0 { (100, 0) } else { (105, 1) };
                    ResponseTemplate::new(200).set_body_string(format!(
                        "\
# TYPE broccoli_plugin_pool_contention_total counter
broccoli_plugin_pool_contention_total{{plugin_id=\"ioi\"}} {contention}
# TYPE broccoli_evaluate_batch_fanout_saturated_total counter
broccoli_evaluate_batch_fanout_saturated_total{{problem_type=\"batch\"}} {fanout}
"
                    ))
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/contests"))
            .respond_with(ResponseTemplate::new(201).set_body_json(contest_json(7, Some("icpc"))))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/problems"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(problem_json(11, "batch", "exact", "icpc")),
            )
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/problems/11/test-cases"))
            .respond_with(ResponseTemplate::new(201).set_body_json(test_case_json(21, 11, "1\n")))
            .expect(20)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/contests/7/problems"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "contest_id": 7,
                "problem_id": 11,
                "label": "A",
                "position": 0,
                "problem_title": "burst problem"
            })))
            .expect(1)
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/api/v1/contests/7/problems/11/submissions"))
            .respond_with({
                let submission_ids = Arc::clone(&submission_ids);
                move |_request: &Request| {
                    let id = submission_ids.fetch_add(1, Ordering::SeqCst);
                    ResponseTemplate::new(201).set_body_json(submission_json(
                        id,
                        SubmissionStatus::Pending,
                        None,
                    ))
                }
            })
            .expect(1000)
            .mount(&server)
            .await;

        Mock::given(method("GET"))
            .and(path_regex(r"^/api/v1/submissions/\d+$"))
            .respond_with(|request: &Request| {
                let id = request
                    .url
                    .path()
                    .rsplit('/')
                    .next()
                    .and_then(|value| value.parse::<i32>().ok())
                    .unwrap_or(0);
                ResponseTemplate::new(200).set_body_json(submission_json(
                    id,
                    SubmissionStatus::Judged,
                    None,
                ))
            })
            .expect(1000)
            .mount(&server)
            .await;

        Mock::given(method("DELETE"))
            .and(path("/api/v1/contests/7"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let outcome = Burst {
            server_url: server.uri(),
            metrics_url: Some(format!("{}/metrics", server.uri())),
            creds: AuthCreds::Token("test-token".into()),
            submission_count: 1000,
            concurrency: 128,
            contest_type: Some("icpc".into()),
            problem_type: Some("batch".into()),
            fanout_test_cases_per_problem: 20,
            baseline_plugin_pool_contention_delta: Some(100),
            require_fanout_saturation: true,
            ..Burst::default()
        }
        .run()
        .await
        .expect("burst scenario should run");

        assert!(outcome.passed, "transcript: {:#?}", outcome.transcript);
        assert_eq!(outcome.transcript.summary["terminal"], json!(1000));
        assert!(outcome.transcript.events.iter().any(|event| {
            event.message == "metrics delta captured"
                && event.fields.get("plugin_pool_contention_delta") == Some(&json!(5))
                && event.fields.get("fanout_saturated_delta") == Some(&json!(1))
        }));
        assert!(outcome.transcript.events.iter().any(|event| {
            event
                .message
                .contains("plugin_pool_contention_total delta 5 within cap")
        }));
    }
}
