use std::path::PathBuf;

use config::{Config, ConfigError, Environment, File};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, SeqAccess, Visitor},
};
use tracing::{info, warn};

pub use common::config::MqAppConfig;
pub use common::storage::config::BlobStoreConfig;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DatabaseConfig {
    #[serde(default = "default_database_url")]
    pub url: String,
    #[serde(default = "default_database_max_connections")]
    pub max_connections: u32,
    /// Size of the RESTRICTED plugin pool (raw `host.db.*` SQL), isolated from the
    /// main pool to avoid cross-operation connection-protocol desync (the spurious
    /// 0x00 bug). There is a SECOND, privileged plugin pool sized by
    /// `plugin_privileged_max_connections`, so total backend connections are
    /// `max_connections + plugin_max_connections + plugin_privileged_max_connections`
    /// - Postgres `max_connections` must accommodate all three.
    #[serde(default = "default_plugin_max_connections")]
    pub plugin_max_connections: u32,
    /// Size of the PRIVILEGED plugin pool that backs the structured, server-owned
    /// host fns (`host.submission.*`, `host.storage.*`, `config:write`). These are
    /// short finalize-time writes, far lower-concurrency than raw plugin SQL, so it
    /// defaults smaller than `plugin_max_connections` to keep the total connection
    /// footprint down. Raise it for write-heavy plugins or high evaluator
    /// parallelism (its own acquire waits would otherwise slow finalize).
    #[serde(default = "default_plugin_privileged_max_connections")]
    pub plugin_privileged_max_connections: u32,
    /// Optional dedicated connection string for the RESTRICTED plugin SQL pool,
    /// authenticating as a pre-provisioned least-privilege LOGIN role (e.g.
    /// `broccoli_plugin_login`, a NOINHERIT member of `broccoli_plugin`). Set
    /// this when a DBA manages that role out of band and the app role must NOT
    /// hold CREATEROLE. When unset, the server auto-provisions the role using the
    /// app role's privileges, with a stable per-database password (so every
    /// replica converges on the same value); if that also fails it falls back to
    /// app-role auth, still guarded by the SQL text guard. Either
    /// way the pool ends up authenticating as a role that cannot `RESET ROLE`
    /// its way back to the privileged app role.
    #[serde(default)]
    pub plugin_url: Option<String>,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_database_url(),
            max_connections: default_database_max_connections(),
            plugin_max_connections: default_plugin_max_connections(),
            plugin_privileged_max_connections: default_plugin_privileged_max_connections(),
            plugin_url: None,
        }
    }
}

fn default_database_url() -> String {
    "postgres://postgres:password@localhost:5432/broccoli".into()
}

fn default_database_max_connections() -> u32 {
    20
}

fn default_plugin_max_connections() -> u32 {
    20
}

fn default_plugin_privileged_max_connections() -> u32 {
    10
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CorsConfig {
    #[serde(default)]
    pub allow_origins: Vec<String>,
    #[serde(default = "default_cors_max_age")]
    pub max_age: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allow_origins: Vec::new(),
            max_age: default_cors_max_age(),
        }
    }
}

