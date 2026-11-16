//! Opening stages. This module is the only writer of a setup's stage
//! progress (`Setup::opened_rounds` and `Setup::stage_clocks`).
//!
//! Opening freezes the stage's rules (and a scheduled stage's shared clock)
//! in the setup document, then creates the stage's matches with create-only
//! writes. The two cannot be one write, so a recovery wakeup is armed before
//! the freeze and cancelled only once every match exists; redelivery repeats
//! the creation idempotently.
//!
//! An unavailable predecessor at a scheduled instant requires a new staff
//! schedule; timer delivery time never shifts the clock.
use broccoli_server_sdk::prelude::*;

use crate::model::{MatchState, Phase, Setup, StageClock};
use crate::timers::TimerKey;
use crate::{bracket, clock, executor, storage};

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
                .filter(|(_, m)| m.round == round && m.winner().is_some())
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

/// Keep `previous`'s stage progress in an edited setup: opened stages stay
/// opened, and a frozen clock survives only while its stage's start is
/// unchanged.
pub fn carry_progress(previous: Setup, next: &mut Setup) {
    next.opened_rounds = previous.opened_rounds;
    next.stage_clocks = previous
        .stage_clocks
        .into_iter()
        .filter(|(round, clock)| {
            next.rounds
                .get(usize::from(round.saturating_sub(1)))
                .is_some_and(|config| config.start_at_ms == Some(clock.start_at_ms))
        })
        .collect();
}

/// Create every match of an opened `round` whose players are known, and
/// start the shared ordering clock of a scheduled stage's new matches. Errors
/// while a feeder slot is still empty, so the caller's recovery wakeup stays.
fn create_matches(host: &Host, contest: i32, setup: &Setup, round: u8) -> Result<(), SdkError> {
    for pos in 0..setup.match_count(round) {
        storage::create_match_if_both_slots_filled(host, contest, setup, round, pos)?;
    }
    let created: Vec<_> = storage::load_all_matches(host, contest)?
        .into_iter()
        .filter(|(_, m)| m.round == round)
        .collect();
    if setup.is_scheduled(round) {
        for (id, m) in &created {
            if m.phase == Phase::Ordering {
                executor::tick(host, contest, *id)?;
            }
        }
    }
    if created.len() != usize::from(setup.match_count(round)) {
        return Err(SdkError::Other("Stage is waiting for feeder slots".into()));
    }
    Ok(())
}

/// Open a scheduled stage at its start; false for an unscheduled stage.
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
        let now = crate::clock::clock(host, contest)?;
        if now.contest_ended() {
            return Ok(true);
        }
        let timer = TimerKey::StageOpen {
            contest,
            round,
            at_ms: start_at_ms,
        }
        .encode();
        if now.now < start_at_ms {
            host.timer.schedule(start_at_ms, &timer, "")?;
            return Ok(true);
        }
        let matches = storage::load_all_matches(host, contest)?;
        let times = crate::clock::contest_times(host, contest)?;
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
        create_matches(host, contest, &setup, round)?;
        host.timer.cancel(&timer)?;
        return Ok(true);
    }
    Err(SdkError::Other(
        "Stage opening CAS retry limit exceeded".into(),
    ))
}

