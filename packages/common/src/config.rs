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

/// The environment prefix of every setting: `a.b` is `BROCCOLI__A__B`.
pub const ENV_PREFIX: &str = "BROCCOLI";

/// The environment source of every Broccoli binary's settings.
pub fn environment() -> ::config::Environment {
    ::config::Environment::with_prefix(ENV_PREFIX)
        .separator("__")
        .try_parsing(true)
}

/// The variables among `names` that are shaped like a setting but miss the
/// double underscore after the prefix (`BROCCOLI_WORKER__TOOLS__DIR`), which
/// [`environment`] would otherwise skip without a word. Variables without a
/// `__` (`BROCCOLI_CONFIG`, the deployment scripts' own) are not settings.
pub fn misnamed_env_settings(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let single = format!("{ENV_PREFIX}_");
    let double = format!("{ENV_PREFIX}__");
    let mut out: Vec<String> = names
        .into_iter()
        .filter(|name| {
            name.starts_with(&single) && !name.starts_with(&double) && name.contains("__")
        })
        .collect();
    out.sort();
    out
}

/// Logs one warning per variable [`misnamed_env_settings`] finds in this
/// process's environment.
pub fn warn_misnamed_env_settings() {
    let names = std::env::vars_os().filter_map(|(name, _)| name.into_string().ok());
    for name in misnamed_env_settings(names) {
        let meant = format!("{ENV_PREFIX}_{}", &name[ENV_PREFIX.len()..]);
        tracing::warn!(
            env = %name,
            meant = %meant,
            "Ignoring an environment variable shaped like a setting: settings take two underscores after the prefix"
        );
    }
}

/// Logs one warning per setting the binary does not read (see
/// [`deserialize_noting_ignored`]): a removed key, or a typo. Setting `a.b` comes from
/// `a.b` in the config file or `BROCCOLI__A__B` in the environment.
pub fn warn_unknown_keys(keys: &[String]) {
    for key in keys {
        let env = format!("{ENV_PREFIX}__{}", key.replace('.', "__").to_uppercase());
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

    #[test]
    fn a_setting_with_one_underscore_after_the_prefix_is_reported() {
        let names = [
            "BROCCOLI_WORKER__TOOLS__DIR",
            "BROCCOLI__WORKER__TOOLS__DIR",
            "BROCCOLI_CONFIG",
            "BROCCOLI_DB_USER",
            "PATH",
        ]
        .map(String::from);
        assert_eq!(
            misnamed_env_settings(names),
            ["BROCCOLI_WORKER__TOOLS__DIR"]
        );
    }

    #[test]
    fn the_environment_source_reads_double_underscore_settings() {
        #[derive(serde::Deserialize)]
        struct Worker {
            tools: Tools,
        }
        #[derive(serde::Deserialize)]
        struct Tools {
            dir: String,
        }
        #[derive(serde::Deserialize)]
        struct App {
            worker: Worker,
        }
        let env = [(
            "BROCCOLI__WORKER__TOOLS__DIR".to_string(),
            "/x/tools".to_string(),
        )]
        .into_iter()
        .collect();
        let app: App = ::config::Config::builder()
            .add_source(environment().source(Some(env)))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(app.worker.tools.dir, "/x/tools");
    }

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
}