fn default_cors_max_age() -> u64 {
    3600
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub cors: CorsConfig,
    /// Externally reachable base URL of this server (scheme + host + optional
    /// port, no trailing slash), used to build links the server hands back to
    /// clients such as the device-flow `verification_url`. The bind address is
    /// not a public origin: under the shipped Docker default
    /// `server.host = 0.0.0.0` it would otherwise yield an unreachable
    /// `http://0.0.0.0:3000`. Set this when the browser-facing origin differs
    /// from what the request reports, for example behind a proxy that does not
    /// forward `X-Forwarded-Host`. When unset the server derives the origin
    /// from the request, so most single-origin deployments need not set it.
    #[serde(default)]
    pub public_base_url: Option<String>,
    /// Directory containing the baked frontend `dist/` output served by the
    /// server in production.
    #[serde(default = "default_frontend_dist")]
    pub frontend_dist: PathBuf,
    /// CIDR ranges for trusted L7 proxies. Empty means no proxy headers are
    /// trusted and client IP extraction falls back to the socket address.
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub trusted_proxies: Vec<String>,
    /// Enables IP-based throttling on `/api/v1/auth/login`.
    #[serde(default)]
    pub rate_limit_auth: bool,
    /// Logical identity of this replica. Used to derive the per-replica
    /// operation-result queue name so multiple servers behind a load balancer
    /// each receive their own plugin-dispatch results. Empty (the default)
    /// resolves to the OS hostname; an unusable hostname falls back to a
    /// random short ID. See [`resolve_server_id`].
    #[serde(default)]
    pub id: String,
    /// Multi-replica deployments require a stable explicit server ID; when
    /// true, hostname/random fallbacks are rejected.
    #[serde(default)]
    pub expects_multi_replica: bool,
    /// Master switch for lease refresh, steal scanning, and reply-queue
    /// sweeping. Defaults off for opt-in rollout.
    #[serde(default)]
    pub dispatcher_lease_steal_enabled: bool,
    /// Admission-control switch for plugin-dispatch background tasks.
    #[serde(default = "default_dispatcher_semaphore_enabled")]
    pub dispatcher_semaphore_enabled: bool,
    #[serde(default = "default_dispatcher_concurrency")]
    pub dispatcher_concurrency: u32,
    /// Per-server cap on in-process dispatch tasks waiting on the
    /// dispatcher semaphore. Post-UP#37 the durable accept path
    /// (POST -> INSERT `Queued` -> claim fiber -> dispatch) no longer
    /// passes through the dispatcher semaphore at ingress - only the
    /// steal-scanner (`dispatcher/steal.rs`) still calls
    /// `dispatcher_permits.reserve()` before spawning a re-dispatch
    /// task. Tune this to bound spawn bursts on a single server when
    /// the steal scanner picks up many orphaned rows at once.
    ///
    /// Historically named `max_queued_submissions`; renamed in UP#39
    /// so that name could be repurposed as the **DB-row backpressure
    /// cap** at POST ingress, matching its plain-English meaning.
    #[serde(default = "default_dispatcher_admission_queue_max")]
    pub dispatcher_admission_queue_max: u32,
    /// Per-server cap on the **durable `Queued` queue depth** observed
    /// in the `submission`, `code_run`, and `submission_judgement`
    /// tables combined. When the live count is at or above this value,
    /// POST endpoints that would insert a new `Queued` row reject with
    /// `503 Service Unavailable` + `Retry-After` rather than enqueueing
    /// more work (UP#39 backpressure-on-post).
    ///
    /// Multi-replica deployments share the DB, so this cap is observed
    /// fleet-wide via `SELECT COUNT(*) WHERE status='Queued'` - not
    /// per-replica. Each replica makes the same decision independently
    /// without coordination.
    ///
    /// Default 5000. `0` disables the cap entirely (used by integration
    /// tests that do not exercise backpressure). Tune by observing the
    /// `Queued` p95 depth during a load test - when claim fibers can
    /// drain `Queued` rows at a steady rate `R` rows/sec, a cap of
    /// `R * acceptable_latency_seconds` keeps p95 POST->dispatch latency
    /// bounded.
    #[serde(default = "default_max_queued_submissions")]
    pub max_queued_submissions: u32,
    #[serde(default = "default_lease_ttl_secs")]
    pub lease_ttl_secs: u64,
    #[serde(default = "default_lease_refresh_interval_secs")]
    pub lease_refresh_interval_secs: u64,
    #[serde(default = "default_steal_scan_interval_secs")]
    pub steal_scan_interval_secs: u64,
    #[serde(default = "default_steal_batch_size")]
    pub steal_batch_size: u32,
    #[serde(default = "default_sweep_interval_secs")]
    pub sweep_interval_secs: u64,
    /// Maximum dispatch ATTEMPTS (first dispatch included) the claim and
    /// lease-steal paths make before finalizing a row as SystemError; compared
    /// against the row's `retry_count`, which counts attempts.
    #[serde(default = "default_max_dispatch_retries")]
    pub max_dispatch_retries: u32,
    /// Retry budget for the global stuck detector. Rows with
    /// `retry_count <= max_stuck_retries` (`retry_count` counts dispatch
    /// attempts) are left for claim/lease-steal recovery; rows above the
    /// budget are finalized as SystemError.
    #[serde(default = "default_max_stuck_retries")]
    pub max_stuck_retries: u32,
    /// Retry budget for the SystemError-retry reaper. A `SystemError` verdict is
    /// a SYSTEM condition (contention, lost result, wedge), never the
    /// contestant's code, so it must not stand as the verdict. This budget is
    /// deliberately much larger than `max_dispatch_retries`/`max_stuck_retries`:
    /// the stuck-handler and dispatch-exhaustion paths SPEND those budgets before
    /// terminalizing to `status == SystemError`, so the reaper needs headroom to
    /// keep re-judging ("slow is fine; never abandon a system fault"). It stays
    /// finite only as a runaway backstop and is rarely approached once the
    /// underlying contention clears. Admission control bounds the blast radius.
    #[serde(default = "default_max_system_error_retries")]
    pub max_system_error_retries: u32,
    /// When true, log ghost reply queues and debounce them without deleting
    /// the Redis keys. Defaults to false now that the sweeper has soaked in
    /// dry-run through earlier UP rollouts; flip back to true if you need to
    /// audit what would be deleted before letting the sweeper act.
    #[serde(default = "default_sweeper_dry_run")]
    pub sweeper_dry_run: bool,
    /// Master switch for the dead-worker operation reaper. When a worker crashes,
    /// the broker (no visibility timeout) strands its in-flight operation in
    /// `<queue>_processing` and any queued work in its private ZSET; the reaper
    /// requeues both onto the shared queue once the owner's heartbeat lapses,
    /// closing the up-to-30-minute waiter-timeout recovery gap. Active by default.
    #[serde(default = "default_operation_reaper_enabled")]
    pub operation_reaper_enabled: bool,
    /// Tick period for the operation reaper.
    #[serde(default = "default_operation_reaper_interval_secs")]
    pub operation_reaper_interval_secs: u64,
    /// Debounce a dead worker for this long (on top of the 15s heartbeat TTL)
    /// before requeuing its strands, so a worker briefly between heartbeats is
    /// not reaped out from under itself.
    #[serde(default = "default_operation_reaper_grace_secs")]
    pub operation_reaper_grace_secs: i64,
    /// Per-tick requeue cap, bounding the reaper's blast radius. Any remainder is
    /// logged and picked up on the next tick.
    #[serde(default = "default_operation_reaper_max_requeues_per_tick")]
    pub operation_reaper_max_requeues_per_tick: usize,
    /// When true, the operation reaper logs the strands it WOULD requeue without
    /// mutating Redis. Defaults false (the reaper is a recovery, not a delete).
    #[serde(default = "default_operation_reaper_dry_run")]
    pub operation_reaper_dry_run: bool,
    /// Master switch for Redis-backed operation cancellation keys. Defaults
    /// off so worker-side cancellation checks can soak independently.
    #[serde(default)]
    pub cancel_primitive_enabled: bool,
    /// Hard cap on the tokio runtime's blocking-thread pool. When unset, the
    /// effective value auto-scales to `available_parallelism * 16`, clamped to
    /// `[512, 8192]`. The blocking pool is consumed by plugin `spawn_blocking`
    /// calls (one thread held per concurrent plugin call) and `tokio::fs::*`
    /// operations. Override down on memory-constrained hosts (each thread
    /// reserves ~2MB of stack) or up when `plugin.evaluator_parallelism` is
    /// dialed well above CPU count.
    #[serde(default)]
    pub max_blocking_threads: Option<usize>,
    /// Per-server cap on concurrent live evaluator-batch test-case dispatch
    /// tasks (UP#14b). `start_evaluate_batch` historically spawned one tokio
    /// task per test case in an unbounded loop; under a 1000-submission burst
    /// with ICPC's 20 testcases/submission that produced 20,000 simultaneous
    /// tasks contending on the inner plugin-pool semaphore. This bounds the
    /// **spawn** itself: the dispatcher acquires a permit before spawning the
    /// per-test-case task and holds it for the task's lifetime.
    ///
    /// Default 64. Relationship to `plugin.evaluator_parallelism`: this is the
    /// outer bound on live test-case tasks server-wide; `evaluator_parallelism`
    /// is the inner bound on concurrent plugin-pool acquisitions. Configure so
    /// that `batch_evaluator_fanout_concurrency >= evaluator_parallelism`,
    /// otherwise the inner pool is never fully utilized.
    #[serde(default = "default_batch_evaluator_fanout_concurrency")]
    pub batch_evaluator_fanout_concurrency: u32,
    /// Per-batch cap on concurrent in-flight `mq.publish` + blob-externalize
    /// operations inside `start_operation_batch` (UP#14c). Historically the
    /// batch loop awaited each publish sequentially, so a 1000-op batch with
    /// 5ms publishes serialized to ~5s of wall time before the first worker
    /// could pick up a task. With `buffer_unordered(N)` the same batch takes
    /// roughly `total_publish_ms / N` plus MQ-broker throughput limits.
    ///
    /// Default 32. Trade-off: higher values reduce dispatch latency but
    /// multiply concurrent load on the Redis MQ broker (each publish is one
    /// RPUSH plus optional priority sorting). Tune down on a constrained MQ
    /// host; tune up when broker is over-provisioned and submission bursts
    /// dominate latency.
    #[serde(default = "default_operation_batch_publish_concurrency")]
    pub operation_batch_publish_concurrency: u32,
    /// Address (e.g. `"0.0.0.0:9091"`) for an **optional** dedicated TCP
    /// listener that serves only `/healthz` and `/metrics` on its own tokio
    /// runtime + OS thread (UP#14e). When `None`, behavior is identical to
    /// the legacy setup: the two endpoints are served only on the main
    /// router and therefore share the main runtime's scheduler.
    ///
    /// Setting this is **opt-in**: it lets liveness probes and metrics
    /// scrapes survive even when the api runtime is saturated by submission
    /// bursts or scheduler thrash - but operators must repoint their
    /// probe/scrape targets at this port to actually realize that
    /// isolation. The endpoints stay mounted on the main router unchanged
    /// for backward compatibility.
    #[serde(default)]
    pub healthz_listen: Option<String>,
    /// Number of tokio worker threads for the dedicated healthz/metrics
    /// runtime (UP#14e). Two is plenty: the only handlers running here are
    /// a Prometheus encode (CPU-bound, microseconds) and a 2s DB/MQ ping.
    /// Increase only if you also use the healthz listener for high-volume
    /// scraping fan-in.
    #[serde(default = "default_healthz_worker_threads")]
    pub healthz_worker_threads: u32,
    /// Master switch for the UP#38 claim fiber. When `true` (default), the
    /// per-server claim fiber polls the `submission` and `code_run` tables
    /// for `status='Queued'` rows, transitions them to `Pending`, and
    /// dispatches them via the existing plugin path. When `false`, the
    /// fiber never starts - UP#37's POST handlers still write `Queued`, but
    /// no one will claim the rows, so they will accumulate until a
    /// configuration roll-forward. Only flip this off if you need to pin a
    /// deployment to the pre-UP#37 dispatch behavior while debugging.
    #[serde(default = "default_claim_fiber_enabled")]
    pub claim_fiber_enabled: bool,
    /// Poll interval (ms) for the UP#38 claim fiber. Default 1000ms. Lower
    /// values reduce POST->Pending latency but increase background SQL load
    /// (a no-op poll is a single `SELECT ... LIMIT $batch FOR UPDATE SKIP
    /// LOCKED` per table).
    #[serde(default = "default_claim_poll_interval_ms")]
    pub claim_poll_interval_ms: u64,
    /// Bounded SELECT LIMIT for the UP#38 claim fiber. The fiber claims at
    /// most this many rows per table per tick. Defaults to 32 to match
    /// `dispatcher_concurrency`'s order of magnitude; raise it on
    /// deployments with bursty submission patterns so a single tick can
    /// drain a spike.
    #[serde(default = "default_claim_batch_size")]
    pub claim_batch_size: u32,
    /// Floor for how many submissions this server judges at once (claimed,
    /// not finished). The cap in force is the larger of this and 8 x the
    /// live worker slots seen in heartbeats, so it grows with the cluster
    /// and never throttles below worker capacity. The claim fiber takes at
    /// most `cap - in_flight` rows per tick, so a burst stays `Queued` in the
    /// database (claimable by any server) instead of piling into one
    /// process. Judging is detached after dispatch, so nothing else bounds
    /// this. 0 = no cap.
    #[serde(default = "default_max_in_flight_submissions")]
    pub max_in_flight_submissions: u32,
    /// Tick interval (seconds) for the plugin-timer delivery loop. Default 1s
    /// to suit sub-second-adjacent deadlines (e.g. a match window's fixed
    /// end time); the loop is a single indexed query against a table that is
    /// empty most of the time, so a 1s tick is cheap. Spawned unconditionally
    /// — independent of `dispatcher_lease_steal_enabled`, which governs
    /// submission judging, not plugin timers.
    #[serde(default = "default_plugin_timer_tick_interval_secs")]
    pub plugin_timer_tick_interval_secs: u64,
    /// Claim lease (seconds) for a plugin timer row: how long a claiming
    /// replica has to deliver before another replica may reclaim it. Default
    /// 30s. Mirrors `lease_ttl_secs`'s crash-recovery role for submissions.
    #[serde(default = "default_plugin_timer_lease_secs")]
    pub plugin_timer_lease_secs: i64,
    /// Bounded batch size for one plugin-timer claim tick. Default 64.
    #[serde(default = "default_plugin_timer_batch")]
    pub plugin_timer_batch: u64,
    /// Maximum delivery attempts before a plugin timer is dropped with a
    /// loud error rather than retried forever. Default 5. A trapping
    /// handler must not wedge the loop for every other plugin's timers.
    #[serde(default = "default_plugin_timer_max_attempts")]
    pub plugin_timer_max_attempts: i32,
}

