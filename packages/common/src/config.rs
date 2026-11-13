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
    /// Prefix of every message queue key: the shared operation stream itself,
    /// and `<name>:worker:<id>`, `<name>:reply:<id>`, `<name>:dlq` (see the
    /// `mq` crate).
    #[serde(default = "default_operation_queue_name")]
    pub operation_queue_name: String,
    #[serde(default)]
    pub dlq: DlqConfig,
}

fn default_mq_enabled() -> bool {
    true
}
fn default_mq_url() -> String {
    "redis://localhost:6379".into()
}
// `pub` so the worker's config builder uses the same default.
pub fn default_operation_queue_name() -> String {
    "operation_tasks".into()
}

impl Default for MqAppConfig {
    fn default() -> Self {
        Self {
            enabled: default_mq_enabled(),
            url: default_mq_url(),
            operation_queue_name: default_operation_queue_name(),
            dlq: DlqConfig::default(),
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
    /// `json`, `pretty`, or `auto` (the default): `pretty` on a terminal,
    /// `json` otherwise (a container or a service manager's log).
    #[serde(default = "default_log_format")]
    pub log_format: String,
    #[serde(default = "default_log_filter")]
    pub log_filter: String,
    #[serde(default)]
    pub otlp: OtlpConfig,
}

fn default_log_format() -> String {
    "auto".into()
}

impl ObservabilityConfig {
    /// Whether logs are written as JSON, resolving `auto` against stdout.
    pub fn json_logs(&self) -> bool {
        match self.log_format.to_ascii_lowercase().as_str() {
            "json" => true,
            "pretty" => false,
            _ => !std::io::IsTerminal::is_terminal(&std::io::stdout()),
        }
    }
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

/// Dotted paths of settings in `raw` (what the config file and environment
/// supplied) that the binary does not read: present in `raw` but not in
/// `known`, the parsed config serialized back. A key under an empty map in
/// `known` is taken as a map entry, not an unknown key.
pub fn unknown_keys(raw: &serde_json::Value, known: &serde_json::Value) -> Vec<String> {
    fn walk(
        raw: &serde_json::Value,
        known: &serde_json::Value,
        prefix: &str,
        out: &mut Vec<String>,
    ) {
        let (Some(raw), Some(known)) = (raw.as_object(), known.as_object()) else {
            return;
        };
        if known.is_empty() {
            return;
        }
        for (key, value) in raw {
            let path = if prefix.is_empty() {
                key.clone()
            } else {
                format!("{prefix}.{key}")
            };
            match known.get(key) {
                Some(inner) => walk(value, inner, &path, out),
                None => out.push(path),
            }
        }
    }
    let mut out = Vec::new();
    walk(raw, known, "", &mut out);
    out.sort();
    out
}

/// Logs one warning per setting the binary does not read (see
/// [`unknown_keys`]): a removed key, or a typo. Setting `a.b` comes from
/// `a.b` in the config file or `{env_prefix}__A__B` in the environment.
pub fn warn_unknown_keys(keys: &[String], env_prefix: &str) {
    for key in keys {
        let env = format!("{env_prefix}__{}", key.replace('.', "__").to_uppercase());
        tracing::warn!(
            key,
            env,
            "Ignoring an unknown configuration key; it may have been removed or misspelled"
        );
    }
}

#[cfg(test)]
mod unknown_key_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_the_binary_does_not_read_are_reported_with_their_path() {
        let known = json!({ "server": { "port": 3000, "id": "" }, "plugins": {} });
        let raw = json!({
            "server": { "port": "4000", "claim_batch_size": "8" },
            "clock": { "max_skew_ms": "0" },
            "plugins": { "anything": { "nested": 1 } },
        });
        assert_eq!(
            unknown_keys(&raw, &known),
            vec!["clock".to_string(), "server.claim_batch_size".to_string()]
        );
    }
}
