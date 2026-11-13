//! `perf` subcommand: judging throughput and per-stage latency.
//!
//! Drives one or more servers over HTTP with a closed loop of `--concurrency`
//! clients, each submitting the A+B solution to a scratch problem with
//! `--test-cases` cases and polling its own submission until it is terminal.
//! The load is spread round-robin over every `--server-url`.
//!
//! When `--database-url`, `--redis-url` and `--metrics-url` are given, the run
//! also reports, per submission and per test case:
//! - per-stage wall time from the durable timestamps of the run's own rows
//!   (all of them written by the database or by the servers, never compared
//!   with this client's clock);
//! - Postgres statement counts from `pg_stat_statements` (reset at the start);
//! - Redis command counts from `INFO commandstats` deltas;
//! - Prometheus counter and histogram deltas from every `/metrics` URL.
//!
//! The database and Redis counters are global, so background fibers are
//! included. `--idle-secs` measures them with no load for the same window
//! first, and the report subtracts that idle rate.
//!
//! See `docs/internals/performance.md` for how to run the matrix.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Parser;
use hdrhistogram::Histogram;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::bootstrap::{build_create_contest_request, build_create_problem_request, contest_label};
use crate::client::{AuthCreds, Client};
use crate::dto::{
    AddContestProblemRequest, CreateSubmissionRequest, CreateTestCaseRequest, SubmissionFileDto,
};
use crate::scenarios::SCENARIOS;

#[derive(Parser, Debug, Clone)]
pub struct PerfArgs {
    /// Server base URLs, comma-separated. Submissions are spread round-robin.
    #[arg(long, value_delimiter = ',', required = true)]
    pub server_url: Vec<String>,
    #[arg(long)]
    pub admin_username: String,
    #[arg(long)]
    pub admin_password: String,
    /// Contest type to judge under (e.g. `icpc`, `ioi`).
    #[arg(long, default_value = "icpc")]
    pub contest_type: String,
    /// Problem type; defaults to the first registered one.
    #[arg(long)]
    pub problem_type: Option<String>,
    /// Built-in scenario whose solution is submitted (`ab-cpp-ac`, `ab-py-ac`).
    #[arg(long, default_value = "ab-cpp-ac")]
    pub scenario: String,
    /// Test cases on the scratch problem.
    #[arg(long, default_value_t = 1)]
    pub test_cases: usize,
    /// Measured submissions.
    #[arg(long, default_value_t = 100)]
    pub submissions: usize,
    /// Closed-loop clients (each has one submission in flight at a time).
    #[arg(long, default_value_t = 1)]
    pub concurrency: usize,
    /// Unmeasured submissions judged first, one at a time, to warm the
    /// compile cache, plugin pools and connection pools.
    #[arg(long, default_value_t = 3)]
    pub warmup: usize,
    /// Poll interval for each client's own submission.
    #[arg(long, default_value_t = 50)]
    pub poll_interval_ms: u64,
    /// Give up on a submission after this long.
    #[arg(long, default_value_t = 300)]
    pub per_job_timeout_secs: u64,
    /// Optional Postgres URL for stage timestamps and pg_stat_statements.
    #[arg(long)]
    pub database_url: Option<String>,
    /// Optional Redis URL for INFO commandstats deltas.
    #[arg(long)]
    pub redis_url: Option<String>,
    /// Prometheus endpoints (servers and workers), comma-separated.
    #[arg(long, value_delimiter = ',')]
    pub metrics_url: Vec<String>,
    /// Seconds of no-load counter sampling before the run (0 = skip).
    #[arg(long, default_value_t = 0)]
    pub idle_secs: u64,
    /// GETs of one finished submission after the run, to measure what one
    /// client poll costs so it can be subtracted (0 = skip).
    #[arg(long, default_value_t = 100)]
    pub calibrate_polls: usize,
    /// Number of top statements to print.
    #[arg(long, default_value_t = 15)]
    pub top_statements: usize,
    /// Write the full JSON report here.
    #[arg(long, default_value = "perf.json")]
    pub out: PathBuf,
    /// Keep the scratch contest and problem.
    #[arg(long, default_value_t = false)]
    pub keep_fixtures: bool,
}

pub async fn run(args: PerfArgs) -> u8 {
    match run_inner(args).await {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(e) => {
            eprintln!("perf: error: {e:#}");
            2
        }
    }
}