/// Serde field defaults below are the single source of truth for server
/// configuration defaults: `AppConfig::load` deliberately registers no
/// builder-level `set_default` values (which would silently shadow these),
/// so a key absent from config files and environment always resolves here.
impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            cors: CorsConfig::default(),
            public_base_url: None,
            frontend_dist: default_frontend_dist(),
            trusted_proxies: Vec::new(),
            rate_limit_auth: false,
            id: String::new(),
            expects_multi_replica: false,
            dispatcher_lease_steal_enabled: false,
            dispatcher_semaphore_enabled: default_dispatcher_semaphore_enabled(),
            dispatcher_concurrency: default_dispatcher_concurrency(),
            dispatcher_admission_queue_max: default_dispatcher_admission_queue_max(),
            max_queued_submissions: default_max_queued_submissions(),
            lease_ttl_secs: default_lease_ttl_secs(),
            lease_refresh_interval_secs: default_lease_refresh_interval_secs(),
            steal_scan_interval_secs: default_steal_scan_interval_secs(),
            steal_batch_size: default_steal_batch_size(),
            sweep_interval_secs: default_sweep_interval_secs(),
            max_dispatch_retries: default_max_dispatch_retries(),
            max_stuck_retries: default_max_stuck_retries(),
            max_system_error_retries: default_max_system_error_retries(),
            sweeper_dry_run: default_sweeper_dry_run(),
            operation_reaper_enabled: default_operation_reaper_enabled(),
            operation_reaper_interval_secs: default_operation_reaper_interval_secs(),
            operation_reaper_grace_secs: default_operation_reaper_grace_secs(),
            operation_reaper_max_requeues_per_tick: default_operation_reaper_max_requeues_per_tick(
            ),
            operation_reaper_dry_run: default_operation_reaper_dry_run(),
            cancel_primitive_enabled: false,
            max_blocking_threads: None,
            batch_evaluator_fanout_concurrency: default_batch_evaluator_fanout_concurrency(),
            operation_batch_publish_concurrency: default_operation_batch_publish_concurrency(),
            healthz_listen: None,
            healthz_worker_threads: default_healthz_worker_threads(),
            claim_fiber_enabled: default_claim_fiber_enabled(),
            claim_poll_interval_ms: default_claim_poll_interval_ms(),
            claim_batch_size: default_claim_batch_size(),
            max_in_flight_submissions: default_max_in_flight_submissions(),
            plugin_timer_tick_interval_secs: default_plugin_timer_tick_interval_secs(),
            plugin_timer_lease_secs: default_plugin_timer_lease_secs(),
            plugin_timer_batch: default_plugin_timer_batch(),
            plugin_timer_max_attempts: default_plugin_timer_max_attempts(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".into()
}

