use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::client::AuthCreds;

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum LoadProfile {
    /// Existing judge-throughput profile: submit official solutions and poll them.
    Judge,
    /// Mixed contest-traffic profile: page reads, scoreboard polling, code-runs, and submissions.
    Mixed,
}

/// Admin credentials: a bearer token, or a username and password to log in
/// with (and log in again with when the token expires).
#[derive(Args, Debug, Clone)]
pub struct AdminCreds {
    /// Admin bearer token. Provide this OR --admin-username + --admin-password.
    #[arg(long)]
    pub admin_token: Option<String>,
    /// Admin username (used with --admin-password to log in).
    #[arg(long)]
    pub admin_username: Option<String>,
    /// Admin password (used with --admin-username to log in).
    #[arg(long)]
    pub admin_password: Option<String>,
}

impl AdminCreds {
    pub fn auth(&self) -> Result<AuthCreds, String> {
        match (
            &self.admin_token,
            &self.admin_username,
            &self.admin_password,
        ) {
            (Some(token), _, _) => Ok(AuthCreds::Token(token.clone())),
            (None, Some(username), Some(password)) => Ok(AuthCreds::UsernamePassword {
                username: username.clone(),
                password: password.clone(),
            }),
            _ => Err(
                "must provide --admin-token, or both --admin-username and --admin-password"
                    .to_string(),
            ),
        }
    }
}

