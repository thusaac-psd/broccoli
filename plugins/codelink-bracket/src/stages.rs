//! Shared stage clocks. An unavailable predecessor at the scheduled instant
//! requires a new staff schedule; timer delivery time never shifts the clock.
use broccoli_server_sdk::prelude::*;

use crate::model::{MatchState, Setup, StageClock};
use crate::{bracket, judge, storage};

pub const ORDERING_SECONDS: i64 = 180;

#[derive(Debug, serde::Serialize)]
pub struct StageView {
    pub round: u8,
    pub phase: &'static str,
    pub start_at_ms: Option<i64>,
    pub ordering_deadline_ms: Option<i64>,
    pub starts_at_ms: Option<i64>,
    pub match_count: u8,
    pub decided_count: usize,
}

pub fn views(setup: &Setup, matches: &[(u8, MatchState)], now: i64) -> Vec<StageView> {
    setup
        .rounds
        .iter()
        .enumerate()
        .map(|(index, definition)| {
            let round = index as u8 + 1;
            let clock = setup
                .stage_clocks
                .get(&round)
                .cloned()
                .or_else(|| planned_clock(setup, round).ok());
            let decided_count = matches
                .iter()
                .filter(|(_, m)| m.round == round && m.state == crate::model::MatchPhase::Decided)
                .count();
            let phase = if decided_count == usize::from(setup.match_count(round)) {
                "complete"
            } else if clock.as_ref().is_some_and(|c| c.awaiting_reschedule) {
                "awaiting_reschedule"
            } else if setup.opened_rounds < usize::from(round) {
                "scheduled"
            } else if clock.as_ref().is_some_and(|c| now < c.ordering_deadline_ms) {
                "ordering"
            } else if clock.as_ref().is_some_and(|c| now < c.starts_at_ms) {
                "preparing"
            } else {
                "playing"
            };
            StageView {
                round,
                phase,
                start_at_ms: definition.start_at_ms,
                ordering_deadline_ms: clock.as_ref().map(|c| c.ordering_deadline_ms),
                starts_at_ms: clock.map(|c| c.starts_at_ms),
                match_count: setup.match_count(round),
                decided_count,
            }
        })
        .collect()
}

pub fn planned_clock(setup: &Setup, round: u8) -> Result<StageClock, SdkError> {
    let stage = setup
        .rounds
        .get(usize::from(round.saturating_sub(1)))
        .ok_or_else(|| SdkError::Other("Stage is not configured".into()))?;
    let start_at_ms = stage
        .start_at_ms
        .ok_or_else(|| SdkError::Other("Stage has no schedule".into()))?;
    let ordering_deadline_ms = start_at_ms
        .checked_add(ORDERING_SECONDS * 1_000)
        .ok_or_else(|| SdkError::Other("Stage ordering deadline overflow".into()))?;
    let starts_at_ms = stage
        .post_order_wait()
        .checked_mul(1_000)
        .and_then(|wait| ordering_deadline_ms.checked_add(wait))
        .ok_or_else(|| SdkError::Other("Stage game start overflow".into()))?;
    Ok(StageClock {
        start_at_ms,
        ordering_deadline_ms,
        starts_at_ms,
        awaiting_reschedule: false,
    })
}

pub fn decide_clock(
    setup: &Setup,
    matches: &[(u8, MatchState)],
    round: u8,
    contest_start: i64,
) -> Result<StageClock, SdkError> {
    let mut planned = planned_clock(setup, round)?;
    if let Some(saved) = setup.stage_clocks.get(&round)
        && saved.start_at_ms == planned.start_at_ms
    {
        return Ok(saved.clone());
    }
    planned.awaiting_reschedule =
        !bracket::prerequisites_ready_at_ms(setup, matches, round, contest_start)
            .is_some_and(|ready| ready <= planned.start_at_ms)
            || usize::from(round) > setup.opened_rounds + 1;
    Ok(planned)
}

