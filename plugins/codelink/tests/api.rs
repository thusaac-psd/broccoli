use broccoli_server_sdk::prelude::*;
use codelink::api::{handle_contest_info, handle_standings};
use serde_json::json;

fn request() -> PluginHttpRequest {
    serde_json::from_value(json!({
        "method": "GET", "params": {"contest_id": "7"}
    }))
    .unwrap()
}

#[test]
fn private_or_inactive_contests_do_not_expose_standings() {
    for (is_public, is_active) in [(false, true), (true, false)] {
        let host = Host::mock();
        host.db.queue_query_result(json!([{
            "contest_type": "codelink", "is_public": is_public,
            "is_active": is_active, "phase": "during"
        }]));
        let error = handle_standings(&host, &request())
            .unwrap_err()
            .into_response();
        assert_eq!(error.status, 404);
        assert_eq!(host.db.queries().len(), 1);
    }
}

#[test]
fn route_rejects_other_contest_types_and_malformed_ids() {
    let host = Host::mock();
    host.db.queue_query_result(json!([{
        "contest_type": "ioi", "is_public": true, "is_active": true, "phase": "during"
    }]));
    assert_eq!(
        handle_standings(&host, &request())
            .unwrap_err()
            .into_response()
            .status,
        404
    );
    let mut malformed = request();
    malformed
        .params
        .insert("contest_id".into(), "invalid".into());
    assert_eq!(
        handle_standings(&host, &malformed)
            .unwrap_err()
            .into_response()
            .status,
        400
    );
    assert_eq!(host.db.queries().len(), 1);
}

#[test]
fn standings_show_global_slot_competition_and_unfinished_judgements() {
    let host = Host::mock();
    host.db.queue_query_result(json!([{
        "contest_type": "codelink", "is_public": true, "is_active": true, "phase": "during"
    }]));
    host.db.queue_query_result(json!([
        {"problem_id": 1, "label": "A"}, {"problem_id": 2, "label": "B"}
    ]));
    host.db.queue_query_result(json!([
        {"user_id": 1, "username": "one"}, {"user_id": 2, "username": "two"},
        {"user_id": 3, "username": "three"}
    ]));
    host.db.queue_query_result(json!([
        {"submission_id": 1, "user_id": 1, "problem_id": 1, "submitted_at_us": 1000000, "accepted": true, "pending": false},
        {"submission_id": 2, "user_id": 2, "problem_id": 1, "submitted_at_us": 2000000, "accepted": true, "pending": false},
        {"submission_id": 3, "user_id": 3, "problem_id": 1, "submitted_at_us": 3000000, "accepted": true, "pending": false},
        {"submission_id": 4, "user_id": 1, "problem_id": 2, "submitted_at_us": 4000000, "accepted": true, "pending": false},
        {"submission_id": 5, "user_id": 3, "problem_id": 2, "submitted_at_us": 5000000, "accepted": false, "pending": true}
    ]));
    let response = handle_standings(&host, &request()).unwrap();
    assert_eq!(response.status, 200);
    let body = response.body.unwrap();
    assert_eq!(body["phase"], "during");
    assert_eq!(body["expected_problem_count"], 16);
    assert_eq!(body["problem_count"], 2);
    assert_eq!(body["scoreboard_refresh_seconds"], 5);
    assert_eq!(body["qualified_count"], 1);
    assert_eq!(body["pending_submissions"], 1);
    assert_eq!(body["rows"][0]["user_id"], 1);
    assert_eq!(body["rows"][0]["credited"], 2);
    assert_eq!(body["rows"][0]["qualification_verdict"], "qualified");
    assert_eq!(body["rows"][2]["problems"]["1"]["status"], "slots_full");
    assert_eq!(body["problems"][1]["remaining"], 1);
}

