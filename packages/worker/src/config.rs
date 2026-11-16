use config::{Config, ConfigError, File};
use serde::{Deserialize, Serialize};

pub use common::config::MqAppConfig;
use common::storage::config::DEFAULT_MAX_BLOB_SIZE_BYTES;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DatabaseConfig {
    /// Required: there is no default database to connect to. Empty until
    /// set; loading the configuration refuses it empty.
    #[serde(default)]
    pub url: String,
    /// Server-side `statement_timeout` for every worker connection, in seconds
    /// (`0` disables it). See `common::db`.
    #[serde(default = "common::db::default_statement_timeout_secs")]
    pub statement_timeout_secs: u64,
    /// Send server-side TCP keepalive settings on every connection. Turn this
    /// off (together with `statement_timeout_secs = 0`) behind a PgBouncer that
    /// rejects the startup `options` parameter. See `common::db::PgSession`.
    #[serde(default = "common::db::default_tcp_keepalives")]
    pub tcp_keepalives: bool,
}

impl DatabaseConfig {
    /// The session every worker connection runs with.
    pub fn session(&self) -> common::db::PgSession {
        common::db::PgSession::new(
            "broccoli-worker",
            self.statement_timeout_secs,
            self.tcp_keepalives,
        )
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            statement_timeout_secs: common::db::default_statement_timeout_secs(),
            tcp_keepalives: common::db::default_tcp_keepalives(),
        }
    }
}

/// Worker-local platform tools (e.g. the `broccoli-compare` static binary).
/// A `MountSource::PlatformTool` resolves a tool by logical name to
/// `<dir>/<name>`, mounted read-only. Unset, the directory is derived from
/// where the worker binary is (see
/// `models::operation::executor::derived_tools_dir`).
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WorkerConfig {
    #[serde(default = "default_worker_id")]
    pub id: String,
    #[serde(default = "default_isolate_bin")]
    pub isolate_bin: String,
    #[serde(default = "default_enable_cgroups")]
    pub enable_cgroups: bool,
    #[serde(default = "default_allow_insecure_no_cgroups")]
    pub allow_insecure_no_cgroups: bool,
    #[serde(default = "default_sandbox_backend")]
    pub sandbox_backend: String,
    /// Maximum submissions judged in parallel on this worker.
    ///
    /// Each in-flight box forks several processes (the `isolate --run` we
    /// spawn, isolate's keeper, the guest, and one extra exec+check pair per
    /// fused communication step), so the peak process/thread count scales with
    /// this value. Size it against the container's `pids`/`nproc` limit with
    /// headroom: if worker logs show "Transient fork failure spawning isolate
    /// --run (EAGAIN)", the container is out of fork headroom - either lower
    /// `max_concurrency` or raise the container's `--pids-limit` / `nproc`
    /// ulimit. The bounded spawn retry only smooths brief spikes, not a
    /// persistently undersized limit.
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: u32,
    #[serde(default)]
    pub fairness_unsafe_allow: bool,
    #[serde(default = "default_cache_leader_election_enabled")]
    pub cache_leader_election_enabled: bool,
    #[serde(default = "default_cache_leader_ttl_secs")]
    pub cache_leader_ttl_secs: u64,
    #[serde(default = "default_cache_leader_heartbeat_interval_secs")]
    pub cache_leader_heartbeat_interval_secs: u64,
    #[serde(default = "default_cache_follower_poll_interval_ms")]
    pub cache_follower_poll_interval_ms: u64,
    #[serde(default = "default_cache_follower_max_wait_secs")]
    pub cache_follower_max_wait_secs: u64,
    #[serde(default)]
    pub tools: ToolsConfig,
    /// Directory holding one `flock` file per isolate box id (see
    /// `handler::box_id`). Defaults to `broccoli-box-ids` in the process temp
    /// dir.
    ///
    /// isolate box ids are one namespace per host: box `N` runs as uid
    /// `first_uid + N`, and `isolate --init` on a box another worker is using
    /// re-initialises it under that worker's running program. Every worker on
    /// a host must therefore lock its ids in the SAME directory. Native
    /// installs share the temp dir and need nothing. Separate containers each
    /// have a private `/tmp`, so every worker container on a host must point
    /// this at one shared volume; the shipped compose templates do. Whatever
    /// it points at must be a directory nothing unlinks under a running
    /// worker: a lock file recreated at the same path is a new inode, whose
    /// lock is free while the old one is still held.
    #[serde(default)]
    pub box_id_lock_dir: Option<String>,
    /// The isolate config file, read for `num_boxes`: the box ids `0..num_boxes`
    /// are the ones workers on this host share.
    #[serde(default = "default_isolate_config")]
    pub isolate_config: String,
    /// Address the Prometheus `/metrics` endpoint binds. Give each worker on
    /// one host its own port so every one of them can be scraped.
    #[serde(default = "default_metrics_addr")]
    pub metrics_addr: String,
}

