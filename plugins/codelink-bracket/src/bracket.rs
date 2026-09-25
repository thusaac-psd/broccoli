//! Round-level pacing: the fixed inter-round intermission, and the pure
//! start-time gate a match must pass before it can leave `Ordering`.
//!
//! "Intermission | Fixed, between rounds only" (spec) is read here as a
//! ROUND-WIDE synchronized break, not a per-match staggered one: everyone in
//! the cohort rests together before the next round starts for everyone,
//! rather than each bracket path racing ahead independently. That is why
//! [`round_ended_at_ms`] requires EVERY match of the previous round to be
//! `Decided` (not just the two direct feeders of one next-round match).

use crate::model::{MatchPhase, MatchState, Setup};
use crate::storage;

/// The earliest instant (Unix epoch ms) at which round `round` may open,
/// given the round directly before it ended at `previous_round_ended_ms`.
///
/// Round 1 has no round before it, so it opens immediately -- bounded only
/// by `/setup` having run and both slots being filled, never by an
/// intermission. `round` is `u8`, so computing `round - 1` unguarded would
/// underflow at `round == 1`; this short-circuits before ever subtracting.
pub fn round_opens_at_ms(setup: &Setup, round: u8, previous_round_ended_ms: i64) -> i64 {
    if round <= 1 {
        return 0;
    }
    previous_round_ended_ms + setup.round_intermission_seconds * 1_000
}

/// When every match of `round` has reached `MatchPhase::Decided`, the
/// instant the LAST of them did -- the round-wide synchronized point the
/// next round's intermission counts from. `None` while any match in the
/// round has not been created yet, is still running, or is stuck in
/// `NeedsAdjudication` (staff must resolve it via `/force-decide` first,
/// which is the only path that sets `decided_at_ms` for a match that was
/// ever `NeedsAdjudication` -- see `MatchState::decided_at_ms`).
pub fn round_ended_at_ms(matches: &[(u8, MatchState)], round: u8) -> Option<i64> {
    let in_round: Vec<&MatchState> = matches
        .iter()
        .filter(|(_, m)| m.round == round)
        .map(|(_, m)| m)
        .collect();

    if in_round.len() != storage::matches_in_round(round) as usize {
        return None;
    }
    if !in_round.iter().all(|m| m.state == MatchPhase::Decided) {
        return None;
    }
    in_round.iter().map(|m| m.decided_at_ms).max()
}

/// Move a match out of `Ordering` into `InProgress`, but only once its
/// round has opened. Deliberately does NOT check `order_a`/`order_b` --
/// that precondition belongs to the caller (`routes::start_match`); this is
/// purely the timing + phase gate, shared by the real `/start` route
/// handler and exercised directly below.
pub fn start_match_at(m: &mut MatchState, now: i64, opens: i64) -> Result<(), &'static str> {
    if m.state != MatchPhase::Ordering {
        return Err("match is not awaiting start");
    }
    if now < opens {
        return Err("the previous round's intermission has not elapsed yet");
    }
    m.state = MatchPhase::InProgress;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_with_intermission(seconds: i64) -> Setup {
        Setup {
            round_intermission_seconds: seconds,
            ..Default::default()
        }
    }

    fn match_in_ordering_at_round(round: u8, player_a: i32, player_b: i32) -> MatchState {
        MatchState {
            round,
            pos: 0,
            player_a,
            player_b,
            state: MatchPhase::Ordering,
            ..Default::default()
        }
    }

    fn both_orders_in(m: &mut MatchState) {
        m.order_a = Some([101, 102, 103]);
        m.order_b = Some([201, 202, 203]);
    }

    #[test]
    fn round_two_cannot_open_before_the_intermission_has_elapsed() {
        // `round_intermission_seconds` is genuinely SECONDS (see model.rs);
        // 600s = 10 minutes = 600_000ms.
        let setup = setup_with_intermission(600);
        assert_eq!(round_opens_at_ms(&setup, 2, 1_000_000), 1_600_000);
    }

    #[test]
    fn round_one_has_no_intermission_before_it() {
        let setup = setup_with_intermission(600);
        assert_eq!(round_opens_at_ms(&setup, 1, 0), 0);
    }

    #[test]
    fn round_one_ignores_previous_round_ended_ms_entirely() {
        // A naive unguarded `round - 1` implementation would add the
        // intermission to whatever `previous_round_ended_ms` is, instead of
        // short-circuiting; round 1 must return 0 no matter what is passed.
        let setup = setup_with_intermission(600);
        assert_eq!(round_opens_at_ms(&setup, 1, 999_999_999), 0);
    }

    #[test]
    fn starting_a_match_before_its_round_opens_is_rejected() {
        let mut m = match_in_ordering_at_round(2, 10, 20);
        both_orders_in(&mut m);
        assert!(start_match_at(&mut m, 1_200_000, 1_600_000).is_err());
        assert_eq!(
            m.state,
            MatchPhase::Ordering,
            "a rejected start must not mutate state"
        );
    }

    #[test]
    fn starting_a_match_once_its_round_has_opened_succeeds() {
        let mut m = match_in_ordering_at_round(2, 10, 20);
        both_orders_in(&mut m);
        assert!(start_match_at(&mut m, 1_600_000, 1_600_000).is_ok());
        assert_eq!(m.state, MatchPhase::InProgress);
    }

    #[test]
    fn starting_a_match_that_is_not_awaiting_start_is_rejected() {
        let mut m = match_in_ordering_at_round(1, 10, 20);
        m.state = MatchPhase::InProgress;
        assert!(start_match_at(&mut m, 0, 0).is_err());
    }

    #[test]
    fn round_ended_at_ms_is_none_until_every_match_in_the_round_is_decided() {
        let mut a = match_in_ordering_at_round(1, 10, 20);
        a.state = MatchPhase::Decided;
        a.decided_at_ms = 500;
        let b = match_in_ordering_at_round(1, 30, 40); // still Ordering
        let matches = vec![(0u8, a), (1u8, b)];
        // Only 2 of round 1's 8 matches are present here.
        assert_eq!(round_ended_at_ms(&matches, 1), None);
    }

    #[test]
    fn round_ended_at_ms_is_the_max_decided_at_ms_once_the_whole_round_is_decided() {
        let mut matches = Vec::new();
        for pos in 0..8u8 {
            let mut m = match_in_ordering_at_round(1, 10 + pos as i32, 20 + pos as i32);
            m.pos = pos;
            m.state = MatchPhase::Decided;
            m.decided_at_ms = 1000 + pos as i64 * 100;
            matches.push((pos, m));
        }
        assert_eq!(round_ended_at_ms(&matches, 1), Some(1700));
    }

    #[test]
    fn round_ended_at_ms_is_none_while_any_match_is_stuck_needing_adjudication() {
        let mut matches = Vec::new();
        for pos in 0..8u8 {
            let mut m = match_in_ordering_at_round(1, 10 + pos as i32, 20 + pos as i32);
            m.pos = pos;
            if pos == 7 {
                m.state = MatchPhase::NeedsAdjudication;
            } else {
                m.state = MatchPhase::Decided;
                m.decided_at_ms = 1000;
            }
            matches.push((pos, m));
        }
        assert_eq!(round_ended_at_ms(&matches, 1), None);
    }
}