#[test]
fn standings_expose_only_pending_or_confirmed_verdicts_and_no_tentative_qualification() {
    let host = Host::mock();
    host.db.queue_query_result(json!([{
        "contest_type": "codelink", "is_public": true, "is_active": true, "phase": "during"
    }]));
    host.db.queue_query_result(json!([
        {"problem_id": 1, "label": "A"}, {"problem_id": 2, "label": "B"}
    ]));
    host.db.queue_query_result(json!([
        {"user_id": 1, "username": "one"}, {"user_id": 2, "username": "two"},
        {"user_id": 3, "username": "three"}
    ]));
    host.db.queue_query_result(json!([
        {"submission_id": 1, "user_id": 1, "problem_id": 1, "submitted_at_us": 1000000, "accepted": false, "pending": true},
        {"submission_id": 2, "user_id": 2, "problem_id": 1, "submitted_at_us": 2000000, "accepted": true, "pending": false},
        {"submission_id": 3, "user_id": 3, "problem_id": 1, "submitted_at_us": 3000000, "accepted": true, "pending": false},
        {"submission_id": 4, "user_id": 3, "problem_id": 2, "submitted_at_us": 4000000, "accepted": true, "pending": false}
    ]));
    let body = handle_standings(&host, &request()).unwrap().body.unwrap();
    assert_eq!(body["qualified_count"], 0);
    assert!(body.get("confirmed_qualified_count").is_none());
    let rows = body["rows"].as_array().unwrap();
    for contestant in rows {
        assert!(contestant.get("qualified").is_none());
        assert!(contestant.get("qualification_confirmed").is_none());
        assert!(contestant["qualified_at_seconds"].is_null());
        assert_eq!(
            contestant["qualification_verdict"],
            if contestant["user_id"] == 3 {
                json!("pending")
            } else {
                json!(null)
            }
        );
    }
}

#[test]
fn rules_and_standings_use_the_same_contest_config_and_actual_problem_count() {
    let host = Host::mock();
    host.config.seed(
        "contest",
        "7",
        "contest",
        json!({
            "expected_problem_count": 8,
            "slots_per_problem": 3,
            "solves_to_qualify": 1,
            "scoreboard_refresh_seconds": 0,
        }),
    );
    let info = json!([{
        "contest_type": "codelink", "is_public": true, "is_active": true, "phase": "during"
    }]);
    host.db.queue_query_result(info.clone());
    host.db.queue_query_result(json!([{"problem_count": 1}]));
    let rules = handle_contest_info(&host, &request())
        .unwrap()
        .body
        .unwrap();

    host.db.queue_query_result(info);
    host.db
        .queue_query_result(json!([{"problem_id": 1, "label": "A"}]));
    host.db.queue_query_result(json!([
        {"user_id": 1, "username": "one"}, {"user_id": 2, "username": "two"},
        {"user_id": 3, "username": "three"}, {"user_id": 4, "username": "four"}
    ]));
    host.db.queue_query_result(
        serde_json::to_value(
            (1..=4)
                .map(|id| {
                    json!({
                        "submission_id": id, "user_id": id, "problem_id": 1,
                        "submitted_at_us": id * 1000000, "accepted": true, "pending": false
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap(),
    );
    let standings = handle_standings(&host, &request()).unwrap().body.unwrap();
    for field in [
        "expected_problem_count",
        "slots_per_problem",
        "solves_to_qualify",
        "scoreboard_refresh_seconds",
        "problem_count",
    ] {
        assert_eq!(standings[field], rules[field], "{field}");
    }
    assert_eq!(rules["expected_problem_count"], 8);
    assert_eq!(rules["problem_count"], 1);
    assert_eq!(rules["scoreboard_refresh_seconds"], 0);
    assert_eq!(standings["qualified_count"], 3);
    assert_eq!(
        standings["problems"][0]["awards"].as_array().unwrap().len(),
        3
    );
    assert_eq!(standings["rows"][3]["credited"], 0);
}

#[test]
fn rules_endpoint_checks_access_and_does_not_mask_bad_configuration() {
    let host = Host::mock();
    host.db.queue_query_result(json!([{
        "contest_type": "codelink", "is_public": false, "is_active": true, "phase": "during"
    }]));
    assert_eq!(
        handle_contest_info(&host, &request())
            .unwrap_err()
            .into_response()
            .status,
        404
    );

    host.config
        .seed("contest", "7", "contest", json!({"solves_to_qualify": 0}));
    for handler in [handle_contest_info, handle_standings] {
        host.db.queue_query_result(json!([{
            "contest_type": "codelink", "is_public": true, "is_active": true, "phase": "during"
        }]));
        let error = handler(&host, &request()).unwrap_err().into_response();
        assert_eq!(error.status, 500);
    }
}
