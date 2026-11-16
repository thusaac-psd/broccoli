//! Config in the `print` namespace at contest and plugin (global) scope.

use broccoli_server_sdk::{Host, error::SdkError};
use serde::Deserialize;

pub const NAMESPACE: &str = "print";

/// One print station's credential. The station is known by `name` to
/// everything it does with `token`.
#[derive(Debug, Clone, Deserialize)]
pub struct StationCredential {
    pub name: String,
    pub token: String,
}

/// The `print` config namespace. Its defaults and bounds live in the
/// namespace's schema in `plugin.toml`; the host fills the defaults in, so
/// every field is present here.
#[derive(Debug, Clone, Deserialize)]
pub struct PrintConfig {
    pub enabled: bool,
    pub require_approval: bool,
    pub max_pages: i32,
    pub banner: String,
    pub stations: Vec<StationCredential>,
}

pub fn load_contest_config(host: &Host, contest_id: i32) -> Result<PrintConfig, SdkError> {
    host.config.get_contest(contest_id, NAMESPACE)?.parse()
}

pub fn load_global_config(host: &Host) -> Result<PrintConfig, SdkError> {
    host.config.get_global(NAMESPACE)?.parse()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn defaults_enable_printing_without_approval() {
        let cfg = load_global_config(&Host::mock()).unwrap();
        assert!(cfg.enabled);
        assert!(!cfg.require_approval);
        assert_eq!(cfg.max_pages, 10);
        assert!(cfg.stations.is_empty());
    }

    #[test]
    fn partial_config_keeps_other_defaults() {
        let host = Host::mock();
        host.config.seed(
            "contest",
            "5",
            NAMESPACE,
            json!({ "require_approval": true }),
        );
        let cfg = load_contest_config(&host, 5).unwrap();
        assert!(cfg.enabled);
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
        let cfg = load_contest_config(&host, 5).unwrap();
        assert!(cfg.require_approval);
        assert_eq!(cfg.max_pages, 3);
        assert_eq!(cfg.stations[0].name, "room-a");
        assert_eq!(cfg.stations[0].token, "t1");
    }

    #[test]
    fn missing_contest_config_is_default() {
        let host = Host::mock();
        let cfg = load_contest_config(&host, 9).unwrap();
        assert!(cfg.enabled);
        assert_eq!(cfg.max_pages, 10);
    }
}
