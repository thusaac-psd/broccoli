//! Match pacing: when a match may leave `Ordering` on its own.
//!
//! Scheduled stages share a frozen ordering window and first-game clock.
//! Every match in the previous stage must finish, followed by the next
//! stage's minimum rest, before that clock may open. Unscheduled stages
//! start each match once both rankings arrive.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;

use crate::model::{MatchState, Phase, Setup};
use crate::storage;

/// The frozen shared start for scheduled stages, or the earliest permitted
/// opening for unscheduled stages. `None` means the stage has not opened yet.
pub fn stage_open_at_ms(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    round: u8,
    contest_start_ms: i64,
) -> Option<i64> {
    if setup
        .rounds
        .get(usize::from(round.checked_sub(1)?))?
        .start_at_ms
        .is_some()
    {
        let clock = setup.stage_clocks.get(&round)?;
        return (!clock.awaiting_reschedule).then_some(clock.start_at_ms);
    }
    prerequisites_ready_at_ms(setup, matches, round, contest_start_ms)
}

/// The earliest permitted ordering start, independent of an explicit schedule.
pub fn prerequisites_ready_at_ms(
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
    let expected = usize::from(setup.match_count(previous_round));
    let previous: Vec<&MatchState> = matches
        .iter()
        .map(|(_, m)| m)
        .filter(|m| m.round == previous_round)
        .collect();
    if previous.len() != expected {
        return None;
    }
    // Every predecessor must be decided; the last decision starts the break.
    previous
        .iter()
        .map(|m| m.decided_at_ms())
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .max()
        .and_then(|last| last.checked_add(stage.break_before_seconds.checked_mul(1_000)?))
}

/// The shared first-game time for scheduled stages. Unscheduled stages
/// additionally require both rankings and start at their earliest permitted
/// opening.
pub fn start_at_ms(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    m: &MatchState,
    contest_start_ms: i64,
) -> Option<i64> {
    if setup
        .rounds
        .get(usize::from(m.round.checked_sub(1)?))?
        .start_at_ms
        .is_some()
    {
        let stage = setup.stage_clocks.get(&m.round)?;
        return (!stage.awaiting_reschedule
            && matches!(m.phase, Phase::Ordering | Phase::Preparing))
        .then_some(stage.starts_at_ms);
    }
    if m.phase != Phase::Ordering || m.order_a.is_none() || m.order_b.is_none() {
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

/// Write a decided match's winner into its next-round feeder slot, and try
/// to create the next-round match if that was the second of its two
/// feeders. A no-op for a final-round match (no next round to feed).
pub fn write_next_round_slots(
    host: &Host,
    contest: i32,
    setup: &Setup,
    m: &MatchState,
    winner: i32,
) -> Result<(), SdkError> {
    if m.round as usize >= setup.rounds.len() {
        return Ok(());
    }
    let next_round = m.round + 1;
    host.storage.set(&[(
        storage::slot_key(contest, next_round, m.pos).as_str(),
        winner.to_string().as_str(),
    )])?;
    if setup.third_place.is_some() && usize::from(next_round) == setup.rounds.len() {
        let loser = if winner == m.player_a {
            m.player_b
        } else {
            m.player_a
        };
        host.storage.set(&[(
            storage::slot_key(contest, next_round, m.pos + 2).as_str(),
            loser.to_string().as_str(),
        )])?;
    }
    // The next stage is opened as a whole only after every match in this
    // stage is decided. The caller opens it after this match's decision has
    // committed, so the final predecessor is visible to that barrier.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(break_seconds: i64) -> Setup {
        Setup {
            stage_clocks: Default::default(),
            third_place: None,
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
            phase: Phase::Ordering,
            ..Default::default()
        }
    }

    fn decided(round: u8, pos: u8, winner: i32, at: i64) -> (u8, MatchState) {
        (
            pos,
            MatchState {
                round,
                pos,
                phase: Phase::Decided {
                    winner,
                    decided_at_ms: at,
                },
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
        m.phase = Phase::Playing;
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
            let last = previous.last().unwrap().1.decided_at_ms().unwrap();
            assert_eq!(
                stage_open_at_ms(&config, &previous, round, 0),
                Some(last + i64::from(round - 1) * 15_000),
            );
            let mut incomplete = previous.clone();
            incomplete.last_mut().unwrap().1.phase = Phase::NeedsAdjudication {
                adjudication: crate::model::Adjudication::ContestEnded,
            };
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

    #[test]
    fn semifinal_winners_and_losers_feed_distinct_final_matches() {
        let host = Host::mock();
        let mut setup = crate::test_support::setup_two_rounds();
        setup.seeds = vec![10, 20, 30, 40];
        setup.third_place = Some(crate::model::ThirdPlaceConfig::Shared);
        for (pos, a, b, winner) in [(0, 10, 20, 10), (1, 30, 40, 40)] {
            let m = MatchState {
                round: 1,
                pos,
                player_a: a,
                player_b: b,
                ..Default::default()
            };
            write_next_round_slots(&host, 7, &setup, &m, winner).unwrap();
        }
        for pos in 0..2 {
            storage::create_match_if_both_slots_filled(&host, 7, &setup, 2, pos).unwrap();
        }
        let final_match = storage::load_match(&host, 7, 2).unwrap().unwrap();
        let bronze = storage::load_match(&host, 7, 3).unwrap().unwrap();
        assert_eq!((final_match.player_a, final_match.player_b), (10, 40));
        assert_eq!((bronze.player_a, bronze.player_b), (20, 30));
        assert_eq!(final_match.group_a, bronze.group_a);
        assert_eq!(final_match.group_b, bronze.group_b);
    }

    #[test]
    fn a_final_stage_winner_feeds_no_further_slot() {
        let host = Host::mock();
        let setup = crate::test_support::setup_two_rounds();
        let m = MatchState {
            round: 2,
            player_a: 10,
            player_b: 20,
            ..Default::default()
        };
        write_next_round_slots(&host, 7, &setup, &m, 20).unwrap();
        assert!(host.storage.data().is_empty());
    }
}