struct Fixture {
    contest_id: i32,
    problem_id: i32,
}

async fn run_inner(args: PerfArgs) -> anyhow::Result<bool> {
    let scenario = SCENARIOS
        .iter()
        .find(|s| s.id == args.scenario)
        .with_context(|| format!("unknown scenario `{}`", args.scenario))?;
    let creds = AuthCreds::UsernamePassword {
        username: args.admin_username.clone(),
        password: args.admin_password.clone(),
    };
    let mut clients = Vec::new();
    for url in &args.server_url {
        clients.push(
            Client::new(url.clone(), creds.clone())
                .await
                .with_context(|| format!("login to {url}"))?,
        );
    }
    let fixture = setup(&clients[0], &args, scenario).await?;
    let req = CreateSubmissionRequest {
        files: scenario
            .files
            .iter()
            .map(|(f, c)| SubmissionFileDto {
                filename: (*f).to_string(),
                content: (*c).to_string(),
            })
            .collect(),
        language: scenario.language.to_string(),
        contest_type: Some(args.contest_type.clone()),
    };

    for i in 0..args.warmup {
        let client = &clients[i % clients.len()];
        let r = one_submission(client, &fixture, &req, &args).await?;
        if !r.terminal {
            bail!("warmup submission {} did not finish", r.id);
        }
    }

    let db = match &args.database_url {
        Some(url) => Some(
            sqlx::postgres::PgPoolOptions::new()
                .max_connections(2)
                .connect(url)
                .await
                .context("connect database")?,
        ),
        None => None,
    };
    let mut redis = match &args.redis_url {
        Some(url) => Some(
            redis::Client::open(url.as_str())?
                .get_multiplexed_async_connection()
                .await
                .context("connect redis")?,
        ),
        None => None,
    };

    let idle = if args.idle_secs > 0 {
        let before = Counters::sample(db.as_ref(), redis.as_mut(), &args.metrics_url).await?;
        tokio::time::sleep(Duration::from_secs(args.idle_secs)).await;
        let after = Counters::sample(db.as_ref(), redis.as_mut(), &args.metrics_url).await?;
        Some((after.delta(&before), args.idle_secs as f64))
    } else {
        None
    };

    if let Some(db) = &db {
        // Best effort: the extension is optional.
        let _ = sqlx::query("SELECT pg_stat_statements_reset()")
            .execute(db)
            .await;
    }
    let before = Counters::sample(db.as_ref(), redis.as_mut(), &args.metrics_url).await?;

    let started = Instant::now();
    let next = Arc::new(AtomicUsize::new(0));
    let results = Arc::new(Mutex::new(Vec::with_capacity(args.submissions)));
    let fixture = Arc::new(fixture);
    let req = Arc::new(req);
    let mut tasks = Vec::new();
    for _ in 0..args.concurrency.max(1) {
        let (next, results, fixture, req, args) = (
            next.clone(),
            results.clone(),
            fixture.clone(),
            req.clone(),
            args.clone(),
        );
        let clients = clients.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                if i >= args.submissions {
                    return;
                }
                let client = &clients[i % clients.len()];
                match one_submission(client, &fixture, &req, &args).await {
                    Ok(r) => results.lock().await.push(r),
                    Err(e) => tracing::warn!(error = %e, "submission failed"),
                }
            }
        }));
    }
    for t in tasks {
        let _ = t.await;
    }
    let wall = started.elapsed().as_secs_f64();
    let after = Counters::sample(db.as_ref(), redis.as_mut(), &args.metrics_url).await?;
    let delta = after.delta(&before);
    let results = Arc::try_unwrap(results)
        .map(|m| m.into_inner())
        .unwrap_or_default();

    let poll_cost = match results.last() {
        Some(last) if args.calibrate_polls > 0 => {
            let c0 = Counters::sample(db.as_ref(), redis.as_mut(), &args.metrics_url).await?;
            for _ in 0..args.calibrate_polls {
                let _ = clients[0].get_submission(last.id).await;
            }
            let c1 = Counters::sample(db.as_ref(), redis.as_mut(), &args.metrics_url).await?;
            Some(c1.delta(&c0))
        }
        _ => None,
    };

    let report = build_report(
        &args,
        &results,
        wall,
        &delta,
        idle.as_ref(),
        poll_cost.as_ref(),
        db.as_ref(),
    )
    .await?;
    print_report(&report, args.top_statements);
    std::fs::write(&args.out, serde_json::to_vec_pretty(&report)?)
        .with_context(|| format!("write {}", args.out.display()))?;

    if !args.keep_fixtures {
        let _ = clients[0].delete_contest(fixture.contest_id).await;
        let _ = clients[0].delete_problem(fixture.problem_id).await;
    }
    let expected = scenario
        .expected_verdict
        .as_ref()
        .map_or_else(|| "-".to_string(), |v| format!("{v:?}"));
    let ok = results.len() == args.submissions
        && results.iter().all(|r| r.terminal && r.verdict == expected);
    Ok(ok)
}

