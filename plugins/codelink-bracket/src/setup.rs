//! Staff configuration of bracket seeding and unopened stages.

use std::collections::HashMap;

use broccoli_server_sdk::permissions as perm;
use broccoli_server_sdk::prelude::*;
use serde::Deserialize;

use crate::model::{RoundDef, Setup};
use crate::storage;

/// Validate a full bracket setup before it is persisted. Every failure here
/// is a rejection, not a warning -- see the module doc comment for why.
pub fn validate_setup(setup: &Setup, seeds: &[i32]) -> Result<(), String> {
    if !(2..=128).contains(&seeds.len()) || !seeds.len().is_power_of_two() {
        return Err("seeds must contain a power of two players between 2 and 128".into());
    }
    let mut seed_set: Vec<i32> = seeds.to_vec();
    seed_set.sort_unstable();
    seed_set.dedup();
    if seed_set.len() != seeds.len() {
        return Err("seeds must contain distinct player ids".to_string());
    }

    if setup.rounds.len() != seeds.len().ilog2() as usize {
        return Err(format!(
            "setup must define exactly {} rounds, got {}",
            seeds.len().ilog2(),
            setup.rounds.len()
        ));
    }

    if setup.penalty_seconds < 0 {
        return Err("penalty_seconds must be greater than or equal to 0".to_string());
    }
    if setup.match_extra_time_seconds < 0 {
        return Err("match_extra_time_seconds must be non-negative".into());
    }
    if setup.escalation_grace_seconds < 0 || setup.escalation_grace_seconds > i64::MAX / 1_000 {
        return Err(
            "escalation_grace_seconds must be nonnegative seconds within the timer range".into(),
        );
    }

    // A problem belongs to exactly one group of one round. Tracked globally
    // (not per-round) because the visibility kernel's opponent-ranking
    // branch would otherwise grant a player `Allow` on a problem that is
    // still their own hidden problem in a DIFFERENT match -- see the module
    // doc comment on this file's caller in `docs/superpowers/plans/
    // 2026-09-19-afternoon-bracket.md` (Task 3) for the full reasoning.
    let mut seen_problems: HashMap<i32, ()> = HashMap::new();
    for (round_idx, round) in setup.rounds.iter().enumerate() {
        if round.group_a.is_empty()
            || round.group_a.len() != round.group_b.len()
            || round.group_a.len() + round.tiebreak.len() > 256
        {
            return Err(format!(
                "round {round_idx} needs equally sized nonempty problem groups and at most 256 games including tiebreaks"
            ));
        }
        if round
            .xiaoju_seconds
            .checked_add(setup.match_extra_time_seconds)
            .and_then(|seconds| seconds.checked_mul(1_000))
            .is_none()
        {
            return Err(format!(
                "round {round_idx}'s xiaoju_seconds plus match_extra_time_seconds exceed the timer range"
            ));
        }
        for (name, seconds) in [
            ("xiaoju_seconds", round.xiaoju_seconds),
            ("tiebreak_seconds", round.tiebreak_seconds),
        ] {
            if seconds <= 0 || seconds > i64::MAX / 1_000 {
                return Err(format!(
                    "round {round_idx}'s {name} must be positive seconds within the timer range"
                ));
            }
        }
        if round.break_before_seconds < 0 || round.break_before_seconds > i64::MAX / 1_000 {
            return Err(format!(
                "round {round_idx}'s break_before_seconds must be nonnegative seconds within the timer range"
            ));
        }
        if round_idx == 0 && round.break_before_seconds != 0 {
            return Err(
                "round 0's break_before_seconds must be 0; round one opens with the contest".into(),
            );
        }
        let mut group_a_set: Vec<i32> = round.group_a.to_vec();
        group_a_set.sort_unstable();
        group_a_set.dedup();
        if group_a_set.len() != round.group_a.len() {
            return Err(format!(
                "round {round_idx}'s group_a must contain distinct problem ids"
            ));
        }

        let mut group_b_set: Vec<i32> = round.group_b.to_vec();
        group_b_set.sort_unstable();
        group_b_set.dedup();
        if group_b_set.len() != round.group_b.len() {
            return Err(format!(
                "round {round_idx}'s group_b must contain distinct problem ids"
            ));
        }

        if round.tiebreak.is_empty() {
            return Err(format!(
                "round {round_idx}'s tiebreak list must not be empty"
            ));
        }

        for problem_id in round
            .group_a
            .iter()
            .chain(round.group_b.iter())
            .chain(round.tiebreak.iter())
        {
            if seen_problems.insert(*problem_id, ()).is_some() {
                return Err(format!(
                    "problem {problem_id} appears in more than one group or tiebreak list"
                ));
            }
        }
    }

    Ok(())
}

