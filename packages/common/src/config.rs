use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DlqConfig {
    #[serde(default = "default_dlq_max_retries")]
    pub max_retries: u8,
    #[serde(default = "default_dlq_base_delay_ms")]
    pub base_delay_ms: u64,
    #[serde(default = "default_dlq_max_delay_ms")]
    pub max_delay_ms: u64,
    #[serde(default = "default_dlq_stuck_job_timeout_secs")]
    pub stuck_job_timeout_secs: u64,
    #[serde(default = "default_dlq_stuck_job_scan_interval_secs")]
    pub stuck_job_scan_interval_secs: u64,
    #[serde(default = "default_dlq_retry_cleanup_interval_secs")]
    pub retry_cleanup_interval_secs: u64,
    #[serde(default = "default_dlq_retry_max_age_secs")]
    pub retry_max_age_secs: u64,
    /// Hard cap on how long a row may sit in a claimed, non-terminal state
    /// (`Pending`/`Compiling`/`Running`) measured from its immutable `leased_at`
    /// dispatch anchor - independent of `lease_heartbeat_at`, which a live server
    /// keeps refreshing even for a silently-wedged worker. Past this age the
    /// stuck-job detector recovers the row directly. Well above the worst-case
    /// legitimate evaluate, below the 6h `stuck_job_timeout_secs` net. `0` disables.
    #[serde(default = "default_dlq_max_inflight_secs")]
    pub max_inflight_secs: u64,
}

fn default_dlq_max_retries() -> u8 {
    3
}
fn default_dlq_base_delay_ms() -> u64 {
    1000
}
fn default_dlq_max_delay_ms() -> u64 {
    60_000
}
fn default_dlq_stuck_job_timeout_secs() -> u64 {
    21_600
}
fn default_dlq_stuck_job_scan_interval_secs() -> u64 {
    60
}
fn default_dlq_retry_cleanup_interval_secs() -> u64 {
    300
}
fn default_dlq_retry_max_age_secs() -> u64 {
    7200
}
fn default_dlq_max_inflight_secs() -> u64 {
    3600
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            max_retries: default_dlq_max_retries(),
            base_delay_ms: default_dlq_base_delay_ms(),
            max_delay_ms: default_dlq_max_delay_ms(),
            stuck_job_timeout_secs: default_dlq_stuck_job_timeout_secs(),
            stuck_job_scan_interval_secs: default_dlq_stuck_job_scan_interval_secs(),
            retry_cleanup_interval_secs: default_dlq_retry_cleanup_interval_secs(),
            retry_max_age_secs: default_dlq_retry_max_age_secs(),
            max_inflight_secs: default_dlq_max_inflight_secs(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct MqAppConfig {
    #[serde(default = "default_mq_enabled")]
    pub enabled: bool,
    #[serde(default = "default_mq_url")]
    pub url: String,
    #[serde(default = "default_mq_pool_size")]
    pub pool_size: u8,
    #[serde(default = "default_operation_queue_name")]
    pub operation_queue_name: String,
    #[serde(default = "default_operation_result_queue_name")]
    pub operation_result_queue_name: String,
    #[serde(default = "default_operation_dlq_queue_name")]
    pub operation_dlq_queue_name: String,
    #[serde(default)]
    pub dlq: DlqConfig,
    /// Server only: how long each of the 8 operation-result consumer tasks
    /// naps after finding the result queue empty. Contest plugins judge one
    /// test case at a time, so this delay is paid once per case; a shorter
    /// nap means more Redis polls while idle. Measured on a real stack (50
    /// tiny cases; idle ops are the server's share):
    ///
    /// | nap    | per case | idle Redis ops/s |
    /// |--------|----------|------------------|
    /// | 500 ms | 0.52 s   | ~50  (the broker's old default) |
    /// | 100 ms | 0.21 s   | ~260 |
    /// | 50 ms  | 0.16 s   | ~490 |
    /// | 20 ms  | 0.15 s   | ~1150 |
    ///
    /// The tasks stay in step, so latency tracks the nap itself, not nap/8.
    #[serde(default = "default_result_poll_interval_ms")]
    pub result_poll_interval_ms: u64,
}

fn default_result_poll_interval_ms() -> u64 {
    50
}

fn default_mq_enabled() -> bool {
    true
}
fn default_mq_url() -> String {
    "redis://localhost:6379".into()
}
fn default_mq_pool_size() -> u8 {
    5
}
// `pub` so the worker's config builder can reference these as the single source
// of truth instead of re-hardcoding the same string literals in its
// `set_default(...)` calls (the builder default overrides the serde default, so
// the two must agree).
pub fn default_operation_queue_name() -> String {
    "operation_tasks".into()
}
pub fn default_operation_result_queue_name() -> String {
    "operation_results".into()
}
pub fn default_operation_dlq_queue_name() -> String {
    "operation_tasks_dlq".into()
}

impl Default for MqAppConfig {
    fn default() -> Self {
        Self {
            enabled: default_mq_enabled(),
            url: default_mq_url(),
            pool_size: default_mq_pool_size(),
            operation_queue_name: default_operation_queue_name(),
            operation_result_queue_name: default_operation_result_queue_name(),
            operation_dlq_queue_name: default_operation_dlq_queue_name(),
            dlq: DlqConfig::default(),
            result_poll_interval_ms: default_result_poll_interval_ms(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OtlpConfig {
    #[serde(default = "default_otlp_endpoint")]
    pub endpoint: String,
    #[serde(default = "default_otlp_service_name")]
    pub service_name: String,
}

fn default_otlp_endpoint() -> String {
    String::new()
}
fn default_otlp_service_name() -> String {
    "broccoli".into()
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: default_otlp_endpoint(),
            service_name: default_otlp_service_name(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ObservabilityConfig {
    #[serde(default = "default_log_format")]
    pub log_format: String,
    #[serde(default = "default_log_filter")]
    pub log_filter: String,
    #[serde(default)]
    pub otlp: OtlpConfig,
}

fn default_log_format() -> String {
    "pretty".into()
}
fn default_log_filter() -> String {
    "info".into()
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            log_format: default_log_format(),
            log_filter: default_log_filter(),
            otlp: OtlpConfig::default(),
        }
    }
}
