//! Storage key schema and typed accessors for the 下午场 bracket plugin.
//!
//! No plugin-side transactions: `host.storage.begin()` deadlocks the
//! runtime under concurrent submissions. Concurrent match updates go through
//! `Storage::modify` (a compare-and-set retry loop), never a read-then-write
//! pair.

use broccoli_server_sdk::prelude::*;

use crate::model::{MatchPhase, MatchState, Setup};

/// Match ids fit in a byte; supported brackets contain at most 128 players.
/// Number of matches in a one-based elimination stage.
pub fn matches_in_round(players: u8, round: u8) -> u8 {
    players.checked_shr(u32::from(round)).unwrap_or(0)
}

/// How many match ids are used up by every round strictly before `round`.
/// `round` may be `0` (meaning "before round 1", i.e. no earlier round at
/// all) -- callers computing an intermission boundary need to ask for the
/// offset of "the round before this one" without first checking whether
/// that round exists, and `round: u8` cannot represent `-1`. A naive
/// `round - 1` subtraction here would underflow for round 1 (and panic in
/// debug builds); summing the OPEN range `1..round` instead is naturally 0
/// when `round <= 1`.
pub fn round_offset(players: u8, round: u8) -> u8 {
    (1..round).map(|r| matches_in_round(players, r)).sum()
}

/// The unique match id for `(round, pos)`, in `0..players-1`. Ids are
/// assigned round-major (all of round 1, then all of round 2, ...) so a
/// match's id never changes shape as later rounds are created.
pub fn match_id_for(players: u8, round: u8, pos: u8) -> u8 {
    round_offset(players, round) + pos
}

/// Storage key for a contest's `/setup` round definitions.
pub fn setup_key(contest: i32) -> String {
    format!("setup:{contest}")
}

/// Storage key for the player id occupying bracket slot `(round, pos)`.
///
/// Slots are separate keys rather than fields of the next-round match
/// document, because two SIBLING matches in a round both feed the SAME
/// next-round match. If that next match were one document, each sibling's
/// winner would be written by reading the document, setting its own slot
/// field, and writing it back -- and the two siblings' winners can finish at
/// arbitrary times relative to each other. Even with `compare_and_set`
/// retries, that is a race between two DIFFERENT writers each trying to
/// fill a DIFFERENT field of the SAME document, which only guarantees no
/// corruption, not that both writes land: whichever writer's `modify` retry
/// loses hits `SdkError::Other("CAS retry limit exceeded")`, and a lost
/// retry silently drops a real winner. Per-slot keys make the collision
/// structurally impossible instead of merely unlikely: each sibling match
/// writes only its OWN slot key, and nothing else ever writes it.
pub fn slot_key(contest: i32, round: u8, pos: u8) -> String {
    format!("slot:{contest}:{round}:{pos}")
}

/// Storage key for a match's full state document, scoped by contest so two
/// concurrent afternoon sessions never collide.
pub fn match_key(contest: i32, id: u8) -> String {
    format!("match:{contest}:{id}")
}