/// Top-level subcommands. Stress mode (the default) takes no subcommand; the
/// `fault` subcommand opts in to the fault-injection scenarios.
#[derive(Subcommand, Debug, Clone)]
pub enum Command {
    /// Fault-injection scenarios (cancel-storm, kill-server-recovery, ...).
    Fault(FaultArgs),
    /// Judging throughput and per-stage latency (see website/docs/internals/performance.md).
    Perf(crate::perf::PerfArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct FaultArgs {
    #[command(subcommand)]
    pub scenario: FaultScenario,
}

#[derive(Subcommand, Debug, Clone)]
pub enum FaultScenario {
    /// Hammer Redis with cancel-batch probes; assert hit/miss/DEL toggle behaviour.
    CancelStorm(CancelStormArgs),
    /// Kill a server replica mid-judgement; assert lease/steal recovery.
    KillServerRecovery(KillServerRecoveryArgs),
    /// Simulate a worker restart while a compile cache leader is active; assert
    /// followers poll first, then take over after the abandoned lease expires.
    RollingWorkerRestart(RollingWorkerRestartArgs),
    /// Post a burst of N submissions across a configurable contest-type mix and
    /// assert that >=95% reach a terminal verdict within the deadline.
    Burst(BurstArgs),
}

#[derive(Parser, Debug, Clone)]
pub struct CancelStormArgs {
    /// Number of distinct batch ids to prime / probe. Half are primed.
    #[arg(long, default_value_t = 1000)]
    pub batch_count: usize,
    /// Number of concurrent reader tasks probing the keys.
    #[arg(long, default_value_t = 64)]
    pub readers: usize,
    /// TTL for the primed cancel keys, in seconds.
    #[arg(long, default_value_t = 21600)]
    pub key_ttl_secs: u64,
    /// Redis URL. If omitted, an ephemeral testcontainer is started.
    #[arg(long)]
    pub redis_url: Option<String>,
    /// Path to write the transcript JSON.
    #[arg(long, default_value = "transcript.json")]
    pub out: PathBuf,
}

#[derive(Parser, Debug, Clone)]
pub struct BurstArgs {
    /// Base URL of the server (e.g. `http://localhost:3000`).
    #[arg(long)]
    pub server_url: String,
    #[command(flatten)]
    pub admin: AdminCreds,
    /// Total number of submissions to post.
    #[arg(long, default_value_t = 1000)]
    pub submission_count: usize,
    /// In-flight submission cap during the inject phase.
    #[arg(long, default_value_t = 64)]
    pub concurrency: usize,
    /// Comma-separated contest-type weights, e.g. `icpc:70,ioi:30`. Empty =
    /// auto-pick all registered contest types with equal weight.
    #[arg(long, default_value = "")]
    pub type_weights: String,
    /// If set, hard-fail when any requested type is missing from the registry.
    /// Otherwise missing types are dropped with a warning.
    #[arg(long, default_value_t = false)]
    pub strict: bool,
    /// Maximum seconds to wait for all submissions to reach a terminal state.
    #[arg(long, default_value_t = 300)]
    pub terminal_deadline_secs: u64,
    /// Optional Prometheus metrics URL to snapshot before/after the burst.
    #[arg(long)]
    pub metrics_url: Option<String>,
    /// Number of testcases to create per scratch problem. Values >1 exercise
    /// evaluator fanout pressure; use 20 for Signpost-class ICPC pressure.
    #[arg(long, default_value_t = 1)]
    pub fanout_test_cases_per_problem: usize,
    /// Optional no-semaphore or unbounded-fanout contention delta to compare
    /// against when --metrics-url is provided.
    #[arg(long)]
    pub baseline_plugin_admission_queued_delta: Option<u64>,
    /// Maximum allowed fraction of the baseline contention delta.
    #[arg(long, default_value_t = 0.2)]
    pub max_plugin_admission_queued_baseline_ratio: f64,
    /// Absolute plugin-admission-queued delta cap when --metrics-url is provided.
    #[arg(long, default_value_t = 10)]
    pub max_plugin_admission_queued_delta: u64,
    /// Require fanout saturation to be observed, proving the semaphore engaged.
    #[arg(long, default_value_t = false)]
    pub require_fanout_saturation: bool,
    /// If set, keep the scratch contests after the run for inspection.
    #[arg(long, default_value_t = false)]
    pub keep_fixtures: bool,
    /// Override the contest_type used for any "auto" picks. Ignored when
    /// --type-weights provides explicit types.
    #[arg(long)]
    pub contest_type: Option<String>,
    /// Override the problem_type for created problems. Defaults to the first
    /// registered problem_type.
    #[arg(long)]
    pub problem_type: Option<String>,
    /// Path to write the transcript JSON.
    #[arg(long, default_value = "transcript.json")]
    pub out: PathBuf,
}

#[derive(Parser, Debug, Clone)]
pub struct KillServerRecoveryArgs {
    /// Postgres connection URL (e.g. `postgres://postgres:password@localhost:5432/broccoli`).
    #[arg(long)]
    pub db_url: String,
    /// Base URL of the server replica fleet (e.g. `http://localhost:3000`).
    #[arg(long)]
    pub server_url: String,
    /// Credentials the submissions are posted with.
    #[command(flatten)]
    pub admin: AdminCreds,
    /// Shell command executed under `sh -c` to kill the target server replica
    /// (e.g. `docker compose kill server-1`).
    #[arg(long)]
    pub kill_command: String,
    /// Redis URL. Required; the scenario presumes operator-managed infra.
    #[arg(long)]
    pub redis_url: String,
    /// Number of submissions to POST before killing the server.
    #[arg(long, default_value_t = 10)]
    pub submission_count: usize,
    /// Time (seconds) to observe for recovery after the kill.
    #[arg(long, default_value_t = 75)]
    pub observe_timeout_secs: u64,
    /// Problem id to submit against. Must be pre-seeded.
    #[arg(long, default_value_t = 1)]
    pub problem_id: i32,
    /// Path to write the transcript JSON.
    #[arg(long, default_value = "transcript.json")]
    pub out: PathBuf,
}

#[derive(Parser, Debug, Clone)]
pub struct RollingWorkerRestartArgs {
    /// Redis URL. If omitted, an ephemeral testcontainer is started.
    #[arg(long)]
    pub redis_url: Option<String>,
    /// Optional shell command executed under `sh -c` while the leader lease is
    /// held, e.g. `docker compose restart worker-1`.
    #[arg(long)]
    pub restart_command: Option<String>,
    /// Leader lease TTL in seconds. Keep this short for harness runs.
    #[arg(long, default_value_t = 3)]
    pub leader_ttl_secs: u64,
    /// How long the leader holds the lease before the restart/simulated death.
    #[arg(long, default_value_t = 500)]
    pub leader_hold_ms: u64,
    /// Follower poll interval while waiting for the abandoned lease to expire.
    #[arg(long, default_value_t = 100)]
    pub follower_poll_interval_ms: u64,
    /// Path to write the transcript JSON.
    #[arg(long, default_value = "transcript.json")]
    pub out: PathBuf,
}

#[derive(Parser, Debug)]
#[command(
    name = "broccoli-stress-test",
    version,
    about = "Broccoli platform stress test",
    long_about = None,
    after_help = "First time? Get the matching binary at <your-server>/downloads.",
)]
pub struct Cli {
    /// Optional subcommand. When omitted, stress mode runs (the historical
    /// behaviour). Currently the only subcommand is `fault` for the
    /// fault-injection scenarios.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Target base URL. Required for stress mode; ignored by the `fault`
    /// subcommand (its scenarios take their own `--server-url` etc.).
    #[arg(long, default_value = "")]
    pub url: String,

