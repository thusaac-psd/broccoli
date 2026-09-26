//! Match pacing: when a match may leave `Ordering` on its own.
//!
//! Matches start per pair, when ready, with no staff action: a match starts
//! once both players have ranked, the contest has started, and each player
//! has had `Setup::round_intermission_seconds` of rest since their OWN
//! previous match. There is no round-wide synchronisation and no ranking
//! deadline - a match waits for its rankings indefinitely (staff can force
//! it with `/start`, which fills a missing ranking with the listed order).
//!
//! Known, accepted consequence (the owner chose this over synchronised
//! rounds): every match of a round shares the round's problems, so a pair
//! that starts later faces problems earlier pairs have already seen.

use crate::model::{MatchPhase, MatchState, Setup};

/// When `player` is free to play in `round`: immediately in round 1,
/// otherwise the moment their previous-round match was decided plus the
/// break. `i64::MAX` if that match is not decided yet (the player cannot be
/// in a later match before winning it, so this only guards odd states).
pub fn player_free_at_ms(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    player: i32,
    round: u8,
) -> i64 {
    if round <= 1 {
        return 0;
    }
    matches
        .iter()
        .map(|(_, m)| m)
        .find(|m| m.round == round - 1 && m.winner == Some(player))
        .filter(|m| m.state == MatchPhase::Decided)
        .map(|m| m.decided_at_ms + setup.round_intermission_seconds * 1_000)
        .unwrap_or(i64::MAX)
}

/// When match `m` may start by itself: `None` while it is not in `Ordering`
/// or a ranking is still missing; otherwise the latest of the contest start
/// and both players' [`player_free_at_ms`].
pub fn start_at_ms(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    m: &MatchState,
    contest_start_ms: i64,
) -> Option<i64> {
    if m.state != MatchPhase::Ordering || m.order_a.is_none() || m.order_b.is_none() {
        return None;
    }
    Some(
        contest_start_ms
            .max(player_free_at_ms(setup, matches, m.player_a, m.round))
            .max(player_free_at_ms(setup, matches, m.player_b, m.round)),
    )
}

/// Staff override for a match stuck waiting on a ranking: any missing order
/// becomes the ranked player's own group in the setup's listed order.
/// `order_a` is the order player A must follow, i.e. A's own group.
pub fn fill_missing_orders(m: &mut MatchState) {
    if m.order_a.is_none() {
        m.order_a = Some(m.group_a);
    }
    if m.order_b.is_none() {
        m.order_b = Some(m.group_b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(break_seconds: i64) -> Setup {
        Setup {
            round_intermission_seconds: break_seconds,
            ..Default::default()
        }
    }

    fn ranked(round: u8, a: i32, b: i32) -> MatchState {
        MatchState {
            round,
            player_a: a,
            player_b: b,
            group_a: [1, 2, 3],
            group_b: [4, 5, 6],
            order_a: Some([2, 1, 3]),
            order_b: Some([6, 5, 4]),
            state: MatchPhase::Ordering,
            ..Default::default()
        }
    }

    fn decided(round: u8, pos: u8, winner: i32, at: i64) -> (u8, MatchState) {
        (
            pos,
            MatchState {
                round,
                pos,
                winner: Some(winner),
                decided_at_ms: at,
                state: MatchPhase::Decided,
                ..Default::default()
            },
        )
    }

    #[test]
    fn a_round_one_match_starts_at_the_contest_start_once_both_have_ranked() {
        let m = ranked(1, 10, 20);
        assert_eq!(start_at_ms(&setup(600), &[], &m, 5_000), Some(5_000));
    }

    #[test]
    fn a_missing_ranking_means_no_start_time_however_long_it_takes() {
        let mut m = ranked(1, 10, 20);
        m.order_b = None;
        assert_eq!(start_at_ms(&setup(0), &[], &m, 0), None);
    }

    #[test]
    fn a_later_match_waits_for_each_players_own_break_not_the_whole_round() {
        // Player 10 won at t=100 s, player 20 at t=400 s; other round-1
        // matches are still running, and that must not matter.
        let matches = vec![
            decided(1, 0, 10, 100_000),
            decided(1, 1, 20, 400_000),
            (2, ranked(1, 30, 40)),
        ];
        let m = ranked(2, 10, 20);
        assert_eq!(start_at_ms(&setup(60), &matches, &m, 0), Some(460_000));
    }

    #[test]
    fn a_match_that_has_already_started_has_no_start_time() {
        let mut m = ranked(1, 10, 20);
        m.state = MatchPhase::InProgress;
        assert_eq!(start_at_ms(&setup(0), &[], &m, 0), None);
    }

    #[test]
    fn a_player_whose_previous_match_is_not_decided_is_never_free() {
        let matches = vec![(0, ranked(1, 10, 11))];
        assert_eq!(player_free_at_ms(&setup(0), &matches, 10, 2), i64::MAX);
    }

    #[test]
    fn the_staff_override_fills_only_the_missing_order_with_the_listed_group() {
        let mut m = ranked(1, 10, 20);
        m.order_a = None;
        fill_missing_orders(&mut m);
        assert_eq!(m.order_a, Some([1, 2, 3]));
        assert_eq!(m.order_b, Some([6, 5, 4]));
    }
}