async fn setup(
    client: &Client,
    args: &PerfArgs,
    scenario: &crate::scenarios::Scenario,
) -> anyhow::Result<Fixture> {
    let registries = client.list_registries().await?;
    let problem_type =
        crate::bootstrap::resolve_problem_type(&registries, args.problem_type.as_deref())?;
    let contest = client
        .create_contest(&build_create_contest_request(&args.contest_type))
        .await?;
    let problem = client
        .create_problem(&build_create_problem_request(
            scenario,
            &args.contest_type,
            &problem_type,
        ))
        .await?;
    let n = args.test_cases.max(1);
    for i in 0..n {
        client
            .create_test_case(
                problem.id,
                &CreateTestCaseRequest {
                    input: scenario.test_input.to_string(),
                    expected_output: scenario.test_expected_output.to_string(),
                    score: (100 / n as i32).max(1),
                    is_sample: false,
                    position: None,
                    description: None,
                    label: Some(format!("perf_{}", i + 1)),
                },
            )
            .await?;
    }
    client
        .add_problem_to_contest(
            contest.id,
            &AddContestProblemRequest {
                problem_id: problem.id,
                label: contest_label(0),
                position: None,
            },
        )
        .await?;
    Ok(Fixture {
        contest_id: contest.id,
        problem_id: problem.id,
    })
}

struct SubResult {
    id: i32,
    terminal: bool,
    status: String,
    verdict: String,
    submit_ms: f64,
    e2e_ms: f64,
    polls: u64,
}

async fn one_submission(
    client: &Client,
    fixture: &Fixture,
    req: &CreateSubmissionRequest,
    args: &PerfArgs,
) -> anyhow::Result<SubResult> {
    let t0 = Instant::now();
    let sub = client
        .create_contest_submission(fixture.contest_id, fixture.problem_id, req)
        .await?;
    let submit_ms = t0.elapsed().as_secs_f64() * 1e3;
    let deadline = t0 + Duration::from_secs(args.per_job_timeout_secs);
    let poll = Duration::from_millis(args.poll_interval_ms.max(1));
    let mut status = sub.status;
    let mut verdict = None;
    let mut polls = 0;
    while !status.is_terminal() && Instant::now() < deadline {
        tokio::time::sleep(poll).await;
        polls += 1;
        if let Ok(s) = client.get_submission(sub.id).await {
            status = s.status;
            verdict = s.result.and_then(|r| r.verdict);
        }
    }
    Ok(SubResult {
        id: sub.id,
        terminal: status.is_terminal(),
        status: format!("{status:?}"),
        verdict: verdict.map_or_else(|| "-".to_string(), |v| format!("{v:?}")),
        submit_ms,
        e2e_ms: t0.elapsed().as_secs_f64() * 1e3,
        polls,
    })
}

// ---------------------------------------------------------------------------
// Counters: pg_stat_statements, Redis commandstats, Prometheus text.
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct Counters {
    /// query text -> (calls, total_exec_ms)
    statements: HashMap<String, (f64, f64)>,
    /// command -> calls
    redis: HashMap<String, f64>,
    /// metric series (name + labels without `le`) -> value, summed over URLs
    prom: BTreeMap<String, f64>,
}

