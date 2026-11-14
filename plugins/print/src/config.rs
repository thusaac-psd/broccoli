//! Config in the `print` namespace at contest and plugin (global) scope.

use std::collections::HashSet;

use broccoli_server_sdk::types::ConfigViolation;
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
    /// Whether contestants may print code they paste or upload, besides their
    /// own submissions.
    pub allow_arbitrary: bool,
    /// Jobs each contestant may send in the contest, 0 for no limit. Staff
    /// prints do not count.
    pub max_jobs_per_user: i32,
    pub banner: String,
    pub stations: Vec<StationCredential>,
}

pub fn load_contest_config(host: &Host, contest_id: i32) -> Result<PrintConfig, SdkError> {
    host.config.get_contest(contest_id, NAMESPACE)?.parse()
}

pub fn load_global_config(host: &Host) -> Result<PrintConfig, SdkError> {
    host.config.get_global(NAMESPACE)?.parse()
}

/// What the `print` namespace refuses beyond its schema: two stations with
/// one name would share an identity, and two with one token would both
/// authenticate as whichever is listed first.
pub fn violations(config: &PrintConfig) -> Vec<ConfigViolation> {
    let mut violations = Vec::new();
    let mut names = HashSet::new();
    let mut tokens = HashSet::new();
    for (i, station) in config.stations.iter().enumerate() {
        if !names.insert(station.name.as_str()) {
            violations.push(
                ConfigViolation::new(
                    format!("stations[{i}].name"),
                    format!("another station is already named '{}'", station.name),
                )
                .key("print.config.stationNameTaken")
                .param("name", station.name.as_str()),
            );
        }
        if !tokens.insert(station.token.as_str()) {
            violations.push(
                ConfigViolation::new(
                    format!("stations[{i}].token"),
                    "another station already uses this token",
                )
                .key("print.config.stationTokenTaken"),
            );
        }
    }
    violations
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
        assert!(!cfg.allow_arbitrary, "pasted-code printing is opt-in");
        assert_eq!(cfg.max_jobs_per_user, 20);
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

    fn with_stations(stations: serde_json::Value) -> PrintConfig {
        let host = Host::mock();
        host.config
            .seed("contest", "1", NAMESPACE, json!({ "stations": stations }));
        load_contest_config(&host, 1).unwrap()
    }

    #[test]
    fn stations_with_distinct_names_and_tokens_are_accepted() {
        let config = with_stations(json!([
            { "name": "a", "token": "t1" },
            { "name": "b", "token": "t2" },
        ]));
        assert_eq!(violations(&config), []);
    }

    #[test]
    fn stations_sharing_a_name_or_a_token_are_refused() {
        let config = with_stations(json!([
            { "name": "a", "token": "t1" },
            { "name": "a", "token": "t2" },
            { "name": "c", "token": "t1" },
        ]));
        let paths: Vec<_> = violations(&config).into_iter().map(|v| v.path).collect();
        assert_eq!(paths, ["stations[1].name", "stations[2].token"]);
    }
}