fn default_metrics_addr() -> String {
    "0.0.0.0:9091".into()
}

/// Empty means "derive one for this host" (see
/// [`common::node_identity::default_node_id`]);
/// [`WorkerAppConfig::resolve_worker_id`] resolves it before the worker uses it.
fn default_worker_id() -> String {
    String::new()
}
fn default_isolate_bin() -> String {
    "isolate".into()
}
fn default_isolate_config() -> String {
    "/usr/local/etc/isolate".into()
}
fn default_enable_cgroups() -> bool {
    true
}
fn default_allow_insecure_no_cgroups() -> bool {
    false
}
fn default_sandbox_backend() -> String {
    "isolate".into()
}
fn default_max_concurrency() -> u32 {
    1
}
fn default_cache_leader_election_enabled() -> bool {
    true
}
fn default_cache_leader_ttl_secs() -> u64 {
    60
}
fn default_cache_leader_heartbeat_interval_secs() -> u64 {
    5
}
fn default_cache_follower_poll_interval_ms() -> u64 {
    250
}
fn default_cache_follower_max_wait_secs() -> u64 {
    30
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            id: default_worker_id(),
            isolate_bin: default_isolate_bin(),
            enable_cgroups: default_enable_cgroups(),
            allow_insecure_no_cgroups: default_allow_insecure_no_cgroups(),
            sandbox_backend: default_sandbox_backend(),
            max_concurrency: default_max_concurrency(),
            fairness_unsafe_allow: false,
            cache_leader_election_enabled: default_cache_leader_election_enabled(),
            cache_leader_ttl_secs: default_cache_leader_ttl_secs(),
            cache_leader_heartbeat_interval_secs: default_cache_leader_heartbeat_interval_secs(),
            cache_follower_poll_interval_ms: default_cache_follower_poll_interval_ms(),
            cache_follower_max_wait_secs: default_cache_follower_max_wait_secs(),
            tools: ToolsConfig::default(),
            metrics_addr: default_metrics_addr(),
            box_id_lock_dir: None,
            isolate_config: default_isolate_config(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct StorageConfig {
    #[serde(flatten)]
    pub blob_store: common::storage::config::BlobStoreConfig,
    #[serde(default = "default_cache_dir")]
    pub cache_dir: String,
    #[serde(default = "default_max_cache_size")]
    pub max_cache_size: u64,
}

fn default_cache_dir() -> String {
    "./data/cache".into()
}
fn default_max_cache_size() -> u64 {
    // 32 GiB. A single problem's test data can be large (e.g. 30 testcases x
    // ~40 MB input+answer ~ 1.2 GB), so a 4 GiB cache holds only ~3 problems
    // and thrashes during a contest - evicting blobs the pre-warm has just
    // fetched. 32 GiB comfortably holds a full contest's working set on a
    // typical judge box; operators with a smaller disk should lower this.
    32 * 1024 * 1024 * 1024
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            blob_store: common::storage::config::BlobStoreConfig::default(),
            cache_dir: default_cache_dir(),
            max_cache_size: default_max_cache_size(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WorkerAppConfig {
    #[serde(default)]
    pub worker: WorkerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub mq: MqAppConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub observability: common::config::ObservabilityConfig,
    /// Settings the environment or config file supplied that this binary does
    /// not read, warned about once logging is up.
    #[serde(skip)]
    pub unknown_keys: Vec<String>,
}

/// What the server reads from a config file it shares with the worker: not
/// unknown here.
const SERVER_SETTINGS: &[&str] = &[
    "server",
    "auth",
    "bootstrap",
    "submission",
    "plugin",
    "batch_max_age_secs",
];

impl WorkerAppConfig {
    /// Connections the worker's database pool may hold.
    pub fn database_pool_size(&self) -> u32 {
        common::worker::database_connections(self.worker.max_concurrency)
    }

    pub fn load() -> Result<Self, ConfigError> {
        let config_path =
            std::env::var("BROCCOLI_CONFIG").unwrap_or_else(|_| "config/config".to_string());

        let s = Config::builder()
            .set_default("worker.isolate_bin", "isolate")?
            .set_default("worker.enable_cgroups", true)?
            .set_default("worker.allow_insecure_no_cgroups", false)?
            .set_default("worker.sandbox_backend", "isolate")?
            .set_default("worker.max_concurrency", 1_i64)?
            .set_default("worker.fairness_unsafe_allow", false)?
            .set_default("worker.cache_leader_election_enabled", true)?
            .set_default("worker.cache_leader_ttl_secs", 60_i64)?
            .set_default("worker.cache_leader_heartbeat_interval_secs", 5_i64)?
            .set_default("worker.cache_follower_poll_interval_ms", 250_i64)?
            .set_default("worker.cache_follower_max_wait_secs", 30_i64)?
            // Reference the shared `common` serde defaults so the worker's
            // builder defaults cannot drift from the server's.
            .set_default(
                "mq.operation_queue_name",
                common::config::default_operation_queue_name(),
            )?
            .set_default("observability.log_format", "auto")?
            .set_default("observability.log_filter", "info")?
            .set_default("observability.otlp.service_name", "broccoli-worker")?
            .set_default("storage.backend", "database")?
            .set_default("storage.data_dir", "./data")?
            .set_default("storage.max_blob_size", DEFAULT_MAX_BLOB_SIZE_BYTES as i64)?
            .set_default("storage.cache_dir", "./data/cache")?
            // Single source of truth with the serde field default. This builder
            // default takes precedence over the `#[serde(default)]`, so both must
            // agree - reference the function to avoid drift.
            .set_default("storage.max_cache_size", default_max_cache_size() as i64)?
            .add_source(File::with_name(&config_path).required(false))
            .add_source(common::config::environment())
            .build()?;

        let (mut config, unknown_keys): (Self, _) =
            common::config::deserialize_noting_ignored(s, SERVER_SETTINGS)?;
        config.unknown_keys = unknown_keys;
        common::config::require_nonempty("database.url", &config.database.url)?;
        common::config::require_nonempty("mq.url", &config.mq.url)?;
        Ok(config)
    }

    /// Replace `worker.id` with the ID this worker runs under (see
    /// [`common::node_identity::resolve_node_id`]). Separate from
    /// [`Self::load`] because it logs how the ID was chosen, so it runs once
    /// logging is up.
    pub fn resolve_worker_id(&mut self) -> anyhow::Result<()> {
        self.worker.id = common::node_identity::resolve_node_id(
            common::liveness::NodeRole::Worker,
            &self.worker.id,
            false,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_setting_the_server_reads_is_not_unknown_here() {
        let built = Config::builder()
            .set_override("worker.max_concurrency", 2)
            .unwrap()
            .set_override("storage.backend", "database")
            .unwrap()
            .set_override("storage.cache_dir", "/var/cache/broccoli")
            .unwrap()
            .set_override("server.port", 3000)
            .unwrap()
            .set_override("plugin.pool_max_instances", 64)
            .unwrap()
            .set_override("worker.max_concurency", 4)
            .unwrap()
            .build()
            .unwrap();
        let (config, unknown): (WorkerAppConfig, _) =
            common::config::deserialize_noting_ignored(built, SERVER_SETTINGS).unwrap();
        assert_eq!(config.worker.max_concurrency, 2);
        assert_eq!(config.storage.cache_dir, "/var/cache/broccoli");
        assert_eq!(unknown, vec!["worker.max_concurency".to_string()]);
    }

    #[test]
    fn worker_config_defaults_to_single_safe_slot() {
        let worker = WorkerConfig::default();

        assert_eq!(worker.max_concurrency, 1);
        assert!(!worker.fairness_unsafe_allow);
    }

    #[test]
    fn worker_config_defaults_for_cache_leader_election() {
        let worker = WorkerConfig::default();

        assert!(worker.cache_leader_election_enabled);
        assert_eq!(worker.cache_leader_ttl_secs, 60);
        assert_eq!(worker.cache_leader_heartbeat_interval_secs, 5);
        assert_eq!(worker.cache_follower_poll_interval_ms, 250);
        assert_eq!(worker.cache_follower_max_wait_secs, 30);
    }

    #[test]
    fn worker_config_deserializes_cache_leader_overrides() {
        let worker: WorkerConfig = toml::from_str(
            r#"
            id = "worker-b"
            cache_leader_election_enabled = false
            cache_leader_ttl_secs = 120
            cache_leader_heartbeat_interval_secs = 10
            cache_follower_poll_interval_ms = 500
            cache_follower_max_wait_secs = 60
            "#,
        )
        .expect("worker config");

        assert!(!worker.cache_leader_election_enabled);
        assert_eq!(worker.cache_leader_ttl_secs, 120);
        assert_eq!(worker.cache_leader_heartbeat_interval_secs, 10);
        assert_eq!(worker.cache_follower_poll_interval_ms, 500);
        assert_eq!(worker.cache_follower_max_wait_secs, 60);
    }

    #[test]
    fn worker_config_deserializes_concurrency_and_fairness_override() {
        let worker: WorkerConfig = toml::from_str(
            r#"
            id = "worker-a"
            max_concurrency = 4
            fairness_unsafe_allow = true
            "#,
        )
        .expect("worker config");

        assert_eq!(worker.id, "worker-a");
        assert_eq!(worker.max_concurrency, 4);
        assert!(worker.fairness_unsafe_allow);
    }
}