impl Counters {
    async fn sample(
        db: Option<&sqlx::PgPool>,
        redis: Option<&mut redis::aio::MultiplexedConnection>,
        metrics_urls: &[String],
    ) -> anyhow::Result<Self> {
        let mut c = Counters::default();
        if let Some(db) = db {
            let rows: Result<Vec<(String, i64, f64)>, _> = sqlx::query_as(
                "SELECT query, calls, total_exec_time FROM pg_stat_statements \
                 WHERE dbid = (SELECT oid FROM pg_database WHERE datname = current_database())",
            )
            .fetch_all(db)
            .await;
            for (q, calls, ms) in rows.unwrap_or_default() {
                let e = c.statements.entry(q).or_default();
                e.0 += calls as f64;
                e.1 += ms;
            }
        }
        if let Some(conn) = redis {
            let info: String = redis::cmd("INFO")
                .arg("commandstats")
                .query_async(conn)
                .await?;
            c.redis = parse_commandstats(&info);
        }
        let http = reqwest::Client::new();
        for url in metrics_urls {
            let text = http
                .get(url)
                .send()
                .await
                .with_context(|| format!("scrape {url}"))?
                .text()
                .await?;
            for (k, v) in parse_prometheus(&text) {
                *c.prom.entry(k).or_default() += v;
            }
        }
        Ok(c)
    }

    fn delta(&self, before: &Self) -> Self {
        let mut d = Counters::default();
        for (k, (calls, ms)) in &self.statements {
            let (bc, bm) = before.statements.get(k).copied().unwrap_or_default();
            if calls - bc > 0.0 {
                d.statements.insert(k.clone(), (calls - bc, ms - bm));
            }
        }
        for (k, v) in &self.redis {
            let dv = v - before.redis.get(k).copied().unwrap_or_default();
            if dv > 0.0 {
                d.redis.insert(k.clone(), dv);
            }
        }
        for (k, v) in &self.prom {
            let dv = v - before.prom.get(k).copied().unwrap_or_default();
            if dv != 0.0 {
                d.prom.insert(k.clone(), dv);
            }
        }
        d
    }
}

/// `cmdstat_get:calls=12,usec=...` lines -> command -> calls.
pub fn parse_commandstats(info: &str) -> HashMap<String, f64> {
    let mut out = HashMap::new();
    for line in info.lines() {
        let Some(rest) = line.strip_prefix("cmdstat_") else {
            continue;
        };
        let Some((cmd, stats)) = rest.split_once(':') else {
            continue;
        };
        if let Some(calls) = stats
            .split(',')
            .find_map(|kv| kv.strip_prefix("calls="))
            .and_then(|v| v.parse::<f64>().ok())
        {
            out.insert(cmd.to_string(), calls);
        }
    }
    out
}

/// Prometheus text -> `name{labels}` -> value. Keeps `_bucket` series (with
/// `le`) so histogram quantiles can be computed from deltas.
pub fn parse_prometheus(text: &str) -> Vec<(String, f64)> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let (series, value) = l.rsplit_once(' ')?;
            Some((series.to_string(), value.parse::<f64>().ok()?))
        })
        .filter(|(s, _)| s.starts_with("broccoli_") || s.starts_with("http_server"))
        .collect()
}

/// Split `name{a="1",le="0.5"}` into (name, labels-without-le, le).
fn split_series(series: &str) -> (&str, String, Option<String>) {
    let Some((name, rest)) = series.split_once('{') else {
        return (series, String::new(), None);
    };
    let rest = rest.trim_end_matches('}');
    let mut le = None;
    let mut keep = Vec::new();
    for kv in rest.split("\",") {
        let kv = kv.trim_end_matches('"');
        if let Some(v) = kv.strip_prefix("le=\"") {
            le = Some(v.to_string());
        } else if !kv.starts_with("otel_scope") && !kv.starts_with("service_name") && !kv.is_empty()
        {
            keep.push(format!("{kv}\""));
        }
    }
    (name, keep.join(","), le)
}

struct HistDelta {
    count: f64,
    sum: f64,
    buckets: Vec<(f64, f64)>,
}

impl HistDelta {
    fn quantile(&self, q: f64) -> f64 {
        if self.count <= 0.0 {
            return 0.0;
        }
        let target = q * self.count;
        let mut prev_le = 0.0;
        let mut prev_c = 0.0;
        for (le, c) in &self.buckets {
            if *c >= target {
                if le.is_infinite() {
                    return prev_le;
                }
                let frac = if c - prev_c > 0.0 {
                    (target - prev_c) / (c - prev_c)
                } else {
                    1.0
                };
                return prev_le + (le - prev_le) * frac;
            }
            prev_le = *le;
            prev_c = *c;
        }
        prev_le
    }
}

