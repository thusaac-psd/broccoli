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
            max_connections: default_database_max_connections(),
            plugin_max_connections: default_plugin_max_connections(),
            plugin_privileged_max_connections: default_plugin_privileged_max_connections(),
            plugin_url: None,
            statement_timeout_secs: common::db::default_statement_timeout_secs(),
            tcp_keepalives: common::db::default_tcp_keepalives(),
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
    /// When the peer is trusted, `X-Forwarded-For` is walked right to left
    /// past every trusted hop; the first untrusted hop is the client (see
    /// `client_ip`). List every proxy in the chain, and only proxies.
    #[serde(default, deserialize_with = "deserialize_string_vec")]
    pub trusted_proxies: Vec<String>,
    /// Enables IP-based throttling on `/api/v1/auth/login`.
    #[serde(default)]
    pub rate_limit_auth: bool,
    /// Logical identity of this replica. Used to derive the per-replica
    /// operation-result queue name so multiple servers behind a load balancer
    /// each receive their own plugin-dispatch results. Empty (the default)
    /// resolves to an ID derived from the machine ID (hostname, then random,
    /// as fallbacks). See [`resolve_server_id`]. Claimed in Redis at boot, so
    /// a second live replica with the same ID refuses to start.
    #[serde(default)]
    pub id: String,
    /// Multi-replica deployments require a stable explicit server ID; when
    /// true, the host-default fallback is rejected.
    #[serde(default)]
    pub expects_multi_replica: bool,
    /// Master switch for lease refresh and steal scanning. Defaults off.
    #[serde(default)]
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
    /// Per-server cap on the **durable `Queued` queue depth**: queued
    /// judgements (a queued submission counts through its judgement) and
    /// code runs combined. When the live count is at or above this value,
    /// POST endpoints that would insert a new `Queued` row reject with
    /// `503 Service Unavailable` + `Retry-After` rather than enqueueing
    /// more work (backpressure on POST).
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
    /// Fallback poll interval (ms) for the claim fiber. Default 1000ms. The
    /// fiber claims as soon as a claim notification says work was queued or
    /// room freed up; this tick only covers notifications lost while its
    /// listening connection was down. A no-op poll is one in-flight count
    /// and one `SELECT ... LIMIT $batch FOR UPDATE SKIP LOCKED` per table.
    #[serde(default = "default_claim_poll_interval_ms")]
    pub claim_poll_interval_ms: u64,
    /// Bounded SELECT LIMIT for one claim pass: at most this many rows per
    /// table. Passes repeat while each fills its batch and in-flight room
    /// is left, so this bounds one statement, not how fast a backlog drains.
    #[serde(default = "default_claim_batch_size")]
    pub claim_batch_size: u32,
    /// Floor for how many judgements and code runs this server judges at
    /// once (claimed, not finished). The cap in force is the larger of this and 8 x the
    /// live worker slots seen in heartbeats, so it grows with the cluster
    /// and never throttles below worker capacity. The claim fiber takes at
    /// most `cap - in_flight` rows, so a burst stays `Queued` in the
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
            max_dispatch_retries: default_max_dispatch_retries(),
            max_system_error_retries: default_max_system_error_retries(),
            cancel_primitive_enabled: false,
            claim_fiber_enabled: default_claim_fiber_enabled(),
            claim_poll_interval_ms: default_claim_poll_interval_ms(),
            claim_batch_size: default_claim_batch_size(),
            max_in_flight_submissions: default_max_in_flight_submissions(),
            plugin_timer_tick_interval_secs: default_plugin_timer_tick_interval_secs(),
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
    // 5000 - the durable `Queued` row depth at which POST
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

fn default_max_dispatch_retries() -> u32 {
    5
}

fn default_max_system_error_retries() -> u32 {
    // Real recovery headroom for transient faults, bounded so a deterministic
    // SystemError (e.g. a checker that always dies on one case) terminalizes in
    // minutes instead of churning the box for a contest.
    10
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
    /// Boot-time clock skew bounds against Postgres and Redis.
    #[serde(default)]
    pub clock: common::clock::ClockConfig,
    /// The [`REMOVED_KEYS`] this configuration still sets.
    #[serde(skip)]
    pub removed_keys_set: Vec<String>,
}

fn default_batch_max_age_secs() -> u64 {
    7200
}

/// Returns true if `id` is a safe queue-suffix. The same rules apply to worker
/// IDs; see [`common::node_identity::is_valid_node_id`].
pub use common::node_identity::is_valid_node_id as is_valid_server_id;

/// Resolves the effective server ID: a valid configured value, else (unless
/// multi-replica mode demands an explicit one) the host default from
/// [`common::node_identity::default_node_id`]. An explicit but invalid value is
/// an error.
pub fn resolve_server_id(
    configured: &str,
    expects_multi_replica: bool,
) -> Result<String, common::node_identity::NodeIdError> {
    common::node_identity::resolve_node_id(
        common::liveness::NodeRole::Server,
        configured,
        expects_multi_replica,
    )
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

        let removed_keys_set = removed_keys(&s);
        let mut config: Self = s.try_deserialize()?;
        config.removed_keys_set = removed_keys_set;
        Ok(config)
    }
}

/// The [`REMOVED_KEYS`] `config` sets, from any source.
fn removed_keys(config: &Config) -> Vec<String> {
    REMOVED_KEYS
        .iter()
        .filter(|key| config.get::<config::Value>(key).is_ok())
        .map(|key| key.to_string())
        .collect()
}

/// Keys that no longer exist. Configuration has no `deny_unknown_fields`, so
/// one still set in a file or the environment would be ignored without a
/// word; boot warns about each instead.
pub const REMOVED_KEYS: &[&str] = &[
    "plugin.pool_max_instances",
    "plugin.pool_acquire_timeout_secs",
    "plugin.evaluator_parallelism",
    "plugin.instance_reclaim_bytes",
    "plugin.instance_min_calls_before_recycle",
    "plugin.pool_idle_timeout_secs",
    "plugin.max_instance_memory_pages",
    "server.evaluate_batch_fanout_concurrency",
    "server.operation_batch_publish_concurrency",
    "server.plugin_timer_lease_secs",
    "server.max_blocking_threads",
];

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
        assert_eq!(resolve_server_id("alpha", false).unwrap(), "alpha");
        assert_eq!(resolve_server_id("  alpha  ", false).unwrap(), "alpha");
    }

    #[test]
    fn rejects_an_invalid_explicit_id() {
        assert!(resolve_server_id("has space", false).is_err());
    }

    #[test]
    fn resolves_empty_to_a_host_default() {
        let resolved = resolve_server_id("", false).unwrap();
        assert!(!resolved.is_empty());
        assert!(is_valid_server_id(&resolved));
    }

    #[test]
    fn resolves_whitespace_only_to_fallback() {
        // Whitespace-only configured value should behave like empty:
        // produce a non-empty, well-formed host default.
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

    #[test]
    fn removed_keys_still_set_are_reported_and_ignored() {
        let built = Config::builder()
            .set_override("auth.jwt_secret", "test-secret")
            .unwrap()
            .set_override("plugin.pool_max_instances", 64)
            .unwrap()
            .set_override("server.evaluate_batch_fanout_concurrency", 8)
            .unwrap()
            .set_override("plugin.call_timeout_secs", 60)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            removed_keys(&built),
            vec![
                "plugin.pool_max_instances".to_string(),
                "server.evaluate_batch_fanout_concurrency".to_string(),
            ]
        );
        let cfg: AppConfig = built.try_deserialize().unwrap();
        assert_eq!(cfg.plugin.call_timeout_secs, 60);
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