/// Return false for a legacy stage, letting the original opening path handle it.
pub fn try_open_scheduled(host: &Host, contest: i32, round: u8) -> Result<bool, SdkError> {
    let key = storage::setup_key(contest);
    for _ in 0..100 {
        let Some(snapshot) = host.storage.snapshot(&key)? else {
            return Ok(false);
        };
        let mut setup: Setup = serde_json::from_str(&snapshot.value)?;
        let Some(config) = round
            .checked_sub(1)
            .and_then(|r| setup.rounds.get(usize::from(r)))
        else {
            return Ok(false);
        };
        let Some(start_at_ms) = config.start_at_ms else {
            return Ok(false);
        };
        let now = judge::clock(host, contest)?;
        if now.contest_ended() {
            return Ok(true);
        }
        let timer = format!("{}:{start_at_ms}", judge::stage_timer_key(contest, round));
        if now.now < start_at_ms {
            host.timer.schedule(start_at_ms, &timer, "")?;
            return Ok(true);
        }
        let matches = storage::load_all_matches(host, contest)?;
        let times = judge::contest_times(host, contest)?;
        let clock = decide_clock(&setup, &matches, round, times.start_ms)?;
        if setup.stage_clocks.get(&round) != Some(&clock) {
            setup.stage_clocks.insert(round, clock.clone());
            if !clock.awaiting_reschedule {
                setup.opened_rounds = setup.opened_rounds.max(usize::from(round));
            }
            // Schedule recovery before freezing the stage or creating its matches.
            host.timer
                .schedule(now.now.saturating_add(1_000), &timer, "")?;
            if !host.storage.compare_and_set_snapshot(
                &key,
                Some(&snapshot),
                &serde_json::to_string(&setup)?,
            )? {
                continue;
            }
        }
        if clock.awaiting_reschedule {
            host.timer.cancel(&timer)?;
            return Ok(true);
        }
        for pos in 0..setup.match_count(round) {
            storage::create_match_if_both_slots_filled(host, contest, &setup, round, pos)?;
            judge::try_autostart(
                host,
                contest,
                storage::match_id_for(setup.player_count(), round, pos),
            )?;
        }
        let actual = storage::load_all_matches(host, contest)?
            .iter()
            .filter(|(_, m)| m.round == round)
            .count();
        if actual != usize::from(setup.match_count(round)) {
            return Err(SdkError::Other("Stage is waiting for feeder slots".into()));
        }
        host.timer.cancel(&timer)?;
        return Ok(true);
    }
    Err(SdkError::Other(
        "Stage opening CAS retry limit exceeded".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MatchPhase, RoundDef, ThirdPlaceConfig};

    fn setup() -> Setup {
        Setup {
            seeds: vec![1, 2, 3, 4],
            opened_rounds: 1,
            third_place: Some(ThirdPlaceConfig::Shared),
            rounds: vec![
                RoundDef {
                    start_at_ms: Some(0),
                    ..Default::default()
                },
                RoundDef {
                    start_at_ms: Some(1_000_000),
                    break_before_seconds: 60,
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }
    fn previous(at: i64) -> Vec<(u8, MatchState)> {
        (0..2)
            .map(|pos| {
                (
                    pos,
                    MatchState {
                        round: 1,
                        pos,
                        state: MatchPhase::Decided,
                        decided_at_ms: at,
                        ..Default::default()
                    },
                )
            })
            .collect()
    }
    #[test]
    fn delayed_and_duplicate_stage_callbacks_create_each_match_once_with_the_original_clock() {
        let host = Host::mock();
        let mut config = setup();
        config.opened_rounds = 0;
        config.rounds[0].start_at_ms = Some(1_000);
        config.rounds[0].group_a = vec![101];
        config.rounds[0].group_b = vec![201];
        config.rounds[0].xiaoju_seconds = 60;
        host.storage
            .set(&[(
                &storage::setup_key(7),
                &serde_json::to_string(&config).unwrap(),
            )])
            .unwrap();
        for _ in 0..2 {
            host.db
                .queue_query_result(serde_json::json!([{ "now_ms": 212_000 }]));
            host.db
                .queue_query_result(serde_json::json!([{ "start_ms": 0 }]));
            for _ in 0..2 {
                host.db
                    .queue_query_result(serde_json::json!([{ "now_ms": 212_000 }]));
            }
            assert!(try_open_scheduled(&host, 7, 1).unwrap());
            let matches = storage::load_all_matches(&host, 7).unwrap();
            assert_eq!(matches.len(), 2);
            for (_, m) in matches {
                assert_eq!(m.xiaoju.len(), 1);
                assert_eq!(m.xiaoju[0].opened_at_ms, 211_000);
            }
        }
    }

    #[test]
    fn one_clock_covers_ordering_preparation_and_both_final_matches() {
        let config = setup();
        let clock = decide_clock(&config, &previous(900_000), 2, 0).unwrap();
        assert!(!clock.awaiting_reschedule);
        assert_eq!(clock.ordering_deadline_ms, 1_180_000);
        assert_eq!(clock.starts_at_ms, 1_210_000);
        assert_eq!(config.match_count(2), 2);
    }
    #[test]
    fn a_missed_schedule_stays_blocked_until_staff_change_the_time() {
        let mut config = setup();
        let blocked = decide_clock(&config, &previous(970_000), 2, 0).unwrap();
        assert!(blocked.awaiting_reschedule);
        config.stage_clocks.insert(2, blocked.clone());
        assert_eq!(
            decide_clock(&config, &previous(900_000), 2, 0).unwrap(),
            blocked
        );
        config.rounds[1].start_at_ms = Some(1_100_000);
        assert!(
            !decide_clock(&config, &previous(970_000), 2, 0)
                .unwrap()
                .awaiting_reschedule
        );
    }
    #[test]
    fn missing_predecessors_block_but_delayed_delivery_does_not_shift_a_ready_stage() {
        let config = setup();
        assert!(
            decide_clock(&config, &[], 2, 0)
                .unwrap()
                .awaiting_reschedule
        );
        let first = decide_clock(&config, &previous(900_000), 2, 0).unwrap();
        let mut recovered = config;
        recovered.stage_clocks.insert(2, first.clone());
        recovered.opened_rounds = 2;
        assert_eq!(
            decide_clock(&recovered, &previous(900_000), 2, 0).unwrap(),
            first
        );
    }
}