fn histograms(prom: &BTreeMap<String, f64>) -> BTreeMap<String, HistDelta> {
    let mut out: BTreeMap<String, HistDelta> = BTreeMap::new();
    for (series, v) in prom {
        let (name, labels, le) = split_series(series);
        let (base, kind) = if let Some(b) = name.strip_suffix("_bucket") {
            (b, 0)
        } else if let Some(b) = name.strip_suffix("_sum") {
            (b, 1)
        } else if let Some(b) = name.strip_suffix("_count") {
            (b, 2)
        } else {
            continue;
        };
        let key = if labels.is_empty() {
            base.to_string()
        } else {
            format!("{base}{{{labels}}}")
        };
        let h = out.entry(key).or_insert(HistDelta {
            count: 0.0,
            sum: 0.0,
            buckets: Vec::new(),
        });
        match kind {
            0 => {
                let le = le
                    .as_deref()
                    .map(|s| s.parse::<f64>().unwrap_or(f64::INFINITY))
                    .unwrap_or(f64::INFINITY);
                h.buckets.push((le, *v));
            }
            1 => h.sum += v,
            _ => h.count += v,
        }
    }
    for h in out.values_mut() {
        h.buckets.sort_by(|a, b| a.0.total_cmp(&b.0));
    }
    out.retain(|_, h| h.count > 0.0 && !h.buckets.is_empty());
    out
}

// ---------------------------------------------------------------------------
// Report.
// ---------------------------------------------------------------------------

fn pct(h: &Histogram<u64>) -> Value {
    if h.is_empty() {
        return json!(null);
    }
    json!({
        "p50": h.value_at_quantile(0.50) as f64 / 1e3,
        "p95": h.value_at_quantile(0.95) as f64 / 1e3,
        "p99": h.value_at_quantile(0.99) as f64 / 1e3,
        "max": h.max() as f64 / 1e3,
        "mean": h.mean() / 1e3,
    })
}

fn hist_us() -> Histogram<u64> {
    Histogram::new_with_bounds(1, 3_600_000_000, 3).expect("valid bounds")
}

/// Per-stage durations from durable timestamps, in milliseconds. The claim
/// timestamp is the current judgement's `leased_at`.
const STAGE_SQL: &str = r#"
WITH r AS (
  SELECT s.id, s.created_at, j.leased_at, s.judged_at,
         MIN(t.created_at) AS first_tc, MAX(t.created_at) AS last_tc
    FROM submission s
    LEFT JOIN test_case_result t ON t.submission_id = s.id
    LEFT JOIN submission_judgement j ON j.submission_id = s.id AND j.is_current
   WHERE s.id = ANY($1)
   GROUP BY s.id, j.leased_at
)
SELECT
  (EXTRACT(EPOCH FROM (leased_at - created_at)) * 1000)::float8 AS queued,
  (EXTRACT(EPOCH FROM (first_tc - leased_at)) * 1000)::float8   AS to_first_result,
  (EXTRACT(EPOCH FROM (last_tc - first_tc)) * 1000)::float8     AS between_results,
  (EXTRACT(EPOCH FROM (judged_at - last_tc)) * 1000)::float8    AS finalize,
  (EXTRACT(EPOCH FROM (judged_at - created_at)) * 1000)::float8 AS total,
  EXTRACT(EPOCH FROM created_at)::float8                        AS created_epoch,
  EXTRACT(EPOCH FROM judged_at)::float8                         AS judged_epoch
FROM r
"#;

async fn stage_breakdown(db: &sqlx::PgPool, ids: &[i32]) -> anyhow::Result<Value> {
    type Row = (
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
        Option<f64>,
    );
    let rows: Vec<Row> = sqlx::query_as(STAGE_SQL).bind(ids).fetch_all(db).await?;
    let names = [
        "queued",
        "to_first_result",
        "between_results",
        "finalize",
        "total",
    ];
    let mut hs: Vec<Histogram<u64>> = names.iter().map(|_| hist_us()).collect();
    let mut created = Vec::new();
    let mut judged = Vec::new();
    for r in &rows {
        let vals = [r.0, r.1, r.2, r.3, r.4];
        for (h, v) in hs.iter_mut().zip(vals) {
            if let Some(v) = v {
                let _ = h.record((v.max(0.0) * 1e3) as u64 + 1);
            }
        }
        if let (Some(c), Some(j)) = (r.5, r.6) {
            created.push(c);
            judged.push(j);
        }
    }
    let mut out = serde_json::Map::new();
    for (n, h) in names.iter().zip(&hs) {
        out.insert((*n).to_string(), pct(h));
    }
    // Server-side throughput: judged rows over the DB-clock span between the
    // first accepted and last judged submission, plus the steady-state rate
    // between the 10th and 90th percentile completions.
    if !judged.is_empty() {
        judged.sort_by(f64::total_cmp);
        let first = created.iter().copied().fold(f64::INFINITY, f64::min);
        let last = *judged.last().unwrap();
        let span = (last - first).max(1e-9);
        let n = judged.len();
        let (a, b) = (n / 10, (n * 9 / 10).saturating_sub(1).max(n / 10));
        let steady = if b > a {
            Some((b - a) as f64 / (judged[b] - judged[a]).max(1e-9))
        } else {
            None
        };
        out.insert(
            "db_throughput".into(),
            json!({ "span_s": span, "subs_per_s": n as f64 / span, "steady_subs_per_s": steady }),
        );
    }
    Ok(Value::Object(out))
}

