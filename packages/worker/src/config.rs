use config::{Config, ConfigError, Environment, File};
use serde::Deserialize;

pub use common::config::MqAppConfig;
use common::storage::config::DEFAULT_MAX_BLOB_SIZE_BYTES;

#[derive(Debug, Deserialize, Clone)]
pub struct DatabaseConfig {
    #[serde(default = "default_database_url")]
    pub url: String,
    #[serde(default = "default_database_max_connections")]
    pub max_connections: u32,
}

fn default_database_url() -> String {
    "postgres://localhost/broccoli".into()
}
fn default_database_max_connections() -> u32 {
    3
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: default_database_url(),
            max_connections: default_database_max_connections(),
        }
    }
}

/// Worker-local platform tools (e.g. the `broccoli-compare` static binary).
/// `[worker.tools] dir = "/opt/broccoli/tools"` lets a `MountSource::PlatformTool`
/// resolve a tool by logical name to `<dir>/<name>`, mounted read-only.
#[derive(Debug, Deserialize, Clone, Default)]
pub struct ToolsConfig {
    #[serde(default)]
    pub dir: Option<String>,
}

#[derive(Debug, Deserialize, Clone)]
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
    #[serde(default = "default_dedup_ttl_secs")]
    pub dedup_ttl_secs: u64,
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
    /// Directory holding the `flock` files a worker uses to claim a disjoint
    /// isolate box-id slot (see `handler::box_id`). Defaults to the process
    /// temp dir.
    ///
    /// isolate runs box `N` as host UID `60000 + N`, and its `--processes`
    /// limit is enforced per UID across the whole kernel. Workers on one host
    /// must therefore claim DIFFERENT slots - which only works if they can see
    /// each other's lock files. Separate containers each have a private
    /// `/tmp`, so with the default every container claims slot 0, all run
    /// contestant code as the same UID, and exhaust each other's process
    /// limit: `execve` fails with EAGAIN and correct solutions come back
    /// SystemError or TimeLimitExceeded. Measured on a 4-worker host under a
    /// 64-submission burst: 0/64 Accepted with private temp dirs, 64/64 with a
    /// shared one.
    ///
    /// Point every worker container on a host at the SAME directory (e.g. a
    /// shared volume). Native installs, which share `/tmp`, need nothing.
    #[serde(default)]
    pub box_slot_lock_dir: Option<String>,
}

fn default_worker_id() -> String {
    "worker-1".into()
}
fn default_isolate_bin() -> String {
    "isolate".into()
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
fn default_dedup_ttl_secs() -> u64 {
    600
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
            dedup_ttl_secs: default_dedup_ttl_secs(),
            fairness_unsafe_allow: false,
            cache_leader_election_enabled: default_cache_leader_election_enabled(),
            cache_leader_ttl_secs: default_cache_leader_ttl_secs(),
            cache_leader_heartbeat_interval_secs: default_cache_leader_heartbeat_interval_secs(),
            cache_follower_poll_interval_ms: default_cache_follower_poll_interval_ms(),
            cache_follower_max_wait_secs: default_cache_follower_max_wait_secs(),
            tools: ToolsConfig::default(),
            box_slot_lock_dir: None,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
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
    // ~40 MB input+answer ~ 1.2 GB), so the old 4 GiB default held only ~3
    // problems and thrashed during a contest - evicting blobs the pre-warm had
    // just fetched. 32 GiB comfortably holds a full contest's working set on a
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

#[derive(Debug, Deserialize, Clone)]
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
}

impl WorkerAppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let config_path =
            std::env::var("BROCCOLI_CONFIG").unwrap_or_else(|_| "config/config".to_string());

        let s = Config::builder()
            .set_default("worker.id", "worker-1")?
            .set_default("worker.isolate_bin", "isolate")?
            .set_default("worker.enable_cgroups", true)?
            .set_default("worker.allow_insecure_no_cgroups", false)?
            .set_default("worker.sandbox_backend", "isolate")?
            .set_default("worker.max_concurrency", 1_i64)?
            .set_default("worker.dedup_ttl_secs", 600_i64)?
            .set_default("worker.fairness_unsafe_allow", false)?
            .set_default("worker.cache_leader_election_enabled", true)?
            .set_default("worker.cache_leader_ttl_secs", 60_i64)?
            .set_default("worker.cache_leader_heartbeat_interval_secs", 5_i64)?
            .set_default("worker.cache_follower_poll_interval_ms", 250_i64)?
            .set_default("worker.cache_follower_max_wait_secs", 30_i64)?
            .set_default("mq.enabled", true)?
            .set_default("mq.url", "redis://localhost:6379")?
            .set_default("mq.pool_size", 5_i64)?
            // Reference the shared `common` serde defaults so the worker's
            // builder defaults cannot drift from the server's.
            .set_default(
                "mq.operation_queue_name",
                common::config::default_operation_queue_name(),
            )?
            .set_default(
                "mq.operation_result_queue_name",
                common::config::default_operation_result_queue_name(),
            )?
            .set_default(
                "mq.operation_dlq_queue_name",
                common::config::default_operation_dlq_queue_name(),
            )?
            .set_default("observability.log_format", "pretty")?
            .set_default("observability.log_filter", "info")?
            .set_default("observability.otlp.service_name", "broccoli-worker")?
            .set_default("database.url", "postgres://localhost/broccoli")?
            .set_default("database.max_connections", 3_i64)?
            .set_default("storage.backend", "database")?
            .set_default("storage.data_dir", "./data")?
            .set_default("storage.max_blob_size", DEFAULT_MAX_BLOB_SIZE_BYTES as i64)?
            .set_default("storage.cache_dir", "./data/cache")?
            // Single source of truth with the serde field default. This builder
            // default takes precedence over the `#[serde(default)]`, so both must
            // agree - reference the function to avoid drift.
            .set_default("storage.max_cache_size", default_max_cache_size() as i64)?
            .add_source(File::with_name(&config_path).required(false))
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
    fn worker_config_defaults_to_single_safe_slot() {
        let worker = WorkerConfig::default();

        assert_eq!(worker.max_concurrency, 1);
        assert!(!worker.fairness_unsafe_allow);
        assert_eq!(worker.dedup_ttl_secs, 600);
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
            dedup_ttl_secs = 900
            "#,
        )
        .expect("worker config");

        assert_eq!(worker.id, "worker-a");
        assert_eq!(worker.max_concurrency, 4);
        assert!(worker.fairness_unsafe_allow);
        assert_eq!(worker.dedup_ttl_secs, 900);
    }
}
