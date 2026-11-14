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
    /// Required: there is no default Redis to connect to. Empty until set;
    /// loading the configuration refuses it empty (see [`require_nonempty`]).
    #[serde(default)]
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
// `pub` so the worker's config builder uses the same default.
pub fn default_operation_queue_name() -> String {
    "operation_tasks".into()
}

impl Default for MqAppConfig {
    fn default() -> Self {
        Self {
            enabled: default_mq_enabled(),
            url: String::new(),
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

/// Deserializes `T` from `built` (the config file, environment and defaults)
/// and returns the dotted paths of the settings `T` did not read, leaving out
/// those under `sibling`: the sections and keys another Broccoli binary reads
/// from a config file it shares with this one. What `T` reads comes from its
/// own fields, so a new field is known the moment it exists, and a typo is
/// still reported.
pub fn deserialize_noting_ignored<T: serde::de::DeserializeOwned>(
    built: ::config::Config,
    sibling: &[&str],
) -> Result<(T, Vec<String>), ::config::ConfigError> {
    let mut ignored = std::collections::BTreeSet::new();
    let value = serde_ignored::deserialize(built, |path| {
        ignored.insert(path.to_string());
    })?;
    let unknown = ignored
        .into_iter()
        .filter(|path| {
            !sibling.iter().any(|s| {
                path == s
                    || path
                        .strip_prefix(s)
                        .is_some_and(|rest| rest.starts_with('.'))
            })
        })
        .collect();
    Ok((value, unknown))
}

/// Fails with a message naming `key` and its environment variable when the
/// required setting `value` is empty.
pub fn require_nonempty(
    key: &str,
    value: &str,
    env_prefix: &str,
) -> Result<(), ::config::ConfigError> {
    if value.trim().is_empty() {
        return Err(::config::ConfigError::Message(format!(
            "{key} is required: set it in the config file or as {}",
            env_name(key, env_prefix)
        )));
    }
    Ok(())
}

fn env_name(key: &str, env_prefix: &str) -> String {
    format!("{env_prefix}__{}", key.replace('.', "__").to_uppercase())
}

/// Logs one warning per setting the binary does not read (see
/// [`deserialize_noting_ignored`]): a removed key, or a typo. Setting `a.b` comes from
/// `a.b` in the config file or `{env_prefix}__A__B` in the environment.
pub fn warn_unknown_keys(keys: &[String], env_prefix: &str) {
    for key in keys {
        let env = env_name(key, env_prefix);
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

    #[derive(Debug, serde::Deserialize)]
    struct Binary {
        #[serde(default)]
        mq: MqAppConfig,
        #[serde(default)]
        own: Own,
    }

    /// Stands for a section the binary gained after this test was written.
    #[derive(Debug, Default, serde::Deserialize)]
    struct Own {
        #[serde(default)]
        newly_added: u32,
    }

    fn read(settings: &[(&str, &str)]) -> (Binary, Vec<String>) {
        let mut builder = ::config::Config::builder();
        for (key, value) in settings {
            builder = builder.set_override(*key, *value).unwrap();
        }
        deserialize_noting_ignored(builder.build().unwrap(), &["sibling", "mq.sibling_only"])
            .unwrap()
    }

    #[test]
    fn a_typo_warns_and_a_sibling_setting_does_not() {
        let (config, unknown) = read(&[
            ("mq.url", "redis://infra:6379"),
            ("mq.dlq.max_retires", "7"),
            ("clock.max_skew_ms", "0"),
            ("sibling.port", "3000"),
            ("sibling.cors.max_age", "60"),
            ("mq.sibling_only", "x"),
        ]);
        assert_eq!(config.mq.url, "redis://infra:6379");
        assert_eq!(
            unknown,
            vec!["clock".to_string(), "mq.dlq.max_retires".to_string()]
        );
    }

    #[test]
    fn a_field_the_type_has_is_read_with_no_other_edit() {
        let (config, unknown) = read(&[("own.newly_added", "5")]);
        assert_eq!(config.own.newly_added, 5);
        assert!(unknown.is_empty(), "{unknown:?}");
    }

    #[test]
    fn an_unset_required_setting_names_the_key_and_its_variable() {
        let (config, _) = read(&[]);
        assert!(require_nonempty("mq.url", &config.mq.url, "BROCCOLI").is_err());
        let err = require_nonempty("mq.url", " ", "BROCCOLI").unwrap_err();
        assert_eq!(
            err.to_string(),
            "mq.url is required: set it in the config file or as BROCCOLI__MQ__URL"
        );
    }
}
