//! Fixtures shared by this crate's unit tests.

use broccoli_server_sdk::Host;

use crate::model::{MatchState, RoundDef, Setup};
use crate::storage;

/// Queue the contest row `contest::check_access` reads, for a bracket
/// contest in `phase` (`"before"`, `"during"` or `"after"`).
pub(crate) fn queue_bracket_contest_phase(host: &Host, phase: &str) {
    host.db.queue_query_result(serde_json::json!([{
        "contest_type": "codelink-bracket",
        "is_public": true,
        "is_active": true,
        "phase": phase,
    }]));
}

/// [`queue_bracket_contest_phase`] for a running contest.
pub(crate) fn queue_bracket_contest_info(host: &Host) {
    queue_bracket_contest_phase(host, "during");
}

/// Queue the server time a `NOW()` read returns.
pub(crate) fn queue_now(host: &Host, now: i64) {
    host.db
        .queue_query_result(serde_json::json!([{ "now_ms": now as f64 }]));
}

/// Store `setup` and one match document.
pub(crate) fn seed(host: &Host, contest: i32, setup: &Setup, match_id: u8, m: &MatchState) {
    host.storage
        .set(&[
            (
                storage::setup_key(contest).as_str(),
                serde_json::to_string(setup).unwrap().as_str(),
            ),
            (
                storage::match_key(contest, match_id).as_str(),
                serde_json::to_string(m).unwrap().as_str(),
            ),
        ])
        .unwrap();
}

/// A 16-player bracket with its first stage open: three regular games per
/// match, two tiebreak problems in stage one and one in stage two.
pub(crate) fn setup_two_rounds() -> Setup {
    Setup {
        stage_clocks: Default::default(),
        third_place: None,
        opened_rounds: 1,
        seeds: (1..=16).collect(),
        rounds: vec![
            RoundDef {
                start_at_ms: None,
                post_order_wait_seconds: None,
                game_break_seconds: None,
                match_extra_time_seconds: None,
                wins_to_advance: None,
                group_a: vec![101, 102, 103],
                group_b: vec![201, 202, 203],
                tiebreak: vec![301, 302],
                xiaoju_seconds: 1_800,
                tiebreak_seconds: 1_800,
                break_before_seconds: 0,
            },
            RoundDef {
                start_at_ms: None,
                post_order_wait_seconds: None,
                game_break_seconds: None,
                match_extra_time_seconds: None,
                wins_to_advance: None,
                group_a: vec![401, 402, 403],
                group_b: vec![501, 502, 503],
                tiebreak: vec![601],
                xiaoju_seconds: 1_800,
                tiebreak_seconds: 1_800,
                break_before_seconds: 600,
            },
        ],
        escalation_grace_seconds: 120,
        penalty_seconds: crate::model::default_penalty_seconds(),
        ..Default::default()
    }
}