fn default_port() -> u16 {
    3000
}

fn default_dispatcher_semaphore_enabled() -> bool {
    true
}

fn default_dispatcher_concurrency() -> u32 {
    16
}

fn default_dispatcher_admission_queue_max() -> u32 {
    100
}

fn default_max_queued_submissions() -> u32 {
    // 5000 - the durable `Queued` row depth at which UP#39
    // backpressure kicks in. Set to `0` to disable.
    5000
}

fn default_lease_ttl_secs() -> u64 {
    60
}

fn default_lease_refresh_interval_secs() -> u64 {
    10
}

fn default_steal_scan_interval_secs() -> u64 {
    15
}

fn default_steal_batch_size() -> u32 {
    8
}

fn default_sweep_interval_secs() -> u64 {
    300
}

fn default_max_dispatch_retries() -> u32 {
    5
}

fn default_max_stuck_retries() -> u32 {
    5
}

fn default_max_system_error_retries() -> u32 {
    // Still well above max_dispatch_retries/max_stuck_retries (both 5) so genuine
    // transient faults get real recovery headroom, but bounded so a DETERMINISTIC
    // SystemError (e.g. a checker that always dies on one case) terminalizes in
    // minutes instead of re-judging ~50x and churning the box for a contest.
    10
}

fn default_sweeper_dry_run() -> bool {
    false
}