/// Open a later stage once every match in the previous stage is decided. The
/// stage's match documents are created together at this point, so rankings
/// cannot be submitted before the official stage opening.
pub fn try_open_stage(host: &Host, contest: i32, round: u8) -> Result<(), SdkError> {
    if try_open_scheduled(host, contest, round)? {
        return Ok(());
    }
    let key = storage::setup_key(contest);
    for _ in 0..100 {
        let Some(snapshot) = host.storage.snapshot(&key)? else {
            return Ok(());
        };
        let mut setup: Setup = serde_json::from_str(&snapshot.value)?;
        if round == 0
            || round as usize > setup.rounds.len()
            || round as usize > setup.opened_rounds + 1
        {
            return Ok(());
        }
        let matches = storage::load_all_matches(host, contest)?;
        if round > 1 && bracket::stage_open_at_ms(&setup, &matches, round, 0).is_none() {
            return Ok(());
        }
        let times = clock::contest_times(host, contest)?;
        let Some(at) = bracket::stage_open_at_ms(&setup, &matches, round, times.start_ms) else {
            return Ok(());
        };
        let now = clock::now_ms(host)?;
        if times.end_ms.is_some_and(|end| now >= end || at >= end) {
            return Ok(());
        }
        let wake = if now < at {
            at
        } else {
            now.saturating_add(1_000)
        };
        let timer_key = TimerKey::StageOpen {
            contest,
            round,
            at_ms: wake,
        }
        .encode();
        // Install recovery before freezing rules: partial creation can be retried.
        host.timer.schedule(wake, &timer_key, "")?;
        if now < at {
            return Ok(());
        }
        if setup.opened_rounds < round as usize {
            setup.opened_rounds = round as usize;
            if !host.storage.compare_and_set_snapshot(
                &key,
                Some(&snapshot),
                &serde_json::to_string(&setup)?,
            )? {
                continue;
            }
        }
        create_matches(host, contest, &setup, round)?;
        host.timer.cancel(&timer_key)?;
        return Ok(());
    }
    Err(SdkError::Other(
        "Stage opening CAS retry limit exceeded".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Adjudication, RoundDef, ThirdPlaceConfig};
    use crate::test_support::{queue_now, seed, setup_two_rounds};

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
                        phase: Phase::Decided {
                            winner: i32::from(pos),
                            decided_at_ms: at,
                        },
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

    fn queue_contest_start(host: &Host, start_ms: i64) {
        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": start_ms as f64 }]));
    }

    fn seed_completed_first_stage(host: &Host, setup: &Setup) {
        for pos in 0..storage::matches_in_round(16, 1) {
            let winner = 100 + i32::from(pos);
            seed(
                host,
                7,
                setup,
                storage::match_id_for(16, 1, pos),
                &MatchState {
                    round: 1,
                    pos,
                    phase: Phase::Decided {
                        winner,
                        decided_at_ms: 100_000,
                    },
                    ..Default::default()
                },
            );
            host.storage
                .set(&[(
                    storage::slot_key(7, 2, pos).as_str(),
                    winner.to_string().as_str(),
                )])
                .unwrap();
        }
    }

    #[test]
    fn interrupted_stage_creation_keeps_a_retry_until_every_winner_slot_is_ready() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed_completed_first_stage(&host, &setup);
        host.storage.delete(&[&storage::slot_key(7, 2, 1)]).unwrap();
        queue_contest_start(&host, 0);
        queue_now(&host, 700_000);
        assert!(try_open_stage(&host, 7, 2).is_err());
        assert_eq!(
            storage::load_setup(&host, 7)
                .unwrap()
                .unwrap()
                .opened_rounds,
            2
        );
        assert_eq!(host.timer.scheduled_at("stage:7:2:701000"), Some(701_000));
        host.storage
            .set(&[(&storage::slot_key(7, 2, 1), "101")])
            .unwrap();
        queue_contest_start(&host, 0);
        queue_now(&host, 701_000);
        crate::timers::handle_timer(&host, "stage:7:2:701000", "", 701_000).unwrap();
        assert_eq!(storage::load_all_matches(&host, 7).unwrap().len(), 12);
    }

    #[test]
    fn stale_stage_wakeup_uses_edited_break_and_cannot_replace_an_earlier_timer() {
        let host = Host::mock();
        let mut setup = setup_two_rounds();
        seed_completed_first_stage(&host, &setup);
        queue_contest_start(&host, 0);
        queue_now(&host, 100_000);
        try_open_stage(&host, 7, 2).unwrap();
        setup.rounds[1].break_before_seconds = 10;
        host.storage
            .set(&[(
                &storage::setup_key(7),
                &serde_json::to_string(&setup).unwrap(),
            )])
            .unwrap();
        queue_contest_start(&host, 0);
        queue_now(&host, 100_000);
        try_open_stage(&host, 7, 2).unwrap();
        assert_eq!(host.timer.scheduled_at("stage:7:2:110000"), Some(110_000));
        assert_eq!(host.timer.scheduled_at("stage:7:2:700000"), Some(700_000));
        queue_contest_start(&host, 0);
        queue_now(&host, 110_000);
        crate::timers::handle_timer(&host, "stage:7:2:110000", "", 110_000).unwrap();
        assert_eq!(
            storage::load_setup(&host, 7)
                .unwrap()
                .unwrap()
                .opened_rounds,
            2
        );
        assert!(storage::load_match(&host, 7, 8).unwrap().is_some());
    }

    #[test]
    fn stage_opening_waits_for_intermission_then_creates_every_match_once() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed_completed_first_stage(&host, &setup);
        queue_contest_start(&host, 0);
        queue_now(&host, 100_000);
        try_open_stage(&host, 7, 2).unwrap();
        assert_eq!(host.timer.scheduled_at("stage:7:2:700000"), Some(700_000));
        assert!(
            storage::load_match(&host, 7, storage::match_id_for(16, 2, 0))
                .unwrap()
                .is_none()
        );

        queue_contest_start(&host, 0);
        queue_now(&host, 700_000);
        try_open_stage(&host, 7, 2).unwrap();
        for pos in 0..storage::matches_in_round(16, 2) {
            let m = storage::load_match(&host, 7, storage::match_id_for(16, 2, pos))
                .unwrap()
                .unwrap();
            assert_eq!(m.phase, Phase::Ordering);
            assert_eq!(m.player_a, 100 + i32::from(pos) * 2);
            assert_eq!(m.player_b, 101 + i32::from(pos) * 2);
        }
        assert!(!host.timer.is_scheduled("stage:7:2:701000"));
        let id = storage::match_id_for(16, 2, 0);
        storage::update_match(&host, 7, id, |m| {
            m.order_a = Some(vec![401, 402, 403]);
            Ok(())
        })
        .unwrap();
        queue_contest_start(&host, 0);
        queue_now(&host, 800_000);
        try_open_stage(&host, 7, 2).unwrap();
        assert_eq!(
            storage::load_match(&host, 7, id).unwrap().unwrap().order_a,
            Some(vec![401, 402, 403])
        );
    }

    #[test]
    fn final_staff_decision_schedules_the_next_stage_after_persisting_the_result() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed_completed_first_stage(&host, &setup);
        let id = storage::match_id_for(16, 1, 7);
        storage::update_match(&host, 7, id, |m| {
            m.phase = Phase::NeedsAdjudication {
                adjudication: Adjudication::TiebreakExhausted,
            };
            m.player_a = 107;
            m.player_b = 207;
            Ok(())
        })
        .unwrap();
        queue_now(&host, 200_000);
        queue_contest_start(&host, 0);
        queue_now(&host, 200_000);
        crate::executor::drive(&host, 7, id, crate::machine::Event::Award { winner: 107 }).unwrap();
        assert_eq!(host.timer.scheduled_at("stage:7:2:800000"), Some(800_000));
        assert!(
            storage::load_match(&host, 7, storage::match_id_for(16, 2, 0))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn incomplete_stage_does_not_create_the_next_stage_or_schedule_it() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed_completed_first_stage(&host, &setup);
        storage::update_match(&host, 7, storage::match_id_for(16, 1, 7), |m| {
            m.phase = Phase::NeedsAdjudication {
                adjudication: Adjudication::TiebreakExhausted,
            };
            Ok(())
        })
        .unwrap();
        try_open_stage(&host, 7, 2).unwrap();
        assert!(!host.timer.is_scheduled("stage:7:2:700000"));
        assert!(
            storage::load_match(&host, 7, storage::match_id_for(16, 2, 0))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stage_opening_does_not_schedule_or_create_matches_after_contest_end() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed_completed_first_stage(&host, &setup);
        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": 0.0, "end_ms": 600_000.0 }]));
        queue_now(&host, 700_000);
        try_open_stage(&host, 7, 2).unwrap();
        assert!(!host.timer.is_scheduled("stage:7:2:700000"));
        assert!(
            storage::load_match(&host, 7, storage::match_id_for(16, 2, 0))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn first_stage_is_created_only_at_the_contest_start() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        host.storage
            .set(&[(
                storage::setup_key(7).as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            )])
            .unwrap();
        for pos in 0..16 {
            host.storage
                .set(&[(
                    storage::slot_key(7, 1, pos).as_str(),
                    (100 + i32::from(pos)).to_string().as_str(),
                )])
                .unwrap();
        }
        queue_contest_start(&host, 60_000);
        queue_now(&host, 5_000);
        try_open_stage(&host, 7, 1).unwrap();
        assert!(storage::load_all_matches(&host, 7).unwrap().is_empty());
        assert_eq!(host.timer.scheduled_at("stage:7:1:60000"), Some(60_000));
        queue_contest_start(&host, 60_000);
        queue_now(&host, 60_000);
        try_open_stage(&host, 7, 1).unwrap();
        assert_eq!(storage::load_all_matches(&host, 7).unwrap().len(), 8);
    }
}
