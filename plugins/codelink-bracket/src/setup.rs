//! `/setup` -- round definitions and manual seeding.
//!
//! Staff submit the whole bracket's problem assignment (4 rounds x 2 groups
//! of 3, plus a per-round 附加赛 list) and the 16-player seeding in one call.
//! Validation rejects rather than warns: an invalid setup left half-applied
//! would leave the bracket in a state nothing else in this plugin can
//! recover from.

use std::collections::HashMap;

use broccoli_server_sdk::permissions as perm;
use broccoli_server_sdk::prelude::*;
use serde::Deserialize;

use crate::model::{RoundDef, Setup};
use crate::storage;

/// Validate a full bracket setup before it is persisted. Every failure here
/// is a rejection, not a warning -- see the module doc comment for why.
pub fn validate_setup(setup: &Setup, seeds: &[i32; 16]) -> Result<(), String> {
    let mut seed_set: Vec<i32> = seeds.to_vec();
    seed_set.sort_unstable();
    seed_set.dedup();
    if seed_set.len() != seeds.len() {
        return Err("seeds must contain 16 distinct player ids".to_string());
    }

    if setup.rounds.len() != 4 {
        return Err(format!(
            "setup must define exactly 4 rounds, got {}",
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
                "round {round_idx}'s group_a must contain 3 distinct problem ids"
            ));
        }

        let mut group_b_set: Vec<i32> = round.group_b.to_vec();
        group_b_set.sort_unstable();
        group_b_set.dedup();
        if group_b_set.len() != round.group_b.len() {
            return Err(format!(
                "round {round_idx}'s group_b must contain 3 distinct problem ids"
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
    seeds: [i32; 16],
}

/// Handle `POST /contests/{contest_id}/setup`: validate, then persist the
/// round definitions and seed round 1's bracket slots. Organizer-only
/// (`contest:manage`). Kept off the WASM ABI (the `api_setup` wrapper in
/// `lib.rs` adapts this) so it unit-tests on the host, matching the
/// `plugins/print/src/handlers.rs` convention.
///
/// Nothing is written unless `validate_setup` accepts the whole document --
/// see the module doc comment for why a half-applied setup is unrecoverable.
///
/// Staff may correct timing before or during a contest. Regular and tiebreak
/// durations and shared extra time are pinned when a match opens its first game,
/// so a correction affects only matches that have not started. Problem assignments
/// and seeds are fixed once the contest starts or any match has been created.
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

    let setup = Setup {
        rounds: body.rounds,
        match_extra_time_seconds: body.match_extra_time_seconds,
        penalty_seconds: body.penalty_seconds,
        escalation_grace_seconds: body.escalation_grace_seconds,
    };
    validate_setup(&setup, &body.seeds).map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let matches = storage::load_all_matches(host, contest_id)?;
    if (info.phase != "before" || !matches.is_empty())
        && let Some(previous) = storage::load_setup(host, contest_id)?
    {
        if previous.rounds.len() != setup.rounds.len()
            || previous.rounds.iter().zip(&setup.rounds).any(|(old, new)| {
                old.group_a != new.group_a
                    || old.group_b != new.group_b
                    || old.tiebreak != new.tiebreak
            })
        {
            return Err(PluginHttpResponse::error(
                409,
                "Problem assignments are fixed once the contest starts",
            )
            .into());
        }
        let keys: Vec<_> = (0..16)
            .map(|pos| storage::slot_key(contest_id, 1, pos))
            .collect();
        let slots = host
            .storage
            .get(&keys.iter().map(String::as_str).collect::<Vec<_>>())?;
        if keys.iter().zip(body.seeds).any(|(key, seed)| {
            slots.get(key).and_then(|value| value.parse::<i32>().ok()) != Some(seed)
        }) {
            return Err(PluginHttpResponse::error(
                409,
                "Player seeding is fixed once the contest starts",
            )
            .into());
        }
    }

    let setup_json = serde_json::to_string(&setup)?;
    // Round 1's slots are seeded directly from the submitted order; later
    // rounds' slots are filled by match results, not by `/setup`. Persist setup
    // and seeds together so the stage opener cannot read a partial update.
    let mut entries: Vec<(String, String)> = body
        .seeds
        .iter()
        .enumerate()
        .map(|(pos, player_id)| {
            (
                storage::slot_key(contest_id, 1, pos as u8),
                player_id.to_string(),
            )
        })
        .collect();
    entries.push((storage::setup_key(contest_id), setup_json));
    let entries_ref: Vec<(&str, &str)> = entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    host.storage.set(&entries_ref)?;

    // Round one is opened by the same stage barrier as every later round:
    // immediately if the contest is already running, or by a timer at the
    // contest start. This keeps the ordering phase inside the official stage
    // window rather than exposing it during setup.
    crate::judge::try_open_stage(host, contest_id, 1)?;

    Ok(PluginHttpResponse {
        status: 200,
        headers: None,
        body: Some(serde_json::json!({ "ok": true })),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_seeds() -> [i32; 16] {
        std::array::from_fn(|i| (i as i32) + 1)
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
            rounds: vec![
                RoundDef {
                    group_a: [101, 102, 103],
                    group_b: [111, 112, 113],
                    tiebreak: vec![119],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 0,
                },
                RoundDef {
                    group_a: [201, 202, 203],
                    group_b: [211, 212, 213],
                    tiebreak: vec![219],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 600,
                },
                RoundDef {
                    group_a: [301, 302, 303],
                    group_b: [311, 312, 313],
                    tiebreak: vec![319],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 600,
                },
                RoundDef {
                    group_a: [401, 402, 403],
                    group_b: [411, 412, 413],
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
        for (phase, opened) in [
            ("before", true),
            ("during", false),
            ("during", true),
            ("after", false),
            ("after", true),
        ] {
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
        assert_eq!(
            host.storage.get_one(&storage::slot_key(7, 1, 0)).unwrap(),
            Some("2".into())
        );
        assert!(storage::load_all_matches(&host, 7).unwrap().is_empty());
    }

    #[test]
    fn handle_setup_accepts_timing_edits_for_an_ordering_bracket() {
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

        for pos in 0..storage::matches_in_round(1) {
            let match_id = storage::match_id_for(1, pos);
            assert!(
                storage::load_match(&host, 7, match_id).unwrap().is_some(),
                "round 1 match at pos {pos} (id {match_id}) should have been created"
            );
        }
    }
}
