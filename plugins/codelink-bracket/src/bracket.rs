//! Match pacing: when a match may leave `Ordering` on its own.
//!
//! Matches start per pair once their stage has opened and both players are
//! ready. Stages are synchronized: every match in the previous stage must be
//! decided before the next stage's intermission can begin. There is no ranking
//! deadline - once a stage opens, a match waits for its rankings indefinitely
//! (staff can force it with `/start`, which fills a missing ranking with the
//! listed order).

use crate::model::{MatchPhase, MatchState, Setup};

/// When a stage opens. Round one opens with the contest; every later stage
/// opens after *all* matches in the immediately preceding stage are decided,
/// followed by the configured intermission. `None` means the preceding stage
/// is not complete yet.
pub fn stage_open_at_ms(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    round: u8,
    contest_start_ms: i64,
) -> Option<i64> {
    if round <= 1 {
        return Some(contest_start_ms);
    }
    let stage = setup.rounds.get(usize::from(round - 1))?;

    let previous_round = round - 1;
    let expected = crate::storage::matches_in_round(setup.player_count(), previous_round) as usize;
    let previous: Vec<&MatchState> = matches
        .iter()
        .map(|(_, m)| m)
        .filter(|m| m.round == previous_round)
        .collect();
    if previous.len() != expected || previous.iter().any(|m| m.state != MatchPhase::Decided) {
        return None;
    }

    previous
        .iter()
        .map(|m| m.decided_at_ms)
        .max()
        .and_then(|last| last.checked_add(stage.break_before_seconds.checked_mul(1_000)?))
}

/// When match `m` may start by itself: `None` while it is not in `Ordering`
/// or a ranking is still missing; otherwise the stage opening time.
pub fn start_at_ms(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    m: &MatchState,
    contest_start_ms: i64,
) -> Option<i64> {
    if m.state != MatchPhase::Ordering || m.order_a.is_none() || m.order_b.is_none() {
        return None;
    }
    stage_open_at_ms(setup, matches, m.round, contest_start_ms)
}

/// Staff override for a match stuck waiting on a ranking: any missing order
/// becomes the ranked player's own group in the setup's listed order.
/// `order_a` is the order player A must follow, i.e. A's own group.
pub fn fill_missing_orders(m: &mut MatchState) {
    if m.order_a.is_none() {
        m.order_a = Some(m.group_a.clone());
    }
    if m.order_b.is_none() {
        m.order_b = Some(m.group_b.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(break_seconds: i64) -> Setup {
        Setup {
            seeds: (1..=16).collect(),
            rounds: (0..4)
                .map(|i| crate::model::RoundDef {
                    break_before_seconds: if i == 0 { 0 } else { break_seconds },
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn ranked(round: u8, a: i32, b: i32) -> MatchState {
        MatchState {
            round,
            player_a: a,
            player_b: b,
            group_a: vec![1, 2, 3],
            group_b: vec![4, 5, 6],
            order_a: Some(vec![2, 1, 3]),
            order_b: Some(vec![6, 5, 4]),
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
    fn a_later_stage_waits_for_the_whole_previous_stage() {
        // The latest round-1 match finishes at t=400 s. The next stage is
        // therefore unavailable until the full intermission has elapsed.
        let mut matches = (0..8)
            .map(|pos| {
                decided(
                    1,
                    pos,
                    if pos == 0 {
                        10
                    } else if pos == 1 {
                        20
                    } else {
                        100 + pos as i32
                    },
                    if pos == 7 { 400_000 } else { 100_000 },
                )
            })
            .collect::<Vec<_>>();
        matches.push((2, ranked(2, 30, 40)));
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
    fn each_stage_uses_its_own_break_after_the_last_preceding_match() {
        let mut config = setup(0);
        for (i, stage) in config.rounds.iter_mut().enumerate() {
            stage.break_before_seconds = i as i64 * 15;
        }
        assert_eq!(stage_open_at_ms(&config, &[], 1, 8_000), Some(8_000));
        for round in 2..=4 {
            let previous: Vec<_> = (0..crate::storage::matches_in_round(16, round - 1))
                .map(|pos| decided(round - 1, pos, pos as i32, 1_000 + i64::from(pos)))
                .collect();
            let last = previous.last().unwrap().1.decided_at_ms;
            assert_eq!(
                stage_open_at_ms(&config, &previous, round, 0),
                Some(last + i64::from(round - 1) * 15_000),
            );
            let mut incomplete = previous.clone();
            incomplete.last_mut().unwrap().1.state = MatchPhase::NeedsAdjudication;
            assert_eq!(stage_open_at_ms(&config, &incomplete, round, 0), None);
            incomplete.pop();
            assert_eq!(stage_open_at_ms(&config, &incomplete, round, 0), None);
        }
    }

    #[test]
    fn the_staff_override_fills_only_the_missing_order_with_the_listed_group() {
        let mut m = ranked(1, 10, 20);
        m.order_a = None;
        fill_missing_orders(&mut m);
        assert_eq!(m.order_a, Some(vec![1, 2, 3]));
        assert_eq!(m.order_b, Some(vec![6, 5, 4]));
    }
}
