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

/// Every setting a Broccoli binary reads, as a dotted path; `a.*` stands for
/// any key under the map `a`. The one source of what the binaries accept:
/// each builds its typed config only from the settings listed here (see
/// [`read_known`]), so a setting a binary gains is listed here or never
/// reaches it. The server and the worker can share one config file, so a key
/// is unknown only when no binary reads it.
///
/// Derived from the config types instead, it would need one crate that sees
/// both binaries' types; the server's live in the server crate, which the
/// worker does not depend on.
pub const KNOWN_SETTINGS: &[&str] = &[
    "auth.jwt_secret",
    "auth.login_failure_limit",
    "auth.login_failure_window_secs",
    "batch_max_age_secs",
    "bootstrap.admin_password",
    "bootstrap.admin_username",
    "bootstrap.reset_admin",
    "database.plugin_url",
    "database.statement_timeout_secs",
    "database.tcp_keepalives",
    "database.url",
    "mq.dlq.base_delay_ms",
    "mq.dlq.max_delay_ms",
    "mq.dlq.max_inflight_secs",
    "mq.dlq.max_retries",
    "mq.dlq.retry_cleanup_interval_secs",
    "mq.dlq.retry_max_age_secs",
    "mq.dlq.stuck_job_scan_interval_secs",
    "mq.dlq.stuck_job_timeout_secs",
    "mq.enabled",
    "mq.operation_queue_name",
    "mq.url",
    "observability.log_filter",
    "observability.log_format",
    "observability.otlp.endpoint",
    "observability.otlp.service_name",
    "plugin.cache_dir",
    "plugin.call_timeout_secs",
    "plugin.enable_wasi",
    "plugin.evaluator_parallelism",
    "plugin.instance_min_calls_before_recycle",
    "plugin.instance_reclaim_bytes",
    "plugin.max_instance_memory_pages",
    "plugin.plugins_dir",
    "plugin.pool_acquire_timeout_secs",
    "plugin.pool_idle_timeout_secs",
    "plugin.pool_max_instances",
    "plugin.uploads_enabled",
    "server.cancel_primitive_enabled",
    "server.claim_fiber_enabled",
    "server.cors.allow_origins",
    "server.cors.max_age",
    "server.dispatcher_admission_queue_max",
    "server.dispatcher_concurrency",
    "server.dispatcher_lease_steal_enabled",
    "server.dispatcher_semaphore_enabled",
    "server.evaluate_batch_fanout_concurrency",
    "server.frontend_dist",
    "server.host",
    "server.id",
    "server.lease_refresh_interval_secs",
    "server.lease_ttl_secs",
    "server.max_blocking_threads",
    "server.max_dispatch_retries",
    "server.max_queued_submissions",
    "server.max_system_error_retries",
    "server.operation_batch_publish_concurrency",
    "server.plugin_timer_batch",
    "server.plugin_timer_lease_secs",
    "server.plugin_timer_max_attempts",
    "server.plugin_timer_tick_interval_secs",
    "server.port",
    "server.public_base_url",
    "server.rate_limit_auth",
    "server.steal_batch_size",
    "server.steal_scan_interval_secs",
    "server.trusted_proxies",
    "storage.backend",
    "storage.cache_dir",
    "storage.data_dir",
    "storage.max_blob_size",
    "storage.max_cache_size",
    "storage.object_storage.access_key",
    "storage.object_storage.bucket",
    "storage.object_storage.endpoint",
    "storage.object_storage.path_style",
    "storage.object_storage.region",
    "storage.object_storage.secret_key",
    "storage.object_storage.temp_dir",
    "submission.max_size",
    "submission.rate_limit_per_minute",
    "worker.allow_insecure_no_cgroups",
    "worker.box_slot_lock_dir",
    "worker.cache_follower_max_wait_secs",
    "worker.cache_follower_poll_interval_ms",
    "worker.cache_leader_election_enabled",
    "worker.cache_leader_heartbeat_interval_secs",
    "worker.cache_leader_ttl_secs",
    "worker.enable_cgroups",
    "worker.fairness_unsafe_allow",
    "worker.id",
    "worker.isolate_bin",
    "worker.max_concurrency",
    "worker.sandbox_backend",
    "worker.tools.dir",
];

/// The dotted leaf paths of `value`. An empty map yields `path.*`, an entry
/// of a map with no fixed keys.
pub fn setting_paths(value: &serde_json::Value) -> std::collections::BTreeSet<String> {
    fn walk(value: &serde_json::Value, prefix: &str, out: &mut std::collections::BTreeSet<String>) {
        match value.as_object() {
            Some(map) if map.is_empty() && !prefix.is_empty() => {
                out.insert(format!("{prefix}.*"));
            }
            Some(map) => {
                for (key, inner) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    walk(inner, &path, out);
                }
            }
            None => {
                out.insert(prefix.to_owned());
            }
        }
    }
    let mut out = std::collections::BTreeSet::new();
    walk(value, "", &mut out);
    out
}