fn default_operation_reaper_enabled() -> bool {
    true
}

fn default_operation_reaper_interval_secs() -> u64 {
    30
}

fn default_operation_reaper_grace_secs() -> i64 {
    30
}

fn default_operation_reaper_max_requeues_per_tick() -> usize {
    1000
}

fn default_operation_reaper_dry_run() -> bool {
    false
}

fn default_batch_evaluator_fanout_concurrency() -> u32 {
    64
}

fn default_operation_batch_publish_concurrency() -> u32 {
    32
}

fn default_healthz_worker_threads() -> u32 {
    2
}

fn default_claim_fiber_enabled() -> bool {
    true
}

fn default_claim_poll_interval_ms() -> u64 {
    1000
}

fn default_claim_batch_size() -> u32 {
    32
}

fn default_max_in_flight_submissions() -> u32 {
    256
}

fn default_plugin_timer_tick_interval_secs() -> u64 {
    1
}

fn default_plugin_timer_lease_secs() -> i64 {
    30
}

fn default_plugin_timer_batch() -> u64 {
    64
}

fn default_plugin_timer_max_attempts() -> i32 {
    5
}

fn default_frontend_dist() -> PathBuf {
    PathBuf::from("/srv/dist")
}

fn parse_string_vec(value: &str) -> Result<Vec<String>, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "[]" {
        return Ok(Vec::new());
    }

    if trimmed.starts_with('[') {
        return serde_json::from_str::<Vec<String>>(trimmed)
            .map_err(|err| format!("invalid JSON string array: {err}"));
    }

    Ok(trimmed
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

fn deserialize_string_vec<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: Deserializer<'de>,
{
    struct StringVecVisitor;

    impl<'de> Visitor<'de> for StringVecVisitor {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a string, comma-separated string, JSON string array, or sequence")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            parse_string_vec(value).map_err(E::custom)
        }

        fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            self.visit_str(&value)
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut values = Vec::new();
            while let Some(value) = seq.next_element::<String>()? {
                values.push(value);
            }
            Ok(values)
        }
    }

    deserializer.deserialize_any(StringVecVisitor)
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AuthConfig {
    pub jwt_secret: String,
    #[serde(default = "default_secure_cookies")]
    pub secure_cookies: bool,
    /// Max FAILED login attempts per (username, client IP) within
    /// `login_failure_window_secs` before the pair is briefly throttled. Counts
    /// only failures and clears on success, so it never throttles a legitimate
    /// sign-in even when hundreds of contestants share one venue NAT IP. `0`
    /// disables it. Deliberately generous - a false throttle during a live
    /// contest is worse than a slightly looser brute-force bound.
    #[serde(default = "default_login_failure_limit")]
    pub login_failure_limit: u32,
    /// Sliding window (seconds) over which `login_failure_limit` is counted, and
    /// the maximum a throttled pair ever waits before it frees on its own.
    #[serde(default = "default_login_failure_window_secs")]
    pub login_failure_window_secs: u64,
}

fn default_secure_cookies() -> bool {
    true
}

fn default_login_failure_limit() -> u32 {
    10
}