async fn build_report(
    args: &PerfArgs,
    results: &[SubResult],
    wall: f64,
    delta: &Counters,
    idle: Option<&(Counters, f64)>,
    poll_cost: Option<&Counters>,
    db: Option<&sqlx::PgPool>,
) -> anyhow::Result<Value> {
    let n = results.len().max(1) as f64;
    let tcs = n * args.test_cases.max(1) as f64;
    let mut submit = hist_us();
    let mut e2e = hist_us();
    let mut statuses: BTreeMap<String, u64> = BTreeMap::new();
    for r in results {
        let _ = submit.record((r.submit_ms * 1e3) as u64 + 1);
        let _ = e2e.record((r.e2e_ms * 1e3) as u64 + 1);
        *statuses
            .entry(format!("{}/{}", r.status, r.verdict))
            .or_default() += 1;
    }
    let idle_scale = |idle_counters: &HashMap<String, f64>, k: &str| -> f64 {
        idle.map(|(_, secs)| idle_counters.get(k).copied().unwrap_or(0.0) * wall / secs)
            .unwrap_or(0.0)
    };

    // Statements.
    let idle_stmt: HashMap<String, f64> = idle
        .map(|(c, _)| c.statements.iter().map(|(k, v)| (k.clone(), v.0)).collect())
        .unwrap_or_default();
    let mut stmts: Vec<(&String, f64, f64)> = delta
        .statements
        .iter()
        .map(|(q, (c, ms))| (q, (*c - idle_scale(&idle_stmt, q)).max(0.0), *ms))
        .collect();
    stmts.sort_by(|a, b| b.1.total_cmp(&a.1));
    let total_calls: f64 = stmts.iter().map(|s| s.1).sum();
    let total_ms: f64 = stmts.iter().map(|s| s.2).sum();
    let is_txn = |q: &str| {
        let u = q.trim_start().to_ascii_uppercase();
        u.starts_with("BEGIN") || u.starts_with("COMMIT") || u.starts_with("ROLLBACK")
    };
    let txn_calls: f64 = stmts.iter().filter(|s| is_txn(s.0)).map(|s| s.1).sum();

    // Redis.
    let mut redis: Vec<(&String, f64)> = delta
        .redis
        .iter()
        .map(|(k, v)| {
            (
                k,
                (*v - idle_scale(idle.map(|i| &i.0.redis).unwrap_or(&HashMap::new()), k)).max(0.0),
            )
        })
        .collect();
    redis.sort_by(|a, b| b.1.total_cmp(&a.1));
    let redis_total: f64 = redis.iter().map(|r| r.1).sum();

    // Prometheus histograms.
    let hists = histograms(&delta.prom);
    let hist_json: BTreeMap<String, Value> = hists
        .iter()
        .map(|(k, h)| {
            (
                k.clone(),
                json!({
                    "count": h.count,
                    "per_submission": h.count / n,
                    "mean_ms": h.sum / h.count * 1e3,
                    "p50_ms": h.quantile(0.5) * 1e3,
                    "p95_ms": h.quantile(0.95) * 1e3,
                    "p99_ms": h.quantile(0.99) * 1e3,
                }),
            )
        })
        .collect();
    let counters: BTreeMap<String, f64> = delta
        .prom
        .iter()
        .filter(|(k, _)| k.split('{').next().is_some_and(|n| n.ends_with("_total")))
        .map(|(k, v)| (k.clone(), *v))
        .collect();

    // What one client GET costs, and the run's totals net of client polling.
    let polls: f64 = results.iter().map(|r| r.polls as f64).sum();
    let polling = poll_cost.map(|c| {
        let k = args.calibrate_polls.max(1) as f64;
        let sql: f64 = c.statements.values().map(|v| v.0).sum::<f64>() / k;
        let redis_per: f64 = c.redis.values().sum::<f64>() / k;
        let plugin_calls: f64 = histograms(&c.prom)
            .iter()
            .filter(|(name, _)| name.starts_with("broccoli_plugin_call_duration"))
            .map(|(_, h)| h.count)
            .sum::<f64>()
            / k;
        json!({
            "polls_per_submission": polls / n,
            "per_poll": { "statements": sql, "redis": redis_per, "plugin_calls": plugin_calls },
            "net_statements_per_submission": (total_calls - sql * polls).max(0.0) / n,
            "net_statements_per_test_case": (total_calls - sql * polls).max(0.0) / tcs,
            "net_redis_per_submission": (redis_total - redis_per * polls).max(0.0) / n,
            "net_redis_per_test_case": (redis_total - redis_per * polls).max(0.0) / tcs,
        })
    });
    let plugin_calls_total: f64 = hists
        .iter()
        .filter(|(name, _)| name.starts_with("broccoli_plugin_call_duration"))
        .map(|(_, h)| h.count)
        .sum();

    let stages = match db {
        Some(db) => {
            let ids: Vec<i32> = results.iter().map(|r| r.id).collect();
            stage_breakdown(db, &ids)
                .await
                .unwrap_or_else(|e| json!({"error": e.to_string()}))
        }
        None => json!(null),
    };

    Ok(json!({
        "params": {
            "servers": args.server_url,
            "contest_type": args.contest_type,
            "scenario": args.scenario,
            "test_cases": args.test_cases,
            "submissions": args.submissions,
            "concurrency": args.concurrency,
            "poll_interval_ms": args.poll_interval_ms,
            "idle_secs": args.idle_secs,
        },
        "completed": results.iter().filter(|r| r.terminal).count(),
        "statuses": statuses,
        "wall_s": wall,
        // A closed loop cannot complete faster than one submission per client
        // per poll interval; near this cap, read `stages_ms.db_throughput`.
        "client_throughput": {
            "subs_per_s": n / wall,
            "test_cases_per_s": tcs / wall,
            "poll_cap_subs_per_s": args.concurrency as f64 * 1000.0 / args.poll_interval_ms.max(1) as f64,
        },
        "submit_latency_ms": pct(&submit),
        "e2e_latency_ms": pct(&e2e),
        "stages_ms": stages,
        "db": {
            "statements_per_submission": total_calls / n,
            "statements_per_test_case": total_calls / tcs,
            "txn_control_per_submission": txn_calls / n,
            "exec_ms_per_submission": total_ms / n,
            "top": stmts.iter().take(40).map(|(q, c, ms)| json!({
                "per_submission": c / n,
                "per_test_case": c / tcs,
                "mean_ms": if *c > 0.0 { ms / c } else { 0.0 },
                "query": q.chars().take(240).collect::<String>(),
            })).collect::<Vec<_>>(),
        },
        "redis": {
            "commands_per_submission": redis_total / n,
            "commands_per_test_case": redis_total / tcs,
            "by_command_per_submission": redis.iter().map(|(k, v)| ((*k).clone(), json!(v / n))).collect::<serde_json::Map<_, _>>(),
        },
        "client_polling": polling,
        "plugin_calls_per_submission": plugin_calls_total / n,
        "metrics": {
            "histograms": hist_json,
            "counters": counters,
        },
    }))
}

