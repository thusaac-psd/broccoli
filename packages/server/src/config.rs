use std::path::PathBuf;

use config::{Config, ConfigError, Environment, File};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, SeqAccess, Visitor},
};

pub use common::config::MqAppConfig;
pub use common::storage::config::BlobStoreConfig;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DatabaseConfig {
    #[serde(default = "default_database_url")]
    pub url: String,
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
    /// Server-side `statement_timeout` for every pooled connection, in seconds
    /// (`0` disables it). Boot-time schema sync, migrations and seeding run on a
    /// separate unbounded session. See `common::db`.
    #[serde(default = "common::db::default_statement_timeout_secs")]
    pub statement_timeout_secs: u64,
    /// Send server-side TCP keepalive settings on every connection. Turn this
    /// off (together with `statement_timeout_secs = 0`) behind a PgBouncer that
    /// rejects the startup `options` parameter. See `common::db::PgSession`.
    #[serde(default = "common::db::default_tcp_keepalives")]
    pub tcp_keepalives: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_database_url(),
            plugin_url: None,
            statement_timeout_secs: common::db::default_statement_timeout_secs(),
            tcp_keepalives: common::db::default_tcp_keepalives(),
        }
    }
}

fn default_database_url() -> String {
    "postgres://postgres:password@localhost:5432/broccoli".into()
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
    /// When the peer is trusted, `X-Forwarded-For` is walked right to left
    /// past every trusted hop; the first untrusted hop is the client (see
    /// `client_ip`). List every proxy in the chain, and only proxies.
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub trusted_proxies: Vec<String>,
    /// Per-IP throttling on `/api/v1/auth/login` (10 a minute per address,
    /// behind `trusted_proxies` the client's own). Turn off only behind a NAT
    /// that puts many contestants on one address the server can see.
    #[serde(default = "default_true")]
    pub rate_limit_auth: bool,
    /// Logical identity of this replica. Used to derive the per-replica
    /// operation-result queue name so multiple servers behind a load balancer
    /// each receive their own plugin-dispatch results. Empty (the default)
    /// resolves to an ID derived from the machine ID (hostname, then random,
    /// as fallbacks). See [`resolve_server_id`]. Claimed in Redis at boot, so
    /// a second live replica with the same ID refuses to start.
    #[serde(default)]
    pub id: String,
    /// Lease refresh and steal scanning: another server takes over the
    /// in-flight work of a server that stopped refreshing its leases (crashed,
    /// partitioned). On by default; with one server it only ever finds
    /// nothing to steal.
    #[serde(default = "default_true")]
    pub dispatcher_lease_steal_enabled: bool,
    /// Admission-control switch for plugin-dispatch background tasks.
    #[serde(default = "default_dispatcher_semaphore_enabled")]
    pub dispatcher_semaphore_enabled: bool,
    #[serde(default = "default_dispatcher_concurrency")]
    pub dispatcher_concurrency: u32,
    /// Per-server cap on in-process dispatch tasks waiting on the
    /// dispatcher semaphore. The claim fiber reserves a dispatcher slot for
    /// every unit it claims and claims fewer when this queue is full, so
    /// work it cannot start stays `Queued` for any server to claim.
    ///
    /// Not to be confused with `max_queued_submissions`, which is the
    /// **DB-row backpressure cap** at POST ingress.
    #[serde(default = "default_dispatcher_admission_queue_max")]
    pub dispatcher_admission_queue_max: u32,
    /// Cap on the **durable `Queued` queue depth** (queued judgements and
    /// code runs, fleet-wide): at or above it, POSTs that would queue more
    /// work get `503 QUEUE_OVERLOADED` + `Retry-After`. Unset, it is derived
    /// from live worker capacity (see
    /// [`crate::dispatcher::queue_depth::derived_queue_cap`]); `0` disables
    /// the cap.
    #[serde(default)]
    pub max_queued_submissions: Option<u32>,
    #[serde(default = "default_lease_ttl_secs")]
    pub lease_ttl_secs: u64,
    #[serde(default = "default_lease_refresh_interval_secs")]
    pub lease_refresh_interval_secs: u64,
    #[serde(default = "default_steal_scan_interval_secs")]
    pub steal_scan_interval_secs: u64,
    #[serde(default = "default_steal_batch_size")]
    pub steal_batch_size: u32,
    /// Dispatch retries a judgement or code run gets after its owner was
    /// lost or it got stuck (the lease steal and the stuck-job detector share
    /// this budget). Once they are spent the unit is finalized as
    /// SystemError and dead-lettered.
    #[serde(default = "default_max_dispatch_retries")]
    pub max_dispatch_retries: u32,
    /// System-error retries a submission gets since its last rejudge. A
    /// `SystemError` verdict is a SYSTEM condition (contention, lost result,
    /// wedge, spent dispatch retries), never the contestant's code, so the
    /// SystemError-retry reaper judges it again; each retry starts with a
    /// fresh dispatch-retry budget. Finite as a backstop against a problem
    /// that fails deterministically.
    #[serde(default = "default_max_system_error_retries")]
    pub max_system_error_retries: u32,
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
    /// tasks. Unbounded, `start_evaluate_batch` would spawn one tokio task per
    /// test case; a 1000-submission burst with ICPC's 20 testcases/submission
    /// would then put 20,000 simultaneous tasks in contention on the inner
    /// plugin-pool semaphore. This bounds the
    /// **spawn** itself: the dispatcher acquires a permit before spawning the
    /// per-test-case task and holds it for the task's lifetime.
    ///
    /// Default 64. Relationship to `plugin.evaluator_parallelism`: this is the
    /// outer bound on live test-case tasks server-wide; `evaluator_parallelism`
    /// is the inner bound on concurrent plugin-pool acquisitions. Configure so
    /// that `evaluate_batch_fanout_concurrency >= evaluator_parallelism`,
    /// otherwise the inner pool is never fully utilized.
    #[serde(default = "default_evaluate_batch_fanout_concurrency")]
    pub evaluate_batch_fanout_concurrency: u32,
    /// Per-batch cap on concurrent in-flight `mq.publish` + blob-externalize
    /// operations inside `start_operation_batch`. Awaiting each publish
    /// sequentially would serialize a 1000-op batch with 5ms publishes to ~5s
    /// of wall time before the first worker could pick up a task. With
    /// `buffer_unordered(N)` the same batch takes
    /// roughly `total_publish_ms / N` plus MQ-broker throughput limits.
    ///
    /// Default 32. Trade-off: higher values reduce dispatch latency but
    /// multiply concurrent load on Redis (each publish is one `XADD`, or one
    /// routing script for a pinned task). Tune down on a constrained MQ
    /// host; tune up when broker is over-provisioned and submission bursts
    /// dominate latency.
    #[serde(default = "default_operation_batch_publish_concurrency")]
    pub operation_batch_publish_concurrency: u32,
    /// Master switch for the claim fiber. When `true` (default), the
    /// per-server claim fiber claims `Queued` judgements and code runs,
    /// moves them to `Pending` and dispatches them; it is the only path
    /// into flight. When `false`, the
    /// fiber never starts - POST handlers still write `Queued`, but no one
    /// claims the rows, so they accumulate until the fiber is re-enabled.
    /// Only flip this off to stop a server from dispatching new work while
    /// debugging.
    #[serde(default = "default_claim_fiber_enabled")]
    pub claim_fiber_enabled: bool,
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
    /// 330s. Mirrors `lease_ttl_secs`'s crash-recovery role for submissions.
    /// A delivery is cancelled when its lease ends, so the lease must exceed
    /// `plugin.call_timeout_secs`; [`AppConfig::load`] rejects one that does
    /// not.
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
            rate_limit_auth: true,
            id: String::new(),
            dispatcher_lease_steal_enabled: true,
            dispatcher_semaphore_enabled: default_dispatcher_semaphore_enabled(),
            dispatcher_concurrency: default_dispatcher_concurrency(),
            dispatcher_admission_queue_max: default_dispatcher_admission_queue_max(),
            max_queued_submissions: None,
            lease_ttl_secs: default_lease_ttl_secs(),
            lease_refresh_interval_secs: default_lease_refresh_interval_secs(),
            steal_scan_interval_secs: default_steal_scan_interval_secs(),
            steal_batch_size: default_steal_batch_size(),
            max_dispatch_retries: default_max_dispatch_retries(),
            max_system_error_retries: default_max_system_error_retries(),
            cancel_primitive_enabled: false,
            max_blocking_threads: None,
            evaluate_batch_fanout_concurrency: default_evaluate_batch_fanout_concurrency(),
            operation_batch_publish_concurrency: default_operation_batch_publish_concurrency(),
            claim_fiber_enabled: default_claim_fiber_enabled(),
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

fn default_true() -> bool {
    true
}

fn default_dispatcher_semaphore_enabled() -> bool {
    true
}

fn default_dispatcher_concurrency() -> u32 {
    16
}

/// Above the claim fiber's in-flight floor (256), so the in-flight cap, which
/// scales with the fleet, is what bounds claiming.
fn default_dispatcher_admission_queue_max() -> u32 {
    1000
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

fn default_max_dispatch_retries() -> u32 {
    5
}

fn default_max_system_error_retries() -> u32 {
    // Real recovery headroom for transient faults, bounded so a deterministic
    // SystemError (e.g. a checker that always dies on one case) terminalizes in
    // minutes instead of churning the box for a contest.
    10
}

fn default_evaluate_batch_fanout_concurrency() -> u32 {
    64
}

fn default_operation_batch_publish_concurrency() -> u32 {
    32
}

fn default_claim_fiber_enabled() -> bool {
    true
}

fn default_plugin_timer_tick_interval_secs() -> u64 {
    1
}

/// Default [`ServerConfig::plugin_timer_lease_secs`]: the default plugin call
/// timeout plus 30s for waiting on a pooled instance.
pub const DEFAULT_PLUGIN_TIMER_LEASE_SECS: i64 = 330;

fn default_plugin_timer_lease_secs() -> i64 {
    DEFAULT_PLUGIN_TIMER_LEASE_SECS
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

/// 2 MiB of source: room for a contestant's generated tables, far below
/// anything that strains a server.
fn default_submission_max_size() -> usize {
    2 * 1_048_576
}

/// Ten submissions or code runs a second per user, sustained for a minute: an
/// abuse guard that a person re-submitting as fast as they can type never
/// reaches. Per-contest attempt policy belongs to plugins (`submission-limit`,
/// `cooldown`), not here.
fn default_submission_rate_limit_per_minute() -> u32 {
    600
}

/// The bootstrap admin account (see `seed::ensure_bootstrap_admin`).
///
/// When both fields are set, boot acts only on recovery: when no active user
/// holds `system:admin` (including a fresh database), or when `reset_admin`
/// is set. It then creates the account, or resets its password to
/// `admin_password`, and restores the `admin` role and its built-in
/// permissions. Otherwise a changed password, a revoked role or a renamed or
/// deleted bootstrap account survives restarts.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct BootstrapConfig {
    #[serde(default)]
    pub admin_username: String,
    #[serde(default)]
    pub admin_password: String,
    /// Force the recovery above on the next boot even though a `system:admin`
    /// holder exists, for example after the admin password was lost. Unset it
    /// again once signed in.
    #[serde(default)]
    pub reset_admin: bool,
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
    /// Settings the environment or config file supplied that this binary does
    /// not read, warned about once logging is up.
    #[serde(skip)]
    pub unknown_keys: Vec<String>,
}

fn default_batch_max_age_secs() -> u64 {
    7200
}

/// Returns true if `id` is a safe queue-suffix. The same rules apply to worker
/// IDs; see [`common::node_identity::is_valid_node_id`].
pub use common::node_identity::is_valid_node_id as is_valid_server_id;

/// Resolves the effective server ID: a valid configured value, else the host
/// default from [`common::node_identity::default_node_id`]. An explicit but
/// invalid value is an error. A default that changes across restarts (a
/// recreated container) is safe: lease stealing hands the old ID's in-flight
/// work to a live server.
pub fn resolve_server_id(configured: &str) -> Result<String, common::node_identity::NodeIdError> {
    common::node_identity::resolve_node_id(common::liveness::NodeRole::Server, configured, false)
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
        Self::from_config(s)
    }

    /// Parse and validate built configuration, noting the keys it does not
    /// read.
    fn from_config(s: Config) -> Result<Self, ConfigError> {
        let raw: serde_json::Value = s.clone().try_deserialize()?;
        let mut config: Self = s.try_deserialize()?;
        config.unknown_keys = common::config::unknown_keys(&raw, common::config::KNOWN_SETTINGS);
        config.validate()?;
        Ok(config)
    }

    /// Reject settings that cannot work together.
    fn validate(&self) -> Result<(), ConfigError> {
        let lease = self.server.plugin_timer_lease_secs;
        let call_timeout = self.plugin.call_timeout().as_secs();
        if lease <= 0 || (lease as u64) <= call_timeout {
            return Err(ConfigError::Message(format!(
                "server.plugin_timer_lease_secs ({lease}) must exceed \
                 plugin.call_timeout_secs ({call_timeout}): a timer delivery is \
                 cancelled when its lease ends"
            )));
        }
        Ok(())
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
        assert!(!is_valid_server_id(&"x".repeat(129)));
    }

    #[test]
    fn resolves_explicit_server_id_when_valid() {
        assert_eq!(resolve_server_id("alpha").unwrap(), "alpha");
        assert_eq!(resolve_server_id("  alpha  ").unwrap(), "alpha");
    }

    #[test]
    fn rejects_an_invalid_explicit_id() {
        assert!(resolve_server_id("has space").is_err());
    }

    #[test]
    fn resolves_empty_to_a_host_default() {
        let resolved = resolve_server_id("").unwrap();
        assert!(!resolved.is_empty());
        assert!(is_valid_server_id(&resolved));
    }

    #[test]
    fn resolves_whitespace_only_to_fallback() {
        // Whitespace-only configured value should behave like empty:
        // produce a non-empty, well-formed host default.
        let resolved = resolve_server_id("   ").unwrap();
        assert!(!resolved.is_empty());
        assert!(is_valid_server_id(&resolved));
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
        assert_eq!(cfg.server.max_queued_submissions, None);
        assert_eq!(
            cfg.database.url,
            "postgres://postgres:password@localhost:5432/broccoli"
        );
        assert_eq!(cfg.plugin.plugins_dir, PathBuf::from("./plugins"));
        assert!(cfg.plugin.enable_wasi);
        assert_eq!(cfg.submission.max_size, 2 * 1_048_576);
        assert_eq!(cfg.submission.rate_limit_per_minute, 600);
        assert!(cfg.mq.enabled);
        assert_eq!(cfg.mq.url, "redis://localhost:6379");
        assert_eq!(cfg.observability.log_format, "auto");
        assert_eq!(cfg.observability.log_filter, "info");
        assert!(cfg.bootstrap.admin_username.is_empty());
    }

    #[test]
    fn a_key_the_server_does_not_read_is_reported() {
        let built = Config::builder()
            .set_override("auth.jwt_secret", "test-secret")
            .unwrap()
            .set_override("server.claim_batch_size", 8)
            .unwrap()
            .set_override("server.port", 4000)
            .unwrap()
            .build()
            .unwrap();
        let cfg = AppConfig::from_config(built).unwrap();
        assert_eq!(cfg.server.port, 4000);
        assert_eq!(
            cfg.unknown_keys,
            vec!["server.claim_batch_size".to_string()]
        );
    }

    /// [`common::config::KNOWN_SETTINGS`] is exactly what the server and the
    /// worker read, so a key either binary reads never warns and a removed
    /// key always does.
    #[test]
    fn the_known_settings_are_what_the_server_and_worker_read() {
        // The object storage section is optional; present, it shows its keys.
        let server: AppConfig = Config::builder()
            .set_override("auth.jwt_secret", "test-secret")
            .unwrap()
            .set_override("storage.object_storage.bucket", "b")
            .unwrap()
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        let worker: worker::config::WorkerAppConfig = serde_json::from_value(
            serde_json::json!({ "storage": { "object_storage": { "bucket": "b" } } }),
        )
        .unwrap();
        let mut read = common::config::setting_paths(&serde_json::to_value(&server).unwrap());
        read.extend(common::config::setting_paths(
            &serde_json::to_value(&worker).unwrap(),
        ));
        let listed: std::collections::BTreeSet<String> = common::config::KNOWN_SETTINGS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let missing: Vec<_> = read.difference(&listed).collect();
        let stale: Vec<_> = listed.difference(&read).collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "update common::config::KNOWN_SETTINGS: add {missing:#?}, remove {stale:#?}"
        );
    }

    fn config_with(overrides: &[(&str, i64)]) -> AppConfig {
        let mut builder = Config::builder()
            .set_override("auth.jwt_secret", "test-secret")
            .unwrap();
        for (key, value) in overrides {
            builder = builder.set_override(*key, *value).unwrap();
        }
        builder.build().unwrap().try_deserialize().unwrap()
    }

    #[test]
    fn default_timer_lease_exceeds_the_default_call_timeout() {
        config_with(&[])
            .validate()
            .expect("defaults are consistent");
    }

    #[test]
    fn a_timer_lease_not_above_the_call_timeout_is_rejected() {
        for lease in [0, 60, 300] {
            let cfg = config_with(&[
                ("server.plugin_timer_lease_secs", lease),
                ("plugin.call_timeout_secs", 300),
            ]);
            let err = cfg
                .validate()
                .expect_err("lease must exceed the call timeout");
            assert!(err.to_string().contains("plugin_timer_lease_secs"), "{err}");
        }
        config_with(&[
            ("server.plugin_timer_lease_secs", 61),
            ("plugin.call_timeout_secs", 60),
        ])
        .validate()
        .expect("a lease above the call timeout is accepted");
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
        assert_eq!(cfg.submission.rate_limit_per_minute, 600);
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