    #[command(flatten)]
    pub admin: AdminCreds,

    #[arg(long, default_value_t = 200)]
    pub total: u64,

    /// Run duration in seconds. For mixed profile this overrides --total.
    #[arg(long)]
    pub duration: Option<u64>,

    #[arg(long, default_value_t = 20)]
    pub rate: u32,

    #[arg(long, default_value_t = 50)]
    pub concurrency: u32,

    #[arg(long, value_enum, default_value_t = LoadProfile::Judge)]
    pub profile: LoadProfile,

    /// Number of contestant accounts to create/enroll for mixed profile traffic.
    #[arg(long, default_value_t = 0)]
    pub contestants: u32,

    #[arg(long, default_value_t = 60)]
    pub per_job_timeout: u64,

    #[arg(long, default_value_t = 15000)]
    pub p95_budget_ms: u64,

    #[arg(long)]
    pub contest_type: Option<String>,

    #[arg(long)]
    pub problem_type: Option<String>,

    #[arg(long)]
    pub contest_id: Option<i32>,

    #[arg(long)]
    pub problem_id: Option<i32>,

    #[arg(long, default_value_t = 20)]
    pub contest_concurrency: u32,

    /// Skip the load phase. A run that bootstraps its own fixtures still
    /// judges every scenario once in the correctness phase.
    #[arg(long, default_value_t = false)]
    pub skip_load: bool,

    #[arg(long, default_value_t = false)]
    pub keep_fixtures: bool,

    #[arg(long, default_value_t = 0)]
    pub seed: u64,

    #[arg(long, default_value_t = false)]
    pub json: bool,

    /// Skip the startup version handshake against the server.
    #[arg(long, default_value_t = false)]
    pub no_version_check: bool,

    /// Stable identifier attached to every request for log/trace correlation.
    #[arg(long)]
    pub run_id: Option<String>,

    /// Duration, in seconds, for the final burst window in the mixed profile.
    #[arg(long, default_value_t = 0)]
    pub final_burst_duration: u64,

    /// Rate multiplier used during the final burst window in the mixed profile.
    #[arg(long, default_value_t = 3)]
    pub final_burst_multiplier: u32,
}

impl Cli {
    pub fn validate(&self) -> Result<(), String> {
        // Fault subcommand carries its own self-contained args; the top-level
        // stress flags do not apply. Clap enforces the per-scenario required
        // fields; nothing for us to validate here.
        if self.command.is_some() {
            return Ok(());
        }

        if self.url.is_empty() {
            return Err("--url is required in stress mode".to_string());
        }

        self.admin.auth()?;

        if self.total == 0 {
            return Err("--total must be greater than zero".to_string());
        }
        if let Some(duration) = self.duration
            && duration == 0
        {
            return Err("--duration must be greater than zero".to_string());
        }
        if self.rate == 0 {
            return Err("--rate must be greater than zero".to_string());
        }
        if self.concurrency == 0 {
            return Err("--concurrency must be greater than zero".to_string());
        }
        if let Some(run_id) = &self.run_id
            && (run_id.is_empty()
                || !run_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'))
        {
            return Err(
                "--run-id must contain only ASCII alphanumeric characters, '.', '-', or '_'"
                    .to_string(),
            );
        }
        if self.final_burst_duration > 0 && self.final_burst_multiplier == 0 {
            return Err(
                "--final-burst-multiplier must be greater than zero when burst is enabled"
                    .to_string(),
            );
        }

        Ok(())
    }
}
