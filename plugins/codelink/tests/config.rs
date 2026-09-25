use broccoli_server_sdk::Host;
use codelink::{config::ContestConfig, standings::calculate};
use serde_json::json;

#[test]
fn missing_fields_keep_defaults_and_contests_have_independent_settings() {
    let host = Host::mock();
    let defaults = ContestConfig::load(&host, 7).unwrap();
    assert_eq!(defaults, ContestConfig::default());
    host.config
        .seed("contest", "7", "contest", json!({"slots_per_problem": 4}));
    let configured = ContestConfig::load(&host, 7).unwrap();
    assert_eq!(configured.slots_per_problem, 4);
    assert_eq!(configured.solves_to_qualify, defaults.solves_to_qualify);
    assert_eq!(
        configured.scoreboard_refresh_seconds,
        defaults.scoreboard_refresh_seconds
    );
    assert_eq!(ContestConfig::load(&host, 8).unwrap(), defaults);
}

#[test]
fn zero_disables_optional_settings_but_cannot_disable_scoring_rules() {
    let host = Host::mock();
    host.config.seed(
        "contest",
        "7",
        "contest",
        json!({
            "expected_problem_count": 0, "scoreboard_refresh_seconds": 0,
        }),
    );
    let config = ContestConfig::load(&host, 7).unwrap();
    assert_eq!(config.expected_problem_count, 0);
    assert_eq!(config.scoreboard_refresh_seconds, 0);

    for field in ["slots_per_problem", "solves_to_qualify"] {
        host.config
            .seed("contest", "7", "contest", json!({(field): 0}));
        assert!(ContestConfig::load(&host, 7).is_err());
    }
}

#[test]
fn malformed_or_out_of_range_settings_never_silently_revert_to_defaults() {
    let host = Host::mock();
    for value in [
        json!({"slots_per_problem": -1}),
        json!({"slots_per_problem": "3"}),
        json!({"solves_to_qualify": 1.5}),
        json!({"solves_to_qualify": 1001}),
        json!({"scoreboard_refresh_seconds": 3601}),
        json!({"expected_problem_count": null}),
        json!({"slots_per_problm": 3}),
    ] {
        host.config.seed("contest", "7", "contest", value);
        assert!(ContestConfig::load(&host, 7).is_err());
    }
    let invalid = ContestConfig {
        slots_per_problem: 0,
        ..Default::default()
    };
    assert!(calculate(vec![], vec![], vec![], &invalid).is_err());
}