fn default_login_failure_window_secs() -> u64 {
    60
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SubmissionConfig {
    #[serde(default = "default_submission_max_size")]
    pub max_size: usize,
    #[serde(default = "default_submission_rate_limit_per_minute")]
    pub rate_limit_per_minute: u32,
}

impl Default for SubmissionConfig {
    fn default() -> Self {
        Self {
            max_size: default_submission_max_size(),
            rate_limit_per_minute: default_submission_rate_limit_per_minute(),
        }
    }
}

fn default_submission_max_size() -> usize {
    1_048_576
}

fn default_submission_rate_limit_per_minute() -> u32 {
    10
}

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct BootstrapConfig {
    #[serde(default)]
    pub admin_username: String,
    #[serde(default)]
    pub admin_password: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct AppConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    /// Required: `auth.jwt_secret` has no default, so configuration must
    /// always provide it (via file or `BROCCOLI__AUTH__JWT_SECRET`).
    pub auth: AuthConfig,
    #[serde(default)]
    pub plugin: plugin_core::config::PluginConfig,
    #[serde(default)]
    pub submission: SubmissionConfig,
    #[serde(default)]
    pub storage: BlobStoreConfig,
    #[serde(default)]
    pub mq: MqAppConfig,
    #[serde(default)]
    pub observability: common::config::ObservabilityConfig,
    #[serde(default = "default_batch_max_age_secs")]
    pub batch_max_age_secs: u64,
    #[serde(default)]
    pub bootstrap: BootstrapConfig,
}

fn default_batch_max_age_secs() -> u64 {
    7200
}

/// Returns true if `id` is a safe queue-suffix (alphanumeric + `-_.`,
/// non-empty, <=128 chars). Mirrors the worker-id rules so the same
/// validation applies to both sides of the MQ envelope.
pub fn is_valid_server_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.ends_with("_processing")
        && !id.ends_with("_failed")
        && !id.ends_with("_fairness_set")
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ServerIdResolveError {
    #[error(
        "server.id must be explicitly set to a valid value when server.expects_multi_replica is true"
    )]
    MultiReplicaServerIdRequired,
}

/// Resolves the effective server ID from a configured value:
/// 1. If `configured` is non-empty and valid, use it.
/// 2. If multi-replica mode is expected, reject missing/invalid configured IDs.
/// 3. Else fall back to the OS hostname (sanitized - Windows hostnames may
///    contain characters Redis dislikes in queue names).
/// 4. Else generate an 8-char random ID and warn.
pub fn resolve_server_id(
    configured: &str,
    expects_multi_replica: bool,
) -> Result<String, ServerIdResolveError> {
    let trimmed = configured.trim();
    if !trimmed.is_empty() {
        if is_valid_server_id(trimmed) {
            info!(server_id = %trimmed, "Server ID resolved from explicit configuration");
            return Ok(trimmed.to_string());
        }
        if expects_multi_replica {
            return Err(ServerIdResolveError::MultiReplicaServerIdRequired);
        }
        warn!(
            configured = %trimmed,
            "Configured server.id failed validation; falling back to hostname"
        );
    } else if expects_multi_replica {
        return Err(ServerIdResolveError::MultiReplicaServerIdRequired);
    }

    if let Ok(host) = hostname::get() {
        let lossy = host.to_string_lossy();
        let sanitized: String = lossy
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '-'
                }
            })
            .take(128)
            .collect();
        if is_valid_server_id(&sanitized) {
            warn!(
                server_id = %sanitized,
                "Server ID not configured; inferred from hostname. \
                 Set BROCCOLI__SERVER__ID explicitly in multi-replica deployments \
                 to avoid silent collisions between replicas with identical hostnames."
            );
            return Ok(sanitized);
        }
        warn!(
            hostname = %lossy,
            "OS hostname unsuitable as server.id; using random fallback"
        );
    } else {
        warn!("Could not read OS hostname; using random server.id fallback");
    }

    let fallback: String = uuid::Uuid::new_v4()
        .simple()
        .to_string()
        .chars()
        .take(8)
        .collect();
    warn!(
        server_id = %fallback,
        "Server ID generated as random fallback because hostname was unobtainable or invalid. \
         This ID changes every restart — set BROCCOLI__SERVER__ID explicitly so in-flight \
         operation results route correctly across restarts."
    );
    Ok(fallback)
}

/// Centralized derivation of the per-replica operation-result queue name.
/// Suffixing with `server_id` ensures each replica's `consume_operation_results`
/// only receives results for tasks it dispatched.
pub fn per_replica_result_queue_name(base: &str, server_id: &str) -> String {
    format!("{}.{}", base, server_id)
}

/// Auto-default for tokio's `max_blocking_threads`: 16x available CPUs,
/// clamped to `[512, 8192]`. The 16x multiplier gives roughly 8x headroom
/// over the realistic peak (`evaluator_parallelism * 2` for plugin +
/// fs/db). The floor matches tokio's historical default so very small boxes
/// keep a sane minimum, and the ceiling keeps worst-case stack budget at
/// ~16GB on enormous hosts.
pub fn auto_max_blocking_threads() -> usize {
    let par = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    (par * 16).clamp(512, 8192)
}

impl ServerConfig {
    /// Resolves the effective `max_blocking_threads` value for the tokio
    /// runtime: user-supplied value wins verbatim (no clamping); otherwise
    /// derived via [`auto_max_blocking_threads`].
    pub fn effective_max_blocking_threads(&self) -> usize {
        self.max_blocking_threads
            .unwrap_or_else(auto_max_blocking_threads)
    }
}

/// Below this length the HS256 signing key is weak enough to brute-force, and a
/// forged token is full account + permission takeover. Not enforced with a hard
/// error -- that could block a restart mid-contest -- only warned at startup,
/// consistent with the availability-first stance on auth.
pub const MIN_RECOMMENDED_JWT_SECRET_BYTES: usize = 32;

