use broccoli_server_sdk::Host;
use codelink_qualifier::{
    config::{ContestConfig, ProblemConfig},
    standings::{Problem, calculate},
};
use serde_json::json;

#[test]
fn missing_fields_keep_defaults_and_contests_have_independent_settings() {
    let host = Host::mock();
    let defaults = ContestConfig::load(&host, 7).unwrap();
    assert_eq!(defaults, ContestConfig::default());
    assert_eq!(defaults.expected_problem_count, 20);
    assert_eq!(defaults.solves_to_qualify, 3);
    assert_eq!(defaults.advancement_slots, 16);
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

    for field in [
        "slots_per_problem",
        "solves_to_qualify",
        "advancement_slots",
    ] {
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
        json!({"advancement_slots": 1001}),
        json!({"advancement_slots": "16"}),
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

#[test]
fn legacy_thresholds_are_preserved_and_problem_values_are_contest_specific() {
    let host = Host::mock();
    host.config.seed(
        "contest",
        "7",
        "contest",
        json!({
            "expected_problem_count": 16,
            "solves_to_qualify": 2,
        }),
    );
    assert_eq!(ContestConfig::load(&host, 7).unwrap().solves_to_qualify, 2);
    assert_eq!(ProblemConfig::load(&host, 7, 1).unwrap().points, 1);
    host.config
        .seed("contest_problem", "7:1", "problem", json!({"points": 2}));
    assert_eq!(ProblemConfig::load(&host, 7, 1).unwrap().points, 2);
    assert_eq!(ProblemConfig::load(&host, 7, 2).unwrap().points, 1);
    assert_eq!(ProblemConfig::load(&host, 8, 1).unwrap().points, 1);
}

#[test]
fn invalid_problem_points_cannot_silently_change_the_scoring_budget() {
    let host = Host::mock();
    for value in [
        json!({"points": 0}),
        json!({"points": 3}),
        json!({"points": -1}),
        json!({"points": 1.5}),
        json!({"points": "2"}),
        json!({"points": null}),
        json!({"point": 2}),
    ] {
        host.config.seed("contest_problem", "7:1", "problem", value);
        assert!(ProblemConfig::load(&host, 7, 1).is_err());
    }
    assert!(
        calculate(
            vec![],
            vec![Problem {
                problem_id: 1,
                label: None,
                points: 3
            }],
            vec![],
            &ContestConfig::default(),
        )
        .is_err()
    );
}
