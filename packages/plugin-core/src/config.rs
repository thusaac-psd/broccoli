use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PluginConfig {
    /// The plugins shipped with this node (the release bundle). Only read,
    /// so it may be mounted read-only.
    #[serde(default = "default_plugins_dir")]
    pub plugins_dir: PathBuf,
    /// Where this server unpacks plugin archives uploaded through the admin
    /// API, fetched from the shared blob store. Must be writable and private
    /// to this server; everything in it can be fetched again, so losing it
    /// only costs a download.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
    /// Whether admins may upload plugin archives. When `false` the upload
    /// endpoints refuse every archive and this server refuses to load a
    /// previously uploaded version, so only bundled plugin code runs.
    #[serde(default = "default_uploads_enabled")]
    pub uploads_enabled: bool,
    #[serde(default = "default_enable_wasi")]
    pub enable_wasi: bool,
    /// How long a background call (judging, operations, timers) may run
    /// before its guest is cancelled. Interactive calls end with their
    /// request instead. Clamped to at least one second; there is no way to
    /// disable it.
    #[serde(default = "default_call_timeout")]
    pub call_timeout_secs: u64,
    /// Guests this server runs at once. Unset: two per available core.
    #[serde(default)]
    pub max_concurrent_calls: Option<usize>,
    /// Resident memory, in bytes (or a size such as `"4GiB"`), above which
    /// idle plugin instances are dropped. Unset: three quarters of this
    /// process's cgroup memory limit, or a quarter of the machine's memory
    /// when no cgroup limits it.
    #[serde(default, deserialize_with = "deserialize_byte_size")]
    pub memory_budget: Option<u64>,
}

/// How far past the call timeout a guest's manifest timeout ends it, as a
/// backstop to the per-call deadline. It exceeds every deadline a call can
/// get: the call timeout, a timer's lease (the call timeout plus 30 s) and a
/// request's deadline (under the 60 s request timeout).
pub const TIMEOUT_BACKSTOP_MARGIN: Duration = Duration::from_secs(60);

fn default_plugins_dir() -> PathBuf {
    PathBuf::from("./plugins")
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from("./data/plugin-cache")
}

fn default_uploads_enabled() -> bool {
    true
}

fn default_enable_wasi() -> bool {
    true
}

fn default_call_timeout() -> u64 {
    300
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            plugins_dir: default_plugins_dir(),
            cache_dir: default_cache_dir(),
            uploads_enabled: default_uploads_enabled(),
            enable_wasi: default_enable_wasi(),
            call_timeout_secs: default_call_timeout(),
            max_concurrent_calls: None,
            memory_budget: None,
        }
    }
}

impl PluginConfig {
    pub fn check_plugins_dir(&self) -> bool {
        self.plugins_dir.exists() && self.plugins_dir.is_dir()
    }

    /// The background call deadline, at least one second so `0` never means
    /// "cancel every call at once" (and can never be read as "unbounded").
    pub fn call_timeout(&self) -> Duration {
        Duration::from_secs(self.call_timeout_secs.max(1))
    }

    /// Options for a background call starting now, bounded by the call
    /// timeout.
    pub fn background_call(&self) -> crate::executor::CallOptions {
        crate::executor::CallOptions::background(std::time::Instant::now() + self.call_timeout())
    }

    /// Options for a contest call (`on_submission`, `on_code_run`) starting
    /// now, bounded by the call timeout.
    pub fn judging_call(&self) -> crate::executor::CallOptions {
        crate::executor::CallOptions::judging(std::time::Instant::now() + self.call_timeout())
    }
}

/// The one entry of `cache_dir` that is not an unpacked archive: compiled
/// plugin modules and their wasmtime cache config. Whatever cleans the
/// archive cache keeps this entry.
pub const COMPILED_MODULES_ENTRY: &str = ".compiled";

/// The wasmtime cache config that keeps compiled plugin modules in
/// `cache_dir`/[`COMPILED_MODULES_ENTRY`], so rebuilding an instance
/// deserializes its module instead of compiling it, wherever the process's
/// home directory is. `None` (with the reason logged) leaves wasmtime's
/// default location.
pub fn compile_cache_config(cache_dir: &std::path::Path) -> Option<PathBuf> {
    let prepare = || -> std::io::Result<PathBuf> {
        let entry = cache_dir.join(COMPILED_MODULES_ENTRY);
        // wasmtime keeps its modules in a `modules/` directory of its own
        // under this one.
        std::fs::create_dir_all(&entry)?;
        let modules = entry.canonicalize()?;
        let config = entry.join("wasmtime-cache.toml");
        let toml = format!("[cache]\ndirectory = {:?}\n", modules.display().to_string());
        if std::fs::read_to_string(&config).ok().as_deref() != Some(toml.as_str()) {
            std::fs::write(&config, toml)?;
        }
        config.canonicalize()
    };
    prepare()
        .inspect_err(|e| {
            tracing::warn!(error = %e, "Cannot keep compiled plugin modules in the plugin cache directory");
        })
        .ok()
}

/// A byte count: a number, or a string with an optional binary or decimal
/// unit (`"512MiB"`, `"4G"`).
fn deserialize_byte_size<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Number(u64),
        Text(String),
    }
    match Option::<Raw>::deserialize(deserializer)? {
        None => Ok(None),
        Some(Raw::Number(n)) => Ok(Some(n)),
        Some(Raw::Text(text)) => parse_byte_size(&text)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid byte size '{text}'"))),
    }
}

fn parse_byte_size(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "kib" => 1 << 10,
        "m" | "mb" => 1_000_000,
        "mib" => 1 << 20,
        "g" | "gb" => 1_000_000_000,
        "gib" => 1 << 30,
        "t" | "tb" => 1_000_000_000_000,
        "tib" => 1 << 40,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_timeout_is_always_positive() {
        let cfg = PluginConfig {
            call_timeout_secs: 0,
            ..Default::default()
        };
        assert_eq!(cfg.call_timeout(), Duration::from_secs(1));
    }

    #[test]
    fn an_empty_config_is_fully_automatic() {
        let cfg: PluginConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.call_timeout(), Duration::from_secs(300));
        assert_eq!(cfg.max_concurrent_calls, None);
        assert_eq!(cfg.memory_budget, None);
    }

    #[test]
    fn the_memory_budget_takes_a_number_or_a_size() {
        let parse = |json: &str| {
            serde_json::from_str::<PluginConfig>(json)
                .unwrap()
                .memory_budget
        };
        assert_eq!(parse(r#"{"memory_budget": 1048576}"#), Some(1 << 20));
        assert_eq!(parse(r#"{"memory_budget": "4GiB"}"#), Some(4 << 30));
        assert_eq!(parse(r#"{"memory_budget": "512 MiB"}"#), Some(512 << 20));
        assert_eq!(parse(r#"{"memory_budget": "2G"}"#), Some(2_000_000_000));
        assert!(serde_json::from_str::<PluginConfig>(r#"{"memory_budget": "lots"}"#).is_err());
    }

    #[test]
    fn compiled_modules_are_cached_under_the_plugin_cache_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = compile_cache_config(&dir.path().join("cache")).expect("writable");
        let text = std::fs::read_to_string(&config).unwrap();
        let modules = dir
            .path()
            .join("cache")
            .join(COMPILED_MODULES_ENTRY)
            .canonicalize()
            .unwrap();
        assert!(
            text.contains(&format!("{:?}", modules.display().to_string())),
            "{text}"
        );
        // Idempotent: a second load finds the same file.
        assert_eq!(
            compile_cache_config(&dir.path().join("cache")),
            Some(config)
        );
    }
}