fn print_report(r: &Value, top: usize) {
    let f = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
    let p = &r["params"];
    println!(
        "== perf: {} x {} test case(s), concurrency {}, servers {}",
        p["submissions"],
        p["test_cases"],
        p["concurrency"],
        p["servers"].as_array().map_or(0, Vec::len)
    );
    println!(
        "completed {} in {:.2}s  ->  {:.2} subs/s, {:.2} test cases/s   statuses {}",
        r["completed"],
        f(r, "wall_s"),
        f(&r["client_throughput"], "subs_per_s"),
        f(&r["client_throughput"], "test_cases_per_s"),
        r["statuses"]
    );
    let cap = f(&r["client_throughput"], "poll_cap_subs_per_s");
    if f(&r["client_throughput"], "subs_per_s") > 0.8 * cap {
        println!(
            "warning: within 20% of the polling cap ({cap:.1} subs/s = concurrency / poll \
             interval); use stages_ms.db_throughput or raise --concurrency"
        );
    }
    for k in ["submit_latency_ms", "e2e_latency_ms"] {
        let v = &r[k];
        println!(
            "{k:<22} p50 {:>9.1}  p95 {:>9.1}  p99 {:>9.1}  max {:>9.1}",
            f(v, "p50"),
            f(v, "p95"),
            f(v, "p99"),
            f(v, "max")
        );
    }
    if let Some(stages) = r["stages_ms"].as_object() {
        println!("-- stages (DB timestamps, ms)");
        for (k, v) in stages {
            if k == "db_throughput" {
                println!("{k:<22} {v}");
            } else {
                println!(
                    "{k:<22} p50 {:>9.1}  p95 {:>9.1}  p99 {:>9.1}  max {:>9.1}",
                    f(v, "p50"),
                    f(v, "p95"),
                    f(v, "p99"),
                    f(v, "max")
                );
            }
        }
    }
    let db = &r["db"];
    println!(
        "-- postgres: {:.1} statements/sub ({:.2}/test case), {:.1} BEGIN/COMMIT/sub, {:.2} ms exec/sub",
        f(db, "statements_per_submission"),
        f(db, "statements_per_test_case"),
        f(db, "txn_control_per_submission"),
        f(db, "exec_ms_per_submission")
    );
    if let Some(rows) = db["top"].as_array() {
        for s in rows.iter().take(top) {
            println!(
                "  {:>7.2}/sub {:>6.2}/tc {:>7.3}ms  {}",
                f(s, "per_submission"),
                f(s, "per_test_case"),
                f(s, "mean_ms"),
                s["query"]
                    .as_str()
                    .unwrap_or("")
                    .replace('\n', " ")
                    .chars()
                    .take(110)
                    .collect::<String>()
            );
        }
    }
    let rd = &r["redis"];
    println!(
        "-- redis: {:.1} commands/sub ({:.2}/test case)  {}",
        f(rd, "commands_per_submission"),
        f(rd, "commands_per_test_case"),
        rd["by_command_per_submission"]
    );
    if !r["client_polling"].is_null() {
        println!("-- net of client polling: {}", r["client_polling"]);
    }
    if let Some(h) = r["metrics"]["histograms"].as_object() {
        println!("-- metric histograms (delta over the run)");
        for (k, v) in h {
            println!(
                "  {:>8.2}/sub mean {:>8.2}ms p50 {:>8.2} p95 {:>8.2}  {}",
                f(v, "per_submission"),
                f(v, "mean_ms"),
                f(v, "p50_ms"),
                f(v, "p95_ms"),
                k
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commandstats_parses_calls() {
        let info = "# Commandstats\r\ncmdstat_get:calls=12,usec=30,usec_per_call=2.5\r\ncmdstat_lpush:calls=3,usec=1\r\n";
        let m = parse_commandstats(info);
        assert_eq!(m["get"], 12.0);
        assert_eq!(m["lpush"], 3.0);
    }

    #[test]
    fn histogram_quantile_interpolates_bucket_deltas() {
        let mut prom = BTreeMap::new();
        prom.insert(
            "broccoli_x_seconds_bucket{f=\"a\",le=\"0.1\"}".to_string(),
            50.0,
        );
        prom.insert(
            "broccoli_x_seconds_bucket{f=\"a\",le=\"1\"}".to_string(),
            100.0,
        );
        prom.insert(
            "broccoli_x_seconds_bucket{f=\"a\",le=\"+Inf\"}".to_string(),
            100.0,
        );
        prom.insert("broccoli_x_seconds_sum{f=\"a\"}".to_string(), 20.0);
        prom.insert("broccoli_x_seconds_count{f=\"a\"}".to_string(), 100.0);
        let h = histograms(&prom);
        let x = &h["broccoli_x_seconds{f=\"a\"}"];
        assert_eq!(x.count, 100.0);
        assert!((x.quantile(0.5) - 0.1).abs() < 1e-9);
        assert!((x.quantile(0.75) - 0.55).abs() < 1e-9);
    }
}