impl AppConfig {
    /// True when `auth.jwt_secret` is shorter than the recommended minimum.
    pub fn jwt_secret_is_weak(&self) -> bool {
        self.auth.jwt_secret.len() < MIN_RECOMMENDED_JWT_SECRET_BYTES
    }

    pub fn load() -> Result<Self, ConfigError> {
        // Defaults live on the struct fields as `#[serde(default = ...)]`
        // attributes - the single source of truth. Builder-level
        // `set_default` values are deliberately avoided: the config crate
        // fills them in before serde runs, so serde field defaults would be
        // silently shadowed and the two sources could drift apart.
        //
        // The one exception is the OTLP service name: the shared
        // `ObservabilityConfig` default is the generic "broccoli" (the
        // worker overrides it to "broccoli-worker" the same way), so the
        // per-binary identity is a builder-level concern, not a
        // struct-level default.
        let s = Config::builder()
            .set_default("observability.otlp.service_name", "broccoli-server")?
            .add_source(File::with_name("config/config").required(false))
            .add_source(
                Environment::with_prefix("BROCCOLI")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()?;

        s.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_server_id_charset() {
        assert!(is_valid_server_id("alpha"));
        assert!(is_valid_server_id("server-01.east_2"));
        assert!(!is_valid_server_id(""));
        assert!(!is_valid_server_id("has space"));
        assert!(!is_valid_server_id("colon:bad"));
        assert!(!is_valid_server_id("server_processing"));
        assert!(!is_valid_server_id("server_failed"));
        assert!(!is_valid_server_id("server_fairness_set"));
        assert!(!is_valid_server_id(&"x".repeat(129)));
    }

    #[test]
    fn resolves_explicit_server_id_when_valid() {
        assert_eq!(resolve_server_id("alpha", false).unwrap(), "alpha");
        assert_eq!(resolve_server_id("  alpha  ", false).unwrap(), "alpha");
    }

    #[test]
    fn resolves_invalid_explicit_id_via_fallback() {
        // "has space" is invalid; we expect a non-empty fallback (hostname or random).
        let resolved = resolve_server_id("has space", false).unwrap();
        assert!(!resolved.is_empty());
        assert!(is_valid_server_id(&resolved));
    }

    #[test]
    fn resolves_empty_to_hostname_or_random_fallback() {
        let resolved = resolve_server_id("", false).unwrap();
        assert!(!resolved.is_empty());
        assert!(is_valid_server_id(&resolved));
    }

    #[test]
    fn resolves_whitespace_only_to_fallback() {
        // Whitespace-only configured value should behave like empty:
        // produce a non-empty, well-formed ID via the hostname/random path.
        let resolved = resolve_server_id("   ", false).unwrap();
        assert!(!resolved.is_empty());
        assert!(is_valid_server_id(&resolved));
    }

    #[test]
    fn multi_replica_server_id_requires_explicit_valid_id() {
        assert!(resolve_server_id("", true).is_err());
        assert!(resolve_server_id("   ", true).is_err());
        assert!(resolve_server_id("has space", true).is_err());
        assert_eq!(resolve_server_id("server-1", true).unwrap(), "server-1");
    }

    #[test]
    fn per_replica_queue_name_appends_dotted_suffix() {
        assert_eq!(
            per_replica_result_queue_name("operation_results", "alpha"),
            "operation_results.alpha"
        );
        assert_eq!(
            per_replica_result_queue_name("operation_results", "server-1"),
            "operation_results.server-1"
        );
    }

    #[derive(Debug, Deserialize)]
    struct TrustedProxyProbe {
        #[serde(default, deserialize_with = "deserialize_string_vec")]
        trusted_proxies: Vec<String>,
    }

    #[test]
    fn trusted_proxy_env_style_string_accepts_empty_json_array() {
        let probe: TrustedProxyProbe =
            serde_json::from_value(serde_json::json!({ "trusted_proxies": "[]" })).unwrap();
        assert!(probe.trusted_proxies.is_empty());
    }

    #[test]
    fn trusted_proxy_env_style_string_accepts_json_array() {
        let probe: TrustedProxyProbe = serde_json::from_value(serde_json::json!({
            "trusted_proxies": "[\"10.0.0.0/8\", \"192.168.0.0/16\"]"
        }))
        .unwrap();
        assert_eq!(probe.trusted_proxies, vec!["10.0.0.0/8", "192.168.0.0/16"]);
    }

    #[test]
    fn trusted_proxy_env_style_string_accepts_comma_list() {
        let probe: TrustedProxyProbe = serde_json::from_value(serde_json::json!({
            "trusted_proxies": "10.0.0.0/8, 192.168.0.0/16"
        }))
        .unwrap();
        assert_eq!(probe.trusted_proxies, vec!["10.0.0.0/8", "192.168.0.0/16"]);
    }

    fn server_config_fixture(max_blocking_threads: Option<usize>) -> ServerConfig {
        ServerConfig {
            max_blocking_threads,
            ..ServerConfig::default()
        }
    }

    #[test]
    fn auto_max_blocking_threads_stays_within_clamp_bounds() {
        let resolved = auto_max_blocking_threads();
        assert!(
            (512..=8192).contains(&resolved),
            "auto-resolved value {resolved} outside [512, 8192]"
        );
    }

    #[test]
    fn effective_max_blocking_threads_honors_explicit_value() {
        let cfg = server_config_fixture(Some(2048));
        assert_eq!(cfg.effective_max_blocking_threads(), 2048);
    }

    #[test]
    fn effective_max_blocking_threads_bypasses_clamp_for_explicit_override() {
        // Operators with very memory-constrained hosts must be able to set a
        // value below the auto-default floor. Confirm 100 is honored verbatim
        // rather than silently bumped to 512.
        let cfg = server_config_fixture(Some(100));
        assert_eq!(cfg.effective_max_blocking_threads(), 100);

        // Symmetric: explicit values above the auto-default ceiling also pass
        // through unchanged.
        let cfg = server_config_fixture(Some(16_384));
        assert_eq!(cfg.effective_max_blocking_threads(), 16_384);
    }

    #[test]
    fn effective_max_blocking_threads_falls_back_to_auto_default() {
        let cfg = server_config_fixture(None);
        let resolved = cfg.effective_max_blocking_threads();
        assert_eq!(resolved, auto_max_blocking_threads());
        assert!((512..=8192).contains(&resolved));
    }

    #[test]
    fn server_config_defaults_max_stuck_retries_to_five() {
        let cfg = server_config_fixture(None);
        assert_eq!(cfg.max_stuck_retries, 5);
    }

    #[test]
    fn weak_jwt_secret_is_flagged_below_the_recommended_minimum() {
        let cfg = |secret: &str| -> AppConfig {
            Config::builder()
                .set_override("auth.jwt_secret", secret)
                .unwrap()
                .build()
                .unwrap()
                .try_deserialize()
                .unwrap()
        };
        assert!(cfg("short").jwt_secret_is_weak());
        assert!(cfg("test-secret").jwt_secret_is_weak());
        assert!(!cfg(&"a".repeat(MIN_RECOMMENDED_JWT_SECRET_BYTES)).jwt_secret_is_weak());
        assert!(!cfg(&"x".repeat(64)).jwt_secret_is_weak());
    }

    /// Guards the single-source-of-truth contract of `AppConfig::load`:
    /// with no config file and no environment, every default must resolve
    /// through the `#[serde(default = ...)]` field attributes. This mirrors
    /// the `load()` path (config-crate builder -> `try_deserialize`) minus
    /// the file/env sources, supplying only the one key with no default
    /// (`auth.jwt_secret`).
    #[test]
    fn empty_config_resolves_defaults_via_serde_attributes() {
        let built = Config::builder()
            .set_override("auth.jwt_secret", "test-secret")
            .unwrap()
            .build()
            .unwrap();
        let cfg: AppConfig = built.try_deserialize().unwrap();

        // The historical builder-level set_default block pinned this to 50,
        // silently shadowing the intended serde default of 10.
        assert_eq!(cfg.server.max_system_error_retries, 10);

        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.port, 3000);
        assert_eq!(cfg.server.dispatcher_concurrency, 16);
        assert!(cfg.server.cors.allow_origins.is_empty());
        assert_eq!(cfg.server.cors.max_age, 3600);
        assert_eq!(cfg.server.max_queued_submissions, 5000);
        assert_eq!(
            cfg.database.url,
            "postgres://postgres:password@localhost:5432/broccoli"
        );
        assert_eq!(cfg.database.max_connections, 20);
        assert!(cfg.auth.secure_cookies);
        assert_eq!(cfg.plugin.plugins_dir, PathBuf::from("./plugins"));
        assert!(cfg.plugin.enable_wasi);
        assert_eq!(cfg.submission.max_size, 1_048_576);
        assert_eq!(cfg.submission.rate_limit_per_minute, 10);
        assert!(cfg.mq.enabled);
        assert_eq!(cfg.mq.url, "redis://localhost:6379");
        assert_eq!(cfg.observability.log_format, "pretty");
        assert_eq!(cfg.observability.log_filter, "info");
        assert!(cfg.bootstrap.admin_username.is_empty());
    }

    /// A partially-populated table must still fill its remaining keys from
    /// serde defaults (previously guaranteed by set_default pre-seeding).
    #[test]
    fn partial_tables_fill_remaining_keys_from_serde_defaults() {
        let built = Config::builder()
            .set_override("auth.jwt_secret", "test-secret")
            .unwrap()
            .set_override("server.port", 8080)
            .unwrap()
            .set_override("submission.max_size", 42)
            .unwrap()
            .build()
            .unwrap();
        let cfg: AppConfig = built.try_deserialize().unwrap();

        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.server.host, "127.0.0.1");
        assert_eq!(cfg.server.max_system_error_retries, 10);
        assert_eq!(cfg.submission.max_size, 42);
        assert_eq!(cfg.submission.rate_limit_per_minute, 10);
    }

    #[test]
    fn trusted_proxy_toml_style_sequence_still_works() {
        let probe: TrustedProxyProbe = serde_json::from_value(serde_json::json!({
            "trusted_proxies": ["10.0.0.0/8"]
        }))
        .unwrap();
        assert_eq!(probe.trusted_proxies, vec!["10.0.0.0/8"]);
    }
}