/// Keeps of `built` (the config file, environment and defaults) only the
/// settings in [`KNOWN_SETTINGS`], for a binary to deserialize its typed
/// config from, and returns the dotted paths of the rest (see
/// [`unknown_keys`]) for [`warn_unknown_keys`].
pub fn read_known(
    built: ::config::Config,
) -> Result<(::config::Config, Vec<String>), ::config::ConfigError> {
    let mut raw: serde_json::Value = built.try_deserialize()?;
    let unknown = unknown_keys(&raw, KNOWN_SETTINGS);
    for path in &unknown {
        remove_path(&mut raw, path);
    }
    let known = ::config::Config::try_from(&raw)?;
    Ok((known, unknown))
}

fn remove_path(value: &mut serde_json::Value, path: &str) {
    let (parent, leaf) = match path.rsplit_once('.') {
        Some((parent, leaf)) => (Some(parent), leaf),
        None => (None, path),
    };
    let mut node = value;
    for segment in parent.into_iter().flat_map(|p| p.split('.')) {
        match node.get_mut(segment) {
            Some(next) => node = next,
            None => return,
        }
    }
    if let Some(map) = node.as_object_mut() {
        map.remove(leaf);
    }
}

/// Settings in `raw` (what the config file and environment supplied) that no
/// binary reads: for each, the shortest dotted prefix that no entry of
/// `known` (see [`KNOWN_SETTINGS`]) covers.
pub fn unknown_keys(raw: &serde_json::Value, known: &[&str]) -> Vec<String> {
    let covers = |prefix: &str| {
        known.iter().any(|entry| {
            entry == &prefix
                || entry
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    };
    let in_map = |path: &str| {
        known.iter().any(|entry| {
            entry
                .strip_suffix(".*")
                .and_then(|map| path.strip_prefix(map))
                .is_some_and(|rest| rest.starts_with('.'))
        })
    };
    let mut out = std::collections::BTreeSet::new();
    for path in setting_paths(raw) {
        let path = path.strip_suffix(".*").unwrap_or(&path);
        let mut end = 0;
        for segment in path.split('.') {
            end += segment.len();
            let prefix = &path[..end];
            if in_map(prefix) {
                break;
            }
            if !covers(prefix) {
                out.insert(prefix.to_owned());
                break;
            }
            end += 1;
        }
    }
    out.into_iter().collect()
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
    fn a_binary_reads_only_listed_settings_and_hears_of_the_rest() {
        let built = ::config::Config::builder()
            .set_override("mq.url", "redis://infra:6379")
            .unwrap()
            .set_override("mq.dlq.max_retries", "7")
            .unwrap()
            .set_override("server.claim_batch_size", 8)
            .unwrap()
            .set_override("clock.max_skew_ms", 0)
            .unwrap()
            .build()
            .unwrap();
        let (known, unknown) = read_known(built).unwrap();
        assert_eq!(
            unknown,
            vec!["clock".to_string(), "server.claim_batch_size".to_string()]
        );
        let mq: MqAppConfig = known.get("mq").unwrap();
        assert_eq!(mq.url, "redis://infra:6379");
        assert_eq!(
            mq.dlq.max_retries, 7,
            "a string from the environment still parses"
        );
        assert!(known.get::<i64>("server.claim_batch_size").is_err());
    }

    #[test]
    fn keys_no_binary_reads_are_reported_by_their_shortest_unknown_prefix() {
        let known = ["server.port", "server.id", "worker.id", "plugins.*"];
        let raw = json!({
            "server": { "port": "4000", "claim_batch_size": "8" },
            "worker": { "id": "w-1" },
            "clock": { "max_skew_ms": "0" },
            "plugins": { "anything": { "nested": 1 } },
        });
        assert_eq!(
            unknown_keys(&raw, &known),
            vec!["clock".to_string(), "server.claim_batch_size".to_string()]
        );
    }

    #[test]
    fn an_empty_map_is_a_map_of_any_keys() {
        let paths = setting_paths(&json!({ "a": { "b": 1, "m": {} }, "l": [1] }));
        assert_eq!(
            paths.into_iter().collect::<Vec<_>>(),
            vec!["a.b".to_string(), "a.m.*".to_string(), "l".to_string()]
        );
    }
}