/// Wire body for `POST /contests/{contest_id}/setup`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupRequest {
    rounds: Vec<RoundDef>,
    #[serde(default)]
    match_extra_time_seconds: i64,
    #[serde(default = "crate::model::default_penalty_seconds")]
    penalty_seconds: i64,
    /// Optional: a request that omits this falls back to
    /// [`crate::model::default_escalation_grace_seconds`], same as a
    /// persisted `Setup` document written before this field existed.
    #[serde(default = "crate::model::default_escalation_grace_seconds")]
    escalation_grace_seconds: i64,
    seeds: Vec<i32>,
}

/// Save a complete configuration. Opening a stage and editing its rules CAS
/// the same setup document, so an edit cannot race past the frozen prefix.
pub fn handle_setup(host: &Host, req: &PluginHttpRequest) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;
    if !req.has_permission(perm::CONTEST_MANAGE) {
        return Err(PluginHttpResponse::error(
            403,
            "Setting up the bracket requires contest:manage",
        )
        .into());
    }

    let body = req
        .body
        .clone()
        .ok_or_else(|| PluginHttpResponse::error(400, "Missing request body"))?;
    let body: SetupRequest = serde_json::from_value(body)
        .map_err(|e| PluginHttpResponse::error(400, format!("Invalid request body: {e}")))?;

    let mut setup = Setup {
        rounds: body.rounds,
        seeds: body.seeds,
        opened_rounds: 0,
        match_extra_time_seconds: body.match_extra_time_seconds,
        penalty_seconds: body.penalty_seconds,
        escalation_grace_seconds: body.escalation_grace_seconds,
    };
    validate_setup(&setup, &setup.seeds).map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let key = storage::setup_key(contest_id);
    let mut saved = false;
    for _ in 0..100 {
        let snapshot = host.storage.snapshot(&key)?;
        if let Some(previous) = snapshot.as_ref() {
            let previous: Setup = serde_json::from_str(&previous.value)?;
            validate_edit(&previous, &setup, info.phase != "before")
                .map_err(|msg| PluginHttpResponse::error(409, msg))?;
            setup.opened_rounds = previous.opened_rounds;
        }
        if host.storage.compare_and_set_snapshot(
            &key,
            snapshot.as_ref(),
            &serde_json::to_string(&setup)?,
        )? {
            saved = true;
            break;
        }
    }
    if !saved {
        return Err(
            PluginHttpResponse::error(409, "Setup changed concurrently; retry the edit").into(),
        );
    }
    // Also recover an interrupted opening of the most recently frozen stage.
    for round in setup.opened_rounds.max(1)..=(setup.opened_rounds + 1).min(setup.rounds.len()) {
        crate::judge::try_open_stage(host, contest_id, round as u8)?;
    }

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({ "ok": true })),
    })
}

fn validate_edit(previous: &Setup, next: &Setup, started: bool) -> Result<(), String> {
    if (started || previous.opened_rounds > 0)
        && (previous.seeds != next.seeds || previous.rounds.len() != next.rounds.len())
    {
        return Err("Bracket size and player seeding are fixed once the contest starts".into());
    }
    if previous.rounds[..previous.opened_rounds] != next.rounds[..previous.opened_rounds] {
        return Err("Opened stages cannot be edited, including matches still ranking".into());
    }
    if (started || previous.opened_rounds > 0)
        && (previous.penalty_seconds != next.penalty_seconds
            || previous.match_extra_time_seconds != next.match_extra_time_seconds
            || previous.escalation_grace_seconds != next.escalation_grace_seconds)
    {
        return Err("Shared settings are fixed once the contest starts".into());
    }
    Ok(())
}

