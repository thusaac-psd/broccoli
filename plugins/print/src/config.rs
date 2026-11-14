//! Config in the `print` namespace at contest and plugin (global) scope.

use broccoli_server_sdk::Host;
use serde::Deserialize;

pub const NAMESPACE: &str = "print";

fn default_true() -> bool {
    true
}

fn default_max_pages() -> i32 {
    10
}

fn default_max_jobs_per_user() -> i32 {
    20
}

/// One print station's credential. The station is known by `name` to
/// everything it does with `token`.
#[derive(Debug, Clone, Deserialize)]
pub struct StationCredential {
    pub name: String,
    pub token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PrintConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub require_approval: bool,
    #[serde(default = "default_max_pages")]
    pub max_pages: i32,
    /// Whether contestants may print code they paste or upload, besides their
    /// own submissions.
    #[serde(default)]
    pub allow_arbitrary: bool,
    /// Jobs each contestant may send in the contest, 0 for no limit. Staff
    /// prints do not count.
    #[serde(default = "default_max_jobs_per_user")]
    pub max_jobs_per_user: i32,
    #[serde(default)]
    pub banner: String,
    #[serde(default)]
    pub stations: Vec<StationCredential>,
}

impl Default for PrintConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            require_approval: false,
            max_pages: default_max_pages(),
            allow_arbitrary: false,
            max_jobs_per_user: default_max_jobs_per_user(),
            banner: String::new(),
            stations: Vec::new(),
        }
    }
}

/// Missing or unparseable config falls back to defaults.
pub fn load_contest_config(host: &Host, contest_id: i32) -> PrintConfig {
    match host.config.get_contest(contest_id, NAMESPACE) {
        Ok(result) => serde_json::from_value(result.config).unwrap_or_default(),
        Err(_) => PrintConfig::default(),
    }
}

pub fn load_global_config(host: &Host) -> PrintConfig {
    match host.config.get_global(NAMESPACE) {
        Ok(result) => serde_json::from_value(result.config).unwrap_or_default(),
        Err(_) => PrintConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_enable_printing_without_approval() {
        let cfg = PrintConfig::default();
        assert!(cfg.enabled);
        assert!(!cfg.require_approval);
        assert_eq!(cfg.max_pages, 10);
        assert!(!cfg.allow_arbitrary, "pasted-code printing is opt-in");
        assert_eq!(cfg.max_jobs_per_user, 20);
        assert!(cfg.stations.is_empty());
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let cfg: PrintConfig = serde_json::from_value(json!({ "require_approval": true })).unwrap();
        assert!(cfg.enabled); // still defaults to true
        assert!(cfg.require_approval);
        assert_eq!(cfg.max_pages, 10);
    }

    #[test]
    fn reads_contest_config_from_host() {
        let host = Host::mock();
        host.config.seed(
            "contest",
            "5",
            NAMESPACE,
            json!({
                "require_approval": true,
                "max_pages": 3,
                "stations": [{ "name": "room-a", "token": "t1" }],
            }),
        );
        let cfg = load_contest_config(&host, 5);
        assert!(cfg.require_approval);
        assert_eq!(cfg.max_pages, 3);
        assert_eq!(cfg.stations[0].name, "room-a");
        assert_eq!(cfg.stations[0].token, "t1");
    }

    #[test]
    fn missing_contest_config_is_default() {
        let host = Host::mock();
        let cfg = load_contest_config(&host, 9);
        assert!(cfg.enabled);
        assert_eq!(cfg.max_pages, 10);
    }
}
