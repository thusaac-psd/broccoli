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

    if setup.xiaoju_seconds <= 0 {
        return Err("xiaoju_seconds must be greater than 0".to_string());
    }

    // A problem belongs to exactly one group of one round. Tracked globally
    // (not per-round) because the visibility kernel's opponent-ranking
    // branch would otherwise grant a player `Allow` on a problem that is
    // still their own hidden problem in a DIFFERENT match -- see the module
    // doc comment on this file's caller in `docs/superpowers/plans/
    // 2026-09-19-afternoon-bracket.md` (Task 3) for the full reasoning.
    let mut seen_problems: HashMap<i32, ()> = HashMap::new();
    for (round_idx, round) in setup.rounds.iter().enumerate() {
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
struct SetupRequest {
    rounds: Vec<RoundDef>,
    xiaoju_seconds: i64,
    round_intermission_seconds: i64,
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
/// # A second `/setup` call is accepted, not rejected
///
/// This handler has no guard against being called again after matches
/// already exist or are in progress: it always unconditionally overwrites
/// the shared `Setup` document (`xiaoju_seconds`, `round_intermission_seconds`,
/// `escalation_grace_seconds`, every round's tiebreak list) and re-seeds
/// round 1's slots. This is a deliberate choice, not an oversight, for two
/// reasons:
///
/// 1. Staff need a way to correct a fat-fingered bracket (wrong problem id,
///    wrong seed order, wrong timing) before or even after play has begun,
///    without restarting the whole contest. Rejecting outright would leave
///    no recovery path.
/// 2. A pacing value (`xiaoju_seconds`) rewritten here can no longer
///    silently reconfigure an ALREADY-RUNNING match: `judge::open_xiaoju`
///    snapshots the live `setup.xiaoju_seconds` into that match's own
///    `MatchState::xiaoju_seconds` the moment its first 小局 actually opens,
///    and uses that pinned per-match value for every subsequent 小局,
///    ignoring later changes to the shared `Setup` document. A match that
///    has NOT opened its first 小局 yet (still `Pending`/`Ordering`) has
///    nothing pinned yet, so it correctly picks up a corrected value. See
///    `MatchState::xiaoju_seconds`'s doc comment and the QA regression test
///    `defect_setup_called_twice_silently_reconfigures_an_in_progress_matchs_timing`.
///
/// This pinning is deliberately scoped to `xiaoju_seconds` only, the one
/// field the QA suite concretely exercises. `escalation_grace_seconds` and
/// each round's `tiebreak` list are still read LIVE from the shared `Setup`
/// document by `judge.rs` on every call and share the same theoretical
/// exposure to a corrective second `/setup` call reaching a match already
/// past the point that value started mattering (e.g. a re-ordered tiebreak
/// list reshuffling which problem an in-progress 附加赛 advances to next).
/// That is a known, out-of-scope residual, not a defect fixed here -- see
/// the QA sweep report.
///
/// Round 1's MATCH documents themselves are unaffected by a second call in
/// a different way: `storage::create_match_if_both_slots_filled` uses a
/// create-only compare-and-set, so re-seeding slots for a round that
/// already has matches created is a no-op for those matches.
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
        xiaoju_seconds: body.xiaoju_seconds,
        round_intermission_seconds: body.round_intermission_seconds,
        escalation_grace_seconds: body.escalation_grace_seconds,
    };
    validate_setup(&setup, &body.seeds).map_err(|msg| PluginHttpResponse::error(400, msg))?;

    let setup_json = serde_json::to_string(&setup)?;
    host.storage
        .set(&[(storage::setup_key(contest_id).as_str(), setup_json.as_str())])?;

    // Round 1's slots are seeded directly from the submitted order; later
    // rounds' slots are filled by match results, not by `/setup`.
    let seed_entries: Vec<(String, String)> = body
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
    let seed_entries_ref: Vec<(&str, &str)> = seed_entries
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    host.storage.set(&seed_entries_ref)?;

    // Every round-1 match's BOTH slots are filled at once by the seeding
    // above (unlike later rounds, whose slots trickle in one winner at a
    // time from `judge::write_next_round_slot`) -- so unlike that path,
    // round 1's matches must be created explicitly here rather than
    // relying on a slot write to trigger it.
    for pos in 0..storage::matches_in_round(1) {
        storage::create_match_if_both_slots_filled(host, contest_id, &setup, 1, pos)?;
    }

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

    fn valid_setup() -> Setup {
        Setup {
            rounds: vec![
                RoundDef {
                    group_a: [101, 102, 103],
                    group_b: [111, 112, 113],
                    tiebreak: vec![119],
                },
                RoundDef {
                    group_a: [201, 202, 203],
                    group_b: [211, 212, 213],
                    tiebreak: vec![219],
                },
                RoundDef {
                    group_a: [301, 302, 303],
                    group_b: [311, 312, 313],
                    tiebreak: vec![319],
                },
                RoundDef {
                    group_a: [401, 402, 403],
                    group_b: [411, 412, 413],
                    tiebreak: vec![419],
                },
            ],
            xiaoju_seconds: 1_800,
            round_intermission_seconds: 600,
            escalation_grace_seconds: 120,
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
        setup.xiaoju_seconds = 0;
        let err = validate_setup(&setup, &valid_seeds()).unwrap_err();
        assert!(err.contains("xiaoju_seconds"), "got: {err}");
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
                "xiaoju_seconds": setup.xiaoju_seconds,
                "round_intermission_seconds": setup.round_intermission_seconds,
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
        host.db.queue_query_result(serde_json::json!([{
            "contest_type": "codelink-bracket",
            "is_public": true,
            "is_active": true,
            "phase": "during",
        }]));
    }

    #[test]
    fn handle_setup_still_accepts_a_second_call_correcting_an_unstarted_bracket() {
        // Documents the "accept, don't reject" rule from `handle_setup`'s doc
        // comment: staff must be able to fix a fat-fingered bracket that
        // nobody has started playing yet. Round 1's matches exist after the
        // first call (create-only CAS), but none has left `Ordering`, so a
        // second call correcting e.g. the timing must still succeed.
        let host = Host::mock();
        queue_bracket_contest_info(&host);
        queue_bracket_contest_info(&host);

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