/// Staff-only configuration; problem assignments must not leak to contestants.
pub fn handle_get_setup(
    host: &Host,
    req: &PluginHttpRequest,
) -> Result<PluginHttpResponse, ApiError> {
    let contest_id: i32 = req.param("contest_id")?;
    let info = contest::check_access(host, req, contest_id)?;
    info.require_type("codelink-bracket")?;
    if !req.has_permission(perm::CONTEST_MANAGE) {
        return Err(
            PluginHttpResponse::error(403, "Editing the bracket requires contest:manage").into(),
        );
    }
    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(
            serde_json::json!({"setup": storage::load_setup(host, contest_id)?, "started": info.phase != "before"}),
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_seeds() -> [i32; 16] {
        std::array::from_fn(|i| (i as i32) + 1)
    }

    #[test]
    fn bracket_size_determines_stage_count_and_groups_determine_game_count() {
        for players in [2, 8, 16, 32, 128] {
            let mut setup = valid_setup();
            setup.seeds = (1..=players).collect();
            setup.rounds = (0..(players as u32).ilog2())
                .map(|i| RoundDef {
                    group_a: (100 + i as i32 * 20..105 + i as i32 * 20).collect(),
                    group_b: (110 + i as i32 * 20..115 + i as i32 * 20).collect(),
                    tiebreak: vec![119 + i as i32 * 20],
                    ..setup.rounds[0].clone()
                })
                .collect();
            assert!(validate_setup(&setup, &setup.seeds).is_ok());
            setup.rounds[0].group_b.pop();
            assert!(validate_setup(&setup, &setup.seeds).is_err());
        }
        for players in [0, 1, 3, 6, 129, 256] {
            let setup = valid_setup();
            assert!(validate_setup(&setup, &(1..=players).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn game_count_bounds_leave_room_for_a_tiebreak_and_fit_the_game_index() {
        for count in [1, 2, 5, 255, 256] {
            let setup = Setup {
                seeds: vec![1, 2],
                rounds: vec![RoundDef {
                    group_a: (1..=count).collect(),
                    group_b: (1001..1001 + count).collect(),
                    tiebreak: vec![2000],
                    xiaoju_seconds: 60,
                    tiebreak_seconds: 90,
                    break_before_seconds: 0,
                }],
                ..Default::default()
            };
            assert_eq!(validate_setup(&setup, &setup.seeds).is_ok(), count < 256);
        }
    }

    #[test]
    fn future_stage_edits_are_allowed_but_opened_stages_and_shared_settings_are_frozen() {
        let mut previous = valid_setup();
        previous.opened_rounds = 1;
        let mut next = previous.clone();
        next.rounds[1].group_a.extend([290, 291]);
        next.rounds[1].group_b.extend([292, 293]);
        next.rounds[1].xiaoju_seconds = 3600;
        next.rounds[1].break_before_seconds = 1200;
        assert!(validate_setup(&next, &next.seeds).is_ok());
        assert!(validate_edit(&previous, &next, true).is_ok());
        next.rounds[0].xiaoju_seconds += 1;
        assert!(validate_edit(&previous, &next, true).is_err());
        next = previous.clone();
        next.penalty_seconds += 1;
        assert!(validate_edit(&previous, &next, true).is_err());
        next = previous.clone();
        next.seeds.swap(0, 1);
        assert!(validate_edit(&previous, &next, false).is_err());
        previous.opened_rounds = 0;
        assert!(validate_edit(&previous, &next, false).is_ok());
    }

    #[test]
    fn shared_extra_time_is_optional_and_checked_against_each_stage_duration() {
        let mut setup = valid_setup();
        for extra in [0, 300] {
            setup.match_extra_time_seconds = extra;
            assert!(validate_setup(&setup, &valid_seeds()).is_ok());
        }
        for extra in [-1, i64::MAX] {
            setup.match_extra_time_seconds = extra;
            assert!(validate_setup(&setup, &valid_seeds()).is_err());
        }
        let request: SetupRequest =
            serde_json::from_value(manage_request(7).body.unwrap()).unwrap();
        assert_eq!(request.match_extra_time_seconds, 0);
        let mut stored = serde_json::to_value(valid_setup()).unwrap();
        stored
            .as_object_mut()
            .unwrap()
            .remove("match_extra_time_seconds");
        assert_eq!(
            serde_json::from_value::<Setup>(stored)
                .unwrap()
                .match_extra_time_seconds,
            0
        );
    }

    fn valid_setup() -> Setup {
        Setup {
            seeds: (1..=16).collect(),
            rounds: vec![
                RoundDef {
                    group_a: vec![101, 102, 103],
                    group_b: vec![111, 112, 113],
                    tiebreak: vec![119],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 0,
                },
                RoundDef {
                    group_a: vec![201, 202, 203],
                    group_b: vec![211, 212, 213],
                    tiebreak: vec![219],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 600,
                },
                RoundDef {
                    group_a: vec![301, 302, 303],
                    group_b: vec![311, 312, 313],
                    tiebreak: vec![319],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 600,
                },
                RoundDef {
                    group_a: vec![401, 402, 403],
                    group_b: vec![411, 412, 413],
                    tiebreak: vec![419],
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

    #[test]
    fn rejects_a_problem_reused_across_two_groups() {
        // A problem in two groups would be visible to a player via the opponent
        // rule in one match while still being their own hidden problem in another.
        let mut setup = valid_setup();
        setup.rounds[0].group_b[0] = setup.rounds[0].group_a[0];
        let err = validate_setup(&setup, &valid_seeds()).unwrap_err();
        assert!(err.contains("appears in more than one group"), "got: {err}");
    }

    #[test]
    fn rejects_a_duplicate_player_in_the_seeding() {
        let mut seeds = valid_seeds();
        seeds[5] = seeds[0];
        let err = validate_setup(&valid_setup(), &seeds).unwrap_err();
        assert!(err.contains("distinct"), "got: {err}");
    }

    #[test]
    fn rejects_an_empty_tiebreak_list() {
        // Spec: a scoreless 附加赛 repeats with a NEW problem, so a round with no
        // tiebreak problems can deadlock a level match at the first tie.
        let mut setup = valid_setup();
        setup.rounds[2].tiebreak.clear();
        assert!(validate_setup(&setup, &valid_seeds()).is_err());
    }

    #[test]
    fn accepts_a_fully_valid_setup() {
        // Negative control: without this, an over-strict validator that rejects
        // everything would pass all three tests above.
        assert!(validate_setup(&valid_setup(), &valid_seeds()).is_ok());
    }

    #[test]
    fn rejects_a_setup_that_does_not_have_exactly_four_rounds() {
        let mut setup = valid_setup();
        setup.rounds.pop();
        let err = validate_setup(&setup, &valid_seeds()).unwrap_err();
        assert!(err.contains("4 rounds"), "got: {err}");
    }

    #[test]
    fn rejects_a_group_with_a_duplicate_problem_within_itself() {
        let mut setup = valid_setup();
        setup.rounds[0].group_a[1] = setup.rounds[0].group_a[0];
        assert!(validate_setup(&setup, &valid_seeds()).is_err());
    }

    #[test]
    fn rejects_a_non_positive_xiaoju_duration() {
        let mut setup = valid_setup();
        setup.rounds[0].xiaoju_seconds = 0;
        let err = validate_setup(&setup, &valid_seeds()).unwrap_err();
        assert!(err.contains("xiaoju_seconds"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_judge_wait_and_allows_immediate_escalation() {
        let mut setup = valid_setup();
        for seconds in [-1, i64::MAX] {
            setup.escalation_grace_seconds = seconds;
            assert!(
                validate_setup(&setup, &valid_seeds())
                    .unwrap_err()
                    .contains("escalation_grace_seconds")
            );
        }
        setup.escalation_grace_seconds = 0;
        assert!(validate_setup(&setup, &valid_seeds()).is_ok());
    }

    #[test]
    fn rejects_invalid_stage_durations_and_breaks() {
        for stage in 0..4 {
            for seconds in [0, -1, i64::MAX] {
                let mut setup = valid_setup();
                setup.rounds[stage].tiebreak_seconds = seconds;
                assert!(
                    validate_setup(&setup, &valid_seeds())
                        .unwrap_err()
                        .contains("tiebreak_seconds")
                );
                setup.rounds[stage].tiebreak_seconds = 60;
                setup.rounds[stage].xiaoju_seconds = seconds;
                assert!(
                    validate_setup(&setup, &valid_seeds())
                        .unwrap_err()
                        .contains("xiaoju_seconds")
                );
            }
            for seconds in [-1, i64::MAX] {
                let mut setup = valid_setup();
                setup.rounds[stage].break_before_seconds = seconds;
                assert!(
                    validate_setup(&setup, &valid_seeds())
                        .unwrap_err()
                        .contains("break_before_seconds")
                );
            }
        }
        let mut setup = valid_setup();
        setup.rounds[0].break_before_seconds = 1;
        assert!(validate_setup(&setup, &valid_seeds()).is_err());
        for (i, stage) in setup.rounds.iter_mut().enumerate() {
            stage.xiaoju_seconds = 60 + i as i64 * 30;
            stage.tiebreak_seconds = 120 + i as i64 * 30;
            stage.break_before_seconds = i as i64 * 60;
        }
        assert!(validate_setup(&setup, &valid_seeds()).is_ok());
    }

    #[test]
    fn setup_requires_stage_timings_and_rejects_removed_global_settings() {
        let request = manage_request(7).body.unwrap();
        for field in ["xiaoju_seconds", "tiebreak_seconds", "break_before_seconds"] {
            for stage in 0..4 {
                let mut missing = request.clone();
                missing["rounds"][stage]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(
                    serde_json::from_value::<SetupRequest>(missing)
                        .unwrap_err()
                        .to_string()
                        .contains(field)
                );
            }
        }
        for field in ["xiaoju_seconds", "round_intermission_seconds"] {
            let mut obsolete = request.clone();
            obsolete[field] = serde_json::json!(600);
            assert!(
                serde_json::from_value::<SetupRequest>(obsolete)
                    .unwrap_err()
                    .to_string()
                    .contains("unknown field")
            );
        }
    }

    #[test]
    fn validates_penalties_and_defaults_older_setup_documents_to_twenty_seconds() {
        assert_eq!(Setup::default().penalty_seconds, 20);
        let mut setup = valid_setup();
        setup.penalty_seconds = -1;
        assert!(
            validate_setup(&setup, &valid_seeds())
                .unwrap_err()
                .contains("penalty_seconds")
        );
        setup.penalty_seconds = 0;
        assert!(validate_setup(&setup, &valid_seeds()).is_ok());

        let mut legacy = serde_json::to_value(&setup).unwrap();
        legacy.as_object_mut().unwrap().remove("penalty_seconds");
        assert_eq!(
            serde_json::from_value::<Setup>(legacy)
                .unwrap()
                .penalty_seconds,
            20
        );
    }

    fn manage_request(contest_id: i32) -> PluginHttpRequest {
        let mut params = HashMap::new();
        params.insert("contest_id".to_string(), contest_id.to_string());
        let setup = valid_setup();
        PluginHttpRequest {
            method: "POST".into(),
            path: String::new(),
            params,
            query: HashMap::new(),
            headers: HashMap::new(),
            body: Some(serde_json::json!({
                "rounds": setup.rounds,
                "seeds": valid_seeds(),
            })),
            auth: Some(PluginHttpAuth {
                user_id: 1,
                username: "staff".into(),
                roles: vec![],
                permissions: vec![perm::CONTEST_MANAGE.to_string()],
            }),
        }
    }

    fn queue_bracket_contest_info(host: &Host) {
        queue_bracket_contest_phase(host, "during");
    }

    fn queue_bracket_contest_phase(host: &Host, phase: &str) {
        host.db.queue_query_result(serde_json::json!([{
            "contest_type": "codelink-bracket",
            "is_public": true,
            "is_active": true,
            "phase": phase,
        }]));
    }

    fn queue_stage_open_now(host: &Host) {
        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": 0.0 }]));
        host.db
            .queue_query_result(serde_json::json!([{ "now_ms": 1_000.0 }]));
    }

    #[test]
    fn setup_persists_custom_and_disabled_penalties_and_defaults_omitted_requests() {
        for penalty in [None, Some(7), Some(0)] {
            let host = Host::mock();
            queue_bracket_contest_info(&host);
            queue_stage_open_now(&host);
            let mut req = manage_request(7);
            if let Some(seconds) = penalty {
                req.body.as_mut().unwrap()["penalty_seconds"] = serde_json::json!(seconds);
            }
            assert_eq!(handle_setup(&host, &req).unwrap().status, 200);
            assert_eq!(
                storage::load_setup(&host, 7)
                    .unwrap()
                    .unwrap()
                    .penalty_seconds,
                penalty.unwrap_or(20)
            );
        }
    }

    #[test]
    fn structural_edits_are_rejected_without_writes_once_the_contest_starts() {
        // Existing matches also keep the bracket frozen if staff move the start time.
        for (phase, opened) in [("before", true), ("during", true), ("after", true)] {
            for field in ["group_a", "group_b", "tiebreak", "seeds"] {
                let host = Host::mock();
                queue_bracket_contest_phase(&host, if opened { "during" } else { "before" });
                host.db.queue_query_result(serde_json::json!([{
                    "start_ms": if opened { 0.0 } else { 10_000.0 },
                }]));
                host.db
                    .queue_query_result(serde_json::json!([{ "now_ms": 1_000.0 }]));
                handle_setup(&host, &manage_request(7)).unwrap();
                let original = host.storage.data();
                let mut request = manage_request(7);
                let body = request.body.as_mut().unwrap();
                if field == "seeds" {
                    body[field].as_array_mut().unwrap().swap(0, 1);
                } else {
                    // Swapping across stages also verifies that future stages
                    // cannot replace problems required by existing matches.
                    let first = body["rounds"][0][field].clone();
                    body["rounds"][0][field] = body["rounds"][3][field].clone();
                    body["rounds"][3][field] = first;
                }
                body["rounds"][0]["xiaoju_seconds"] = serde_json::json!(600);
                queue_bracket_contest_phase(&host, phase);
                let response = handle_setup(&host, &request).unwrap_err().into_response();
                assert_eq!(response.status, 409, "{phase}/{field}");
                assert_eq!(host.storage.data(), original, "{phase}/{field}");
            }
        }
    }

    #[test]
    fn assignments_and_seeds_can_be_corrected_before_the_contest_opens() {
        let host = Host::mock();
        let mut request = manage_request(7);
        for update in [false, true] {
            if update {
                let body = request.body.as_mut().unwrap();
                body["seeds"].as_array_mut().unwrap().swap(0, 1);
                body["rounds"].as_array_mut().unwrap().swap(1, 2);
            }
            queue_bracket_contest_phase(&host, "before");
            host.db
                .queue_query_result(serde_json::json!([{ "start_ms": 10_000.0 }]));
            host.db
                .queue_query_result(serde_json::json!([{ "now_ms": 1_000.0 }]));
            assert_eq!(handle_setup(&host, &request).unwrap().status, 200);
        }
        let setup = storage::load_setup(&host, 7).unwrap().unwrap();
        assert_eq!(setup.rounds[1].group_a, valid_setup().rounds[2].group_a);
        assert_eq!(setup.seeds[0], 2);
        assert!(storage::load_all_matches(&host, 7).unwrap().is_empty());
    }

    #[test]
    fn handle_setup_accepts_an_unchanged_ordering_bracket() {
        // Round 1's matches exist after the first call, but none has left
        // Ordering. Saving timing settings with the same assignments and
        // seeds must still succeed.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        queue_stage_open_now(&host);
        queue_bracket_contest_info(&host);
        queue_stage_open_now(&host);

        let first = handle_setup(&host, &manage_request(7)).unwrap();
        assert_eq!(first.status, 200);

        let second = handle_setup(&host, &manage_request(7)).unwrap();
        assert_eq!(
            second.status, 200,
            "a second /setup call on a bracket nobody has started playing must succeed"
        );
    }

    #[test]
    fn handle_setup_creates_all_eight_round_one_matches() {
        // Round 1's slots are seeded directly from `/setup`'s player order
        // (see `handle_setup`'s doc comment), but a MATCH document only
        // exists once `create_match_if_both_slots_filled` has run for it --
        // without this, round 1 would have 16 filled slots and zero actual
        // matches for `judge::advance`/`ordering::handle_order` to act on.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        queue_stage_open_now(&host);

        let resp = handle_setup(&host, &manage_request(7)).unwrap();
        assert_eq!(resp.status, 200);

        for pos in 0..storage::matches_in_round(16, 1) {
            let match_id = storage::match_id_for(16, 1, pos);
            assert!(
                storage::load_match(&host, 7, match_id).unwrap().is_some(),
                "round 1 match at pos {pos} (id {match_id}) should have been created"
            );
        }
    }
}