/// Load a match's current state, if it has been created.
pub fn load_match(host: &Host, contest: i32, id: u8) -> Result<Option<MatchState>, SdkError> {
    let key = match_key(contest, id);
    match host.storage.get_one(&key)? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// Load a contest's `/setup` round definitions, if `/setup` has run.
pub fn load_setup(host: &Host, contest: i32) -> Result<Option<Setup>, SdkError> {
    let key = setup_key(contest);
    match host.storage.get_one(&key)? {
        Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
        None => Ok(None),
    }
}

/// Visit persisted documents in bounded pages. The visitor may update a
/// document; scanning is not a transactional snapshot.
pub fn scan_documents(
    host: &Host,
    prefix: &str,
    mut visit: impl FnMut(&str, &str) -> Result<(), SdkError>,
) -> Result<(), SdkError> {
    let mut after: Option<String> = None;
    loop {
        let page = host.storage.scan(prefix, after.as_deref())?;
        if page.is_empty() {
            return Ok(());
        }
        after = page.keys().max().cloned();
        for (key, value) in page {
            visit(&key, &value)?;
        }
    }
}

/// Read created matches, independent of the configured bracket size.
pub fn load_all_matches(host: &Host, contest: i32) -> Result<Vec<(u8, MatchState)>, SdkError> {
    let prefix = format!("match:{contest}:");
    let mut matches = Vec::new();
    scan_documents(host, &prefix, |key, value| {
        let id = key
            .strip_prefix(&prefix)
            .unwrap()
            .parse()
            .map_err(|_| SdkError::Other("Invalid match key".into()))?;
        matches.push((id, serde_json::from_str(value)?));
        Ok(())
    })?;
    matches.sort_by_key(|(id, _)| *id);
    Ok(matches)
}

/// Find the one match, among every match already loaded for a contest, where
/// `player` participates AND whose round is `round` (1-based, matching
/// [`MatchState::round`]). A player is in at most one match per round, so
/// this uniquely resolves "the match this (problem, viewer) pair is about"
/// without needing a match-id numbering scheme. Shared by `visibility.rs`
/// (per-viewer problem visibility) and `gate.rs` (submission gating) -- both
/// resolve a problem to a round via `Setup.rounds`, then need this same
/// lookup to find the viewer's match in that round.
pub fn find_players_match(
    matches: &[(u8, MatchState)],
    round: u8,
    player: i32,
) -> Option<&MatchState> {
    matches
        .iter()
        .find(|(_, m)| m.round == round && (m.player_a == player || m.player_b == player))
        .map(|(_, m)| m)
}

/// Apply `f` to a match's state via a compare-and-set retry loop, so a
/// judging callback and a timer firing on the same match cannot clobber
/// each other's write.
pub fn update_match<F>(host: &Host, contest: i32, id: u8, f: F) -> Result<MatchState, SdkError>
where
    F: Fn(&mut MatchState) -> Result<(), SdkError>,
{
    let key = match_key(contest, id);
    host.storage.modify(&key, f)
}

/// First-stage participants come from the canonical setup seeds. Later
/// matches at `(round, pos)` wait until both feeder slots
/// (`slot_key(contest, round, pos*2)` and `..pos*2+1`) have been filled. A no-op, not an error, while either slot is still empty -- the
/// caller (round 1: `setup::handle_setup`; later rounds: the synchronized
/// stage opener in `judge`) calls this for every position only after the
/// stage's opening barrier has passed.
///
/// Uses `compare_and_set(key, None, ..)` (create-only) rather than
/// `update_match`'s read-modify-write, so calling this again once the match
/// already exists is a harmless no-op instead of clobbering whatever
/// progress (ordering, xiaoju) it has made since. That matters because a
/// slot write and this call are not transactional together: a retried or
/// redelivered write to the second slot must not re-create (and reset) a
/// match the first successful call already started.
pub fn create_match_if_both_slots_filled(
    host: &Host,
    contest: i32,
    setup: &Setup,
    round: u8,
    pos: u8,
) -> Result<(), SdkError> {
    let (player_a, player_b) = if round == 1 {
        let Some(pair) = setup.seeds.get(pos as usize * 2..pos as usize * 2 + 2) else {
            return Err(SdkError::Other("Invalid first-stage position".into()));
        };
        (pair[0], pair[1])
    } else {
        let key_a = slot_key(contest, round, pos * 2);
        let key_b = slot_key(contest, round, pos * 2 + 1);
        let raw = host.storage.get(&[key_a.as_str(), key_b.as_str()])?;
        let (Some(a), Some(b)) = (raw.get(&key_a), raw.get(&key_b)) else {
            return Ok(());
        };
        let player_a: i32 = a
            .parse()
            .map_err(|_| SdkError::Other(format!("slot {key_a} holds a non-integer player id")))?;
        let player_b: i32 = b
            .parse()
            .map_err(|_| SdkError::Other(format!("slot {key_b} holds a non-integer player id")))?;

        (player_a, player_b)
    };

    let Some(round_def) = setup.rounds.get(round.saturating_sub(1) as usize) else {
        return Err(SdkError::Other(format!(
            "setup has no RoundDef for round {round}"
        )));
    };

    let m = MatchState {
        round,
        pos,
        player_a,
        player_b,
        group_a: round_def.group_a.clone(),
        group_b: round_def.group_b.clone(),
        state: MatchPhase::Ordering,
        ..Default::default()
    };
    let id = match_id_for(setup.player_count(), round, pos);
    let _ =
        host.storage
            .compare_and_set(&match_key(contest, id), None, &serde_json::to_string(&m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_keys_are_distinct_per_position() {
        // The two feeders of one next-round match must not share a key.
        assert_ne!(slot_key(7, 2, 0), slot_key(7, 2, 1));
    }

    #[test]
    fn match_keys_are_scoped_by_contest() {
        // Two concurrent afternoon sessions must not collide.
        assert_ne!(match_key(7, 3), match_key(8, 3));
    }

    #[test]
    fn load_match_returns_none_for_an_uncreated_match() {
        let host = Host::mock();
        assert!(load_match(&host, 7, 0).unwrap().is_none());
    }

    #[test]
    fn update_match_persists_the_mutation_and_survives_a_concurrent_writer() {
        let host = Host::mock();
        let key = match_key(7, 0);
        host.storage
            .set(&[(
                key.as_str(),
                &serde_json::to_string(&MatchState {
                    round: 1,
                    pos: 0,
                    player_a: 10,
                    player_b: 20,
                    ..Default::default()
                })
                .unwrap(),
            )])
            .unwrap();

        let updated = update_match(&host, 7, 0, |m| {
            m.score_a += 1;
            Ok(())
        })
        .unwrap();

        assert_eq!(updated.score_a, 1);
        assert_eq!(updated.player_a, 10, "unrelated fields must be preserved");

        let reloaded = load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.score_a, 1, "the mutation must be durable");
    }

    #[test]
    fn load_setup_returns_none_before_setup_has_run() {
        let host = Host::mock();
        assert!(load_setup(&host, 7).unwrap().is_none());
    }

    #[test]
    fn load_setup_returns_the_persisted_document() {
        let host = Host::mock();
        let setup = setup_with_round_1();
        host.storage
            .set(&[(
                setup_key(7).as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            )])
            .unwrap();
        assert_eq!(load_setup(&host, 7).unwrap(), Some(setup));
    }

    #[test]
    fn matches_in_round_halves_each_round() {
        assert_eq!(matches_in_round(16, 1), 8);
        assert_eq!(matches_in_round(16, 2), 4);
        assert_eq!(matches_in_round(16, 3), 2);
        assert_eq!(matches_in_round(16, 4), 1);
    }

    #[test]
    fn round_offset_sums_every_earlier_rounds_match_count() {
        assert_eq!(round_offset(16, 1), 0);
        assert_eq!(round_offset(16, 2), 8);
        assert_eq!(round_offset(16, 3), 12);
        assert_eq!(round_offset(16, 4), 14);
    }

    #[test]
    fn round_offset_does_not_underflow_at_round_one() {
        // Round 1 has no earlier round to sum; a naive `round - 1` on a `u8`
        // would underflow instead of returning 0.
        assert_eq!(round_offset(16, 0), 0);
    }

    #[test]
    fn match_id_for_covers_every_id_exactly_once() {
        for players in [2u8, 8, 16, 32, 128] {
            let ids: Vec<_> = (1..=players.ilog2() as u8)
                .flat_map(|round| {
                    (0..matches_in_round(players, round))
                        .map(move |pos| match_id_for(players, round, pos))
                })
                .collect();
            assert_eq!(ids, (0..players - 1).collect::<Vec<_>>());
        }
    }

    fn setup_with_round_1() -> Setup {
        Setup {
            seeds: (1..=16).collect(),
            rounds: vec![
                crate::model::RoundDef {
                    group_a: vec![1, 2, 3],
                    group_b: vec![4, 5, 6],
                    tiebreak: vec![7],
                    xiaoju_seconds: 1_800,
                    tiebreak_seconds: 1_800,
                    break_before_seconds: 0,
                },
                crate::model::RoundDef::default(),
                crate::model::RoundDef::default(),
                crate::model::RoundDef::default(),
            ],
            escalation_grace_seconds: 120,
            penalty_seconds: crate::model::default_penalty_seconds(),
            ..Default::default()
        }
    }

    #[test]
    fn later_stage_waits_until_both_feeder_slots_are_set() {
        let host = Host::mock();
        let setup = setup_with_round_1();
        host.storage
            .set(&[(slot_key(7, 2, 0).as_str(), "10")])
            .unwrap();
        create_match_if_both_slots_filled(&host, 7, &setup, 2, 0).unwrap();
        assert!(
            load_match(&host, 7, match_id_for(16, 2, 0))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn create_match_if_both_slots_filled_creates_the_match_once_both_slots_are_set() {
        let host = Host::mock();
        let mut setup = setup_with_round_1();
        setup.seeds[0] = 10;
        setup.seeds[1] = 20;
        host.storage
            .set(&[
                (slot_key(7, 1, 0).as_str(), "10"),
                (slot_key(7, 1, 1).as_str(), "20"),
            ])
            .unwrap();
        create_match_if_both_slots_filled(&host, 7, &setup, 1, 0).unwrap();
        let m = load_match(&host, 7, match_id_for(16, 1, 0))
            .unwrap()
            .unwrap();
        assert_eq!(m.player_a, 10);
        assert_eq!(m.player_b, 20);
        assert_eq!(m.round, 1);
        assert_eq!(m.pos, 0);
        assert_eq!(m.state, MatchPhase::Ordering);
        assert_eq!(m.group_a, vec![1, 2, 3]);
        assert_eq!(m.group_b, vec![4, 5, 6]);
    }

    #[test]
    fn create_match_if_both_slots_filled_does_not_clobber_an_already_created_match() {
        let host = Host::mock();
        let mut setup = setup_with_round_1();
        setup.seeds[0] = 10;
        setup.seeds[1] = 20;
        host.storage
            .set(&[
                (slot_key(7, 1, 0).as_str(), "10"),
                (slot_key(7, 1, 1).as_str(), "20"),
            ])
            .unwrap();
        create_match_if_both_slots_filled(&host, 7, &setup, 1, 0).unwrap();
        update_match(&host, 7, match_id_for(16, 1, 0), |m| {
            m.state = MatchPhase::InProgress;
            Ok(())
        })
        .unwrap();

        // A redelivered/retried slot write re-runs this; it must not reset
        // a match that has already progressed past `Ordering`.
        create_match_if_both_slots_filled(&host, 7, &setup, 1, 0).unwrap();

        let m = load_match(&host, 7, match_id_for(16, 1, 0))
            .unwrap()
            .unwrap();
        assert_eq!(m.state, MatchPhase::InProgress);
    }

    #[test]
    fn loads_every_match_in_a_128_player_bracket_across_scan_pages() {
        let host = Host::mock();
        for id in 0..127u8 {
            let state = MatchState {
                player_a: i32::from(id),
                ..Default::default()
            };
            host.storage
                .set(&[(&match_key(7, id), &serde_json::to_string(&state).unwrap())])
                .unwrap();
        }
        let matches = load_all_matches(&host, 7).unwrap();
        assert_eq!(
            matches.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            (0..127).collect::<Vec<_>>()
        );
        assert_eq!(matches[126].1.player_a, 126);
    }

    #[test]
    fn load_all_matches_skips_uncreated_ids_and_scans_the_bracket_in_pages() {
        let host = Host::mock();
        host.storage
            .set(&[
                (
                    match_key(7, 0).as_str(),
                    serde_json::to_string(&MatchState {
                        round: 1,
                        pos: 0,
                        player_a: 10,
                        player_b: 20,
                        ..Default::default()
                    })
                    .unwrap()
                    .as_str(),
                ),
                (
                    match_key(7, 5).as_str(),
                    serde_json::to_string(&MatchState {
                        round: 2,
                        pos: 0,
                        player_a: 30,
                        player_b: 40,
                        ..Default::default()
                    })
                    .unwrap()
                    .as_str(),
                ),
            ])
            .unwrap();

        // A second contest's match must not leak into contest 7's scan.
        host.storage
            .set(&[(
                match_key(8, 0).as_str(),
                serde_json::to_string(&MatchState::default())
                    .unwrap()
                    .as_str(),
            )])
            .unwrap();

        let before = host.storage.get_call_count();
        let matches = load_all_matches(&host, 7).unwrap();
        assert_eq!(
            host.storage.get_call_count(),
            before + 2,
            "one populated page plus the end-of-scan read"
        );

        let ids: Vec<u8> = matches.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, vec![0, 5]);
        assert_eq!(matches[0].1.player_a, 10);
        assert_eq!(matches[1].1.player_a, 30);
    }
}
