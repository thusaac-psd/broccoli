//! The match state machine.
//!
//! [`transition`] is the only function that changes a match's [`Phase`]. It
//! is pure: the executor (`crate::executor`) gathers its inputs, runs it
//! inside a compare-and-set loop, arms the wakeups it asks for before writing
//! the new state, and applies its effects once the write has committed.
//!
//! # Events
//!
//! Every timer, judging result, ranking and staff "force expiry" is the same
//! [`Event::Tick`]: re-evaluate the match at the current server time from the
//! current evidence. Judging completion, a deadline and a staff nudge can all
//! land on the same match at unpredictable times, in any order and more than
//! once, so a tick must give the same answer however often it runs. Only the
//! two staff overrides are separate events: [`Event::Start`] (an unscheduled
//! stage's match whose player never ranks) and [`Event::Award`].
//!
//! # Wakeups and effects
//!
//! Each phase that waits for a time owns exactly one wakeup purpose
//! ([`MatchTimer`]), so scheduling one phase's wakeup can never replace
//! another's. A wakeup is armed before the state that needs it is written,
//! so it cannot be lost; a spurious wakeup from a lost write is harmless,
//! since it only ticks the match again. A wakeup is cancelled only after the
//! state that no longer needs it has committed and the post-commit work has
//! succeeded, so until then it doubles as the retry of that work.
//!
//! Entering a closed phase ([`Phase::is_closed`]) arms a
//! [`MatchTimer::Closing`] wakeup that is cancelled only once the closing
//! work (feedback publication, admission release, the winner's next-stage
//! slot) has succeeded, whatever triggered the decision.

use broccoli_server_sdk::prelude::Verdict;

use crate::bracket;
use crate::clock::Clock;
use crate::decide::{self, MatchOutcome, SubmissionRecord, XiaojuOutcome};
use crate::model::{Adjudication, MatchState, Phase, Setup, XiaojuState};

/// How long after entering a closed phase its closing work is retried.
const CLOSING_RETRY_MS: i64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Re-evaluate the match at the current time from current evidence.
    Tick,
    /// Staff start a match in an unscheduled stage now; missing rankings
    /// become the listed order.
    Start,
    /// Staff award the match to `winner`.
    Award { winner: i32 },
}

/// Everything [`transition`] reads besides the match itself.
#[derive(Debug, Clone, Copy)]
pub struct Inputs<'a> {
    pub setup: &'a Setup,
    pub clock: Clock,
    /// Submissions to the open game, for a tick while one is open.
    pub subs: &'a [SubmissionRecord],
    /// When a match in an unscheduled stage may start
    /// (`bracket::start_at_ms`), for a tick while it is ordering.
    pub start_at_ms: Option<i64>,
}

/// A wakeup purpose of one match. Every match wakeup ticks the match; the
/// purpose only keeps the wakeups of different phases apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchTimer {
    /// An unscheduled stage's match may start.
    Start,
    /// A scheduled stage's ordering window closes.
    OrderingClose,
    /// A scheduled stage's preparation ends and its first games open.
    GameStart,
    /// The open game's deadline, or the contest end if that comes first.
    Deadline,
    /// The moment no new opponent submission can beat an accepted one. Each
    /// accepted submission owns its own wakeup, and one moved earlier by
    /// opponent penalties also names its time (`at_us`), so a callback that
    /// read an older snapshot cannot postpone it.
    Effective {
        game: u8,
        submission_id: i32,
        at_us: Option<i64>,
    },
    /// The judging grace of [`Phase::AwaitingJudge`] runs out.
    JudgeWait,
    /// The break before the next game ends.
    Intermission,
    /// Retry of a closed match's closing work.
    Closing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wakeup {
    pub timer: MatchTimer,
    pub at_ms: i64,
}

/// Post-commit work, applied in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Release settled feedback; in a closed match, also retire admissions
    /// still being judged. Also cancels the effective-time wakeups of
    /// settled games.
    PublishFeedback,
    /// Write the winner (and, before a final with a bronze match, the
    /// loser) into the next stage's slots and try to open that stage.
    AdvanceBracket {
        winner: i32,
    },
    Cancel(MatchTimer),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Transition {
    pub next: MatchState,
    /// Armed before `next` is written.
    pub wakeups: Vec<Wakeup>,
    /// Applied once `next` has committed.
    pub effects: Vec<Effect>,
}

/// Why an event cannot apply to the match as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    NotParticipant(i32),
    AlreadyDecided,
    NotAwaitingStart,
    TimeOverflow(&'static str),
}

impl std::fmt::Display for Rejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Rejection::NotParticipant(player) => {
                write!(f, "player {player} is not a participant in this match")
            }
            Rejection::AlreadyDecided => f.write_str("this match has already been decided"),
            Rejection::NotAwaitingStart => f.write_str("match is not awaiting start"),
            Rejection::TimeOverflow(what) => write!(f, "{what} is out of range"),
        }
    }
}

/// Apply `event` to `current`. The only place a match's phase changes.
pub fn transition(
    current: &MatchState,
    event: Event,
    inputs: &Inputs<'_>,
) -> Result<Transition, Rejection> {
    let mut next = current.clone();
    let mut wakeups = Vec::new();
    match event {
        Event::Tick => tick(&mut next, inputs, &mut wakeups)?,
        Event::Start => {
            if next.phase != Phase::Ordering || is_scheduled(inputs.setup, next.round) {
                return Err(Rejection::NotAwaitingStart);
            }
            bracket::fill_missing_orders(&mut next);
            open_game(&mut next, inputs, inputs.clock.now, &mut wakeups)?;
        }
        Event::Award { winner } => {
            if winner != next.player_a && winner != next.player_b {
                return Err(Rejection::NotParticipant(winner));
            }
            if matches!(next.phase, Phase::Decided { .. }) {
                return Err(Rejection::AlreadyDecided);
            }
            next.phase = Phase::Decided {
                winner,
                decided_at_ms: inputs.clock.now,
            };
        }
    }
    Ok(finish(current, next, inputs, wakeups))
}

/// Add the effects every transition into `next`'s phase needs: publication,
/// closing work, and cancelling the wakeup of a phase that was left.
///
/// While a match is open, the left phase's wakeup is cancelled last, so it
/// retries the post-commit work until that has succeeded. A closing match
/// has its [`MatchTimer::Closing`] wakeup armed before the write for that,
/// so the left wakeup is released first and `Closing` once the closing work
/// is done.
fn finish(
    current: &MatchState,
    next: MatchState,
    inputs: &Inputs<'_>,
    mut wakeups: Vec<Wakeup>,
) -> Transition {
    let left = phase_timer(current, inputs.setup);
    let left = left
        .filter(|_| left != phase_timer(&next, inputs.setup))
        .map(Effect::Cancel);
    let publish = (!next.xiaoju.is_empty()).then_some(Effect::PublishFeedback);
    let effects = if next.phase.is_closed() {
        wakeups.push(Wakeup {
            timer: MatchTimer::Closing,
            at_ms: inputs.clock.now.saturating_add(CLOSING_RETRY_MS),
        });
        let advance = next
            .winner()
            .map(|winner| Effect::AdvanceBracket { winner });
        left.into_iter()
            .chain(publish)
            .chain(advance)
            .chain([Effect::Cancel(MatchTimer::Closing)])
            .collect()
    } else {
        publish.into_iter().chain(left).collect()
    };
    Transition {
        next,
        wakeups,
        effects,
    }
}

/// The wakeup purpose a match waits on in its phase.
fn phase_timer(m: &MatchState, setup: &Setup) -> Option<MatchTimer> {
    match m.phase {
        Phase::Ordering if is_scheduled(setup, m.round) => Some(MatchTimer::OrderingClose),
        Phase::Ordering => Some(MatchTimer::Start),
        Phase::Preparing => Some(MatchTimer::GameStart),
        Phase::Playing => Some(MatchTimer::Deadline),
        Phase::Intermission { .. } => Some(MatchTimer::Intermission),
        Phase::AwaitingJudge { .. } => Some(MatchTimer::JudgeWait),
        Phase::Decided { .. } | Phase::NeedsAdjudication { .. } => None,
    }
}

/// Whether `round` follows a shared stage schedule.
pub fn is_scheduled(setup: &Setup, round: u8) -> bool {
    round
        .checked_sub(1)
        .and_then(|index| setup.rounds.get(usize::from(index)))
        .is_some_and(|r| r.start_at_ms.is_some())
}

fn adjudicate(m: &mut MatchState, adjudication: Adjudication) {
    m.phase = Phase::NeedsAdjudication { adjudication };
}

fn wake(wakeups: &mut Vec<Wakeup>, timer: MatchTimer, at_ms: i64) {
    wakeups.push(Wakeup { timer, at_ms });
}

fn tick(
    m: &mut MatchState,
    inputs: &Inputs<'_>,
    wakeups: &mut Vec<Wakeup>,
) -> Result<(), Rejection> {
    let Inputs { setup, clock, .. } = *inputs;
    let now = clock.now;
    match m.phase {
        Phase::Ordering | Phase::Preparing if is_scheduled(setup, m.round) => {
            let Some(stage) = setup
                .stage_clocks
                .get(&m.round)
                .filter(|stage| !stage.awaiting_reschedule)
            else {
                return Ok(());
            };
            if clock.contest_ended() {
                adjudicate(m, Adjudication::ContestEnded);
                return Ok(());
            }
            if m.phase == Phase::Ordering {
                if now < stage.ordering_deadline_ms {
                    wake(
                        wakeups,
                        MatchTimer::OrderingClose,
                        stage.ordering_deadline_ms,
                    );
                    return Ok(());
                }
                bracket::fill_missing_orders(m);
                m.phase = Phase::Preparing;
            }
            if now < stage.starts_at_ms {
                wake(wakeups, MatchTimer::GameStart, stage.starts_at_ms);
                return Ok(());
            }
            open_game(m, inputs, stage.starts_at_ms, wakeups)
        }
        Phase::Ordering => {
            // A match that would start at or after the contest end could never
            // be played: it waits for staff instead of starting or waking.
            let Some(at) = inputs.start_at_ms else {
                return Ok(());
            };
            if clock.contest_ended() || clock.contest_end.is_some_and(|end| at >= end) {
                return Ok(());
            }
            if now < at {
                wake(wakeups, MatchTimer::Start, at);
                return Ok(());
            }
            open_game(m, inputs, now, wakeups)
        }
        // Only scheduled stages prepare.
        Phase::Preparing => Ok(()),
        Phase::Intermission { next_game_at_ms } => {
            if clock.contest_ended() {
                adjudicate(m, Adjudication::ContestEnded);
                return Ok(());
            }
            if now < next_game_at_ms {
                wake(wakeups, MatchTimer::Intermission, next_game_at_ms);
                return Ok(());
            }
            open_game(m, inputs, next_game_at_ms, wakeups)
        }
        Phase::Playing | Phase::AwaitingJudge { .. } => play(m, inputs, wakeups),
        Phase::Decided { .. } | Phase::NeedsAdjudication { .. } => Ok(()),
    }
}

/// Decide the open game from its submissions, then what follows it.
fn play(
    m: &mut MatchState,
    inputs: &Inputs<'_>,
    wakeups: &mut Vec<Wakeup>,
) -> Result<(), Rejection> {
    let Inputs {
        setup, clock, subs, ..
    } = *inputs;
    let now = clock.now;
    let Some(round_def) = setup.round_for_match(m.round, m.pos) else {
        adjudicate(m, Adjudication::SetupMissing);
        return Ok(());
    };
    let Some(game) = m.xiaoju.last() else {
        return Ok(());
    };
    let index = game.index;
    let cutoff = clock.cutoff(game.deadline_ms);
    let mut outcome = decide::decide_game_at_cutoff(m, subs, now, cutoff, setup.penalty_seconds);

    // Past the contest end nobody can submit, so an undecided game cannot be
    // won any more. A verdict still in flight may yet settle it (wait for it
    // exactly as at a deadline); otherwise it goes to staff rather than being
    // scored as if both players had simply failed.
    if outcome == XiaojuOutcome::NotYet && clock.contest_ended() {
        match decide::oldest_in_flight(subs) {
            Some(blocking_submission_id) => {
                outcome = XiaojuOutcome::AwaitingJudge {
                    blocking_submission_id,
                }
            }
            None => {
                adjudicate(m, Adjudication::ContestEnded);
                return Ok(());
            }
        }
    }

    match outcome {
        XiaojuOutcome::NotYet => {
            m.phase = Phase::Playing;
            wake(wakeups, MatchTimer::Deadline, cutoff);
            wakeups.extend(effective_wakeups(m, inputs, index, cutoff));
        }
        XiaojuOutcome::AwaitingJudge {
            blocking_submission_id,
        } => {
            // The grace runs from the first time the game blocked; a later
            // tick never pushes it further out.
            let escalate_at_ms = match m.phase {
                Phase::AwaitingJudge { escalate_at_ms, .. } => escalate_at_ms,
                _ => setup
                    .escalation_grace_seconds
                    .checked_mul(1_000)
                    .and_then(|grace| now.checked_add(grace))
                    .ok_or(Rejection::TimeOverflow("judge wait deadline"))?,
            };
            if now >= escalate_at_ms {
                adjudicate(
                    m,
                    Adjudication::StuckJudge {
                        submission_id: blocking_submission_id,
                    },
                );
            } else {
                m.phase = Phase::AwaitingJudge {
                    blocking_submission_id,
                    escalate_at_ms,
                };
                wake(wakeups, MatchTimer::JudgeWait, escalate_at_ms);
            }
        }
        XiaojuOutcome::Decided { .. } => match decide::match_outcome(m, &round_def) {
            Some(MatchOutcome::Won(winner)) => {
                m.phase = Phase::Decided {
                    winner,
                    decided_at_ms: now,
                }
            }
            Some(MatchOutcome::TiebreaksExhausted) => {
                adjudicate(m, Adjudication::TiebreakExhausted)
            }
            Some(MatchOutcome::NextGame) | None if clock.contest_ended() => {
                adjudicate(m, Adjudication::ContestEnded)
            }
            Some(MatchOutcome::NextGame) | None => {
                let seconds = round_def.game_break();
                if seconds == 0 {
                    open_game(m, inputs, now, wakeups)?;
                } else {
                    let at = seconds
                        .checked_mul(1_000)
                        .and_then(|duration| now.checked_add(duration))
                        .ok_or(Rejection::TimeOverflow("intermission deadline"))?;
                    m.phase = Phase::Intermission {
                        next_game_at_ms: at,
                    };
                    wake(wakeups, MatchTimer::Intermission, at);
                }
            }
        },
    }
    Ok(())
}

/// Wakeups for accepted submissions whose score time has not been reached:
/// once it is, no new opponent submission can beat them.
fn effective_wakeups(m: &MatchState, inputs: &Inputs<'_>, game: u8, cutoff: i64) -> Vec<Wakeup> {
    let Some(current) = m.xiaoju.last() else {
        return Vec::new();
    };
    let cutoff_us = cutoff.saturating_mul(1_000);
    inputs
        .subs
        .iter()
        .filter(|s| {
            current.contains_submission_at_us(s.submitted_at_us)
                && s.submitted_at_us < cutoff_us
                && !decide::submission_in_flight(s)
                && s.verdict.as_ref().is_some_and(Verdict::is_accepted)
        })
        .filter_map(|sub| {
            let at_us =
                decide::ac_settlement_at_us(m, inputs.subs, sub, inputs.setup.penalty_seconds);
            let at_ms = at_us.saturating_add(999) / 1_000;
            (at_ms > inputs.clock.now && at_us <= cutoff_us).then_some(Wakeup {
                timer: MatchTimer::Effective {
                    game,
                    submission_id: sub.submission_id,
                    at_us: (at_us != sub.effective_at_us).then_some(at_us),
                },
                at_ms,
            })
        })
        .collect()
}

/// Open the next game (regular or 附加赛 -- both live in `m.xiaoju`) at
/// `opened_at_ms` and wake at its deadline.
///
/// The first game pins this stage's regular and tiebreak durations and the
/// shared extra-time allowance. Later setup corrections affect only matches
/// that have not started yet.
fn open_game(
    m: &mut MatchState,
    inputs: &Inputs<'_>,
    opened_at_ms: i64,
    wakeups: &mut Vec<Wakeup>,
) -> Result<(), Rejection> {
    let setup = inputs.setup;
    let Some(round) = setup.round_for_match(m.round, m.pos) else {
        adjudicate(m, Adjudication::SetupMissing);
        return Ok(());
    };
    // A zero balance may mean exhausted, not uninitialized, so only a match
    // with no opened games receives its allowance.
    if m.xiaoju.is_empty() {
        m.match_extra_time_seconds = round
            .match_extra_time_seconds
            .unwrap_or(setup.match_extra_time_seconds);
        m.extra_time_remaining_ms = m
            .match_extra_time_seconds
            .checked_mul(1_000)
            .ok_or(Rejection::TimeOverflow("extra time"))?;
    }
    let durations = *m.game_durations.get_or_insert(round.game_durations());
    let index = u8::try_from(m.xiaoju.len()).map_err(|_| Rejection::TimeOverflow("game index"))?;
    let regular = m.is_regular_game(index);
    let seconds = if regular {
        durations.xiaoju_seconds
    } else {
        durations.tiebreak_seconds
    };
    let base_deadline_ms = seconds
        .checked_mul(1_000)
        .and_then(|duration| opened_at_ms.checked_add(duration))
        .ok_or(Rejection::TimeOverflow("game deadline"))?;
    let extra_ms = if regular {
        m.extra_time_remaining_ms
    } else {
        0
    };
    let deadline_ms = base_deadline_ms
        .checked_add(extra_ms)
        .ok_or(Rejection::TimeOverflow("game deadline"))?;
    m.xiaoju.push(XiaojuState {
        index,
        opened_at_ms,
        base_deadline_ms,
        deadline_ms,
        extra_time_used_ms: 0,
        winner: None,
        decided: false,
    });
    m.phase = Phase::Playing;
    wake(
        wakeups,
        MatchTimer::Deadline,
        inputs.clock.cutoff(deadline_ms),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decide::SubmissionLifecycle;
    use crate::model::{GameKind, StageClock};
    use crate::test_support::setup_two_rounds;

    const A: i32 = 10;
    const B: i32 = 20;

    fn inputs<'a>(setup: &'a Setup, clock: Clock, subs: &'a [SubmissionRecord]) -> Inputs<'a> {
        Inputs {
            setup,
            clock,
            subs,
            start_at_ms: None,
        }
    }

    fn tick_at(m: &MatchState, setup: &Setup, now: i64, subs: &[SubmissionRecord]) -> Transition {
        transition(m, Event::Tick, &inputs(setup, Clock::at(now), subs)).unwrap()
    }

    fn ordering() -> MatchState {
        MatchState {
            round: 1,
            player_a: A,
            player_b: B,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            ..Default::default()
        }
    }

    /// `ordering()` started at `now` by staff.
    fn started(setup: &Setup, now: i64) -> MatchState {
        transition(
            &ordering(),
            Event::Start,
            &inputs(setup, Clock::at(now), &[]),
        )
        .unwrap()
        .next
    }

    fn submission(id: i32, user: i32, at_ms: i64, verdict: Option<Verdict>) -> SubmissionRecord {
        SubmissionRecord {
            submission_id: id,
            user_id: user,
            problem_id: 0,
            submitted_at_us: at_ms * 1_000,
            effective_at_us: at_ms * 1_000,
            status: if verdict.is_some() {
                SubmissionLifecycle::Judged
            } else {
                SubmissionLifecycle::Running
            },
            verdict,
        }
    }

    fn ac(id: i32, user: i32, at_ms: i64) -> SubmissionRecord {
        submission(id, user, at_ms, Some(Verdict::Accepted))
    }

    fn deadline_of(m: &MatchState) -> i64 {
        m.xiaoju.last().unwrap().deadline_ms
    }

    fn wakeup(t: &Transition, timer: MatchTimer) -> Option<i64> {
        t.wakeups.iter().find(|w| w.timer == timer).map(|w| w.at_ms)
    }

    /// Settle the open game for `winner` (or nobody) at its deadline.
    fn settle(m: &MatchState, setup: &Setup, winner: Option<i32>) -> Transition {
        let at = deadline_of(m);
        let subs: Vec<_> = winner.map(|w| ac(1, w, at - 1)).into_iter().collect();
        tick_at(m, setup, at, &subs)
    }

    #[test]
    fn a_staff_start_fills_a_missing_ranking_and_opens_the_first_game() {
        let setup = setup_two_rounds();
        let mut m = ordering();
        m.order_b = None;
        let t = transition(&m, Event::Start, &inputs(&setup, Clock::at(1_000), &[])).unwrap();
        assert_eq!(t.next.phase, Phase::Playing);
        assert_eq!(t.next.order_b, Some(m.group_b.clone()));
        assert_eq!(t.next.order_a, m.order_a, "a real ranking is kept");
        assert_eq!(t.next.xiaoju[0].opened_at_ms, 1_000);
        assert_eq!(wakeup(&t, MatchTimer::Deadline), Some(1_801_000));
        assert_eq!(
            t.effects,
            [Effect::PublishFeedback, Effect::Cancel(MatchTimer::Start)]
        );
    }

    #[test]
    fn a_staff_start_is_refused_outside_ordering_and_for_scheduled_stages() {
        let mut setup = setup_two_rounds();
        let playing = started(&setup, 0);
        let start = |m: &MatchState, setup: &Setup| {
            transition(m, Event::Start, &inputs(setup, Clock::at(0), &[]))
        };
        assert_eq!(start(&playing, &setup), Err(Rejection::NotAwaitingStart));
        setup.rounds[0].start_at_ms = Some(0);
        assert_eq!(start(&ordering(), &setup), Err(Rejection::NotAwaitingStart));
    }

    #[test]
    fn an_unscheduled_match_waits_for_its_start_time_and_never_starts_at_the_contest_end() {
        let setup = setup_two_rounds();
        let at = |now: i64, start: Option<i64>, end: Option<i64>| {
            let inputs = Inputs {
                start_at_ms: start,
                clock: Clock {
                    now,
                    contest_end: end,
                },
                ..inputs(&setup, Clock::at(now), &[])
            };
            transition(&ordering(), Event::Tick, &inputs).unwrap()
        };
        let waiting = at(5_000, Some(60_000), None);
        assert_eq!(waiting.next, ordering());
        assert_eq!(wakeup(&waiting, MatchTimer::Start), Some(60_000));
        let due = at(60_000, Some(60_000), None);
        assert_eq!(due.next.phase, Phase::Playing);
        assert_eq!(due.next.xiaoju[0].opened_at_ms, 60_000);
        for unplayable in [
            at(5_000, None, None),
            at(5_000, Some(60_000), Some(60_000)),
            at(70_000, Some(60_000), Some(60_000)),
        ] {
            assert_eq!(unplayable.next, ordering());
            assert!(unplayable.wakeups.is_empty());
        }
    }

    fn scheduled_setup() -> Setup {
        let mut setup = setup_two_rounds();
        setup.rounds[0].start_at_ms = Some(0);
        setup.stage_clocks.insert(
            1,
            StageClock {
                start_at_ms: 0,
                ordering_deadline_ms: 180_000,
                starts_at_ms: 210_000,
                awaiting_reschedule: false,
            },
        );
        setup
    }

    #[test]
    fn a_scheduled_stage_keeps_its_full_ordering_window_and_shared_first_game_clock() {
        let setup = scheduled_setup();
        let mut m = ordering();
        m.order_b = None;

        let ranking = tick_at(&m, &setup, 100_000, &[]);
        assert_eq!(
            ranking.next, m,
            "both rankings arriving early start nothing"
        );
        assert_eq!(wakeup(&ranking, MatchTimer::OrderingClose), Some(180_000));

        let preparing = tick_at(&m, &setup, 180_000, &[]);
        assert_eq!(preparing.next.phase, Phase::Preparing);
        assert_eq!(preparing.next.order_b, Some(m.group_b.clone()));
        assert_eq!(wakeup(&preparing, MatchTimer::GameStart), Some(210_000));
        assert_eq!(
            preparing.effects,
            [Effect::Cancel(MatchTimer::OrderingClose)]
        );

        // A late wakeup still opens the game on the shared clock.
        let playing = tick_at(&preparing.next, &setup, 215_000, &[]);
        assert_eq!(playing.next.phase, Phase::Playing);
        assert_eq!(playing.next.xiaoju[0].opened_at_ms, 210_000);
        assert!(
            playing
                .effects
                .contains(&Effect::Cancel(MatchTimer::GameStart))
        );
    }

    #[test]
    fn a_scheduled_match_waits_while_its_stage_needs_rescheduling() {
        let mut setup = scheduled_setup();
        setup.stage_clocks.get_mut(&1).unwrap().awaiting_reschedule = true;
        let t = tick_at(&ordering(), &setup, 500_000, &[]);
        assert_eq!(t.next, ordering());
        assert!(t.wakeups.is_empty());
    }

    #[test]
    fn an_open_game_wakes_at_its_deadline_or_the_contest_end_if_that_is_earlier() {
        let setup = setup_two_rounds();
        let m = started(&setup, 0);
        let t = tick_at(&m, &setup, 1_000, &[]);
        assert_eq!(t.next, m);
        assert_eq!(wakeup(&t, MatchTimer::Deadline), Some(1_800_000));
        assert_eq!(t.effects, [Effect::PublishFeedback]);
        let ending = Clock {
            now: 1_000,
            contest_end: Some(900_000),
        };
        let t = transition(&m, Event::Tick, &inputs(&setup, ending, &[])).unwrap();
        assert_eq!(wakeup(&t, MatchTimer::Deadline), Some(900_000));
    }

    #[test]
    fn ticking_twice_with_the_same_evidence_changes_nothing_more() {
        let setup = setup_two_rounds();
        let m = started(&setup, 0);
        let subs = [ac(1, A, 10_000)];
        let once = tick_at(&m, &setup, 40_000, &subs);
        assert_eq!(once.next.score_a, 1);
        let twice = tick_at(&once.next, &setup, 40_000, &subs);
        assert_eq!(twice.next, once.next);
    }

    #[test]
    fn a_scoreless_deadline_opens_the_next_game_under_the_same_wakeup() {
        let setup = setup_two_rounds();
        let m = started(&setup, 0);
        let t = settle(&m, &setup, None);
        assert_eq!((t.next.score_a, t.next.score_b), (0, 0));
        assert_eq!(t.next.xiaoju.len(), 2);
        assert_eq!(t.next.xiaoju[1].opened_at_ms, 1_800_000);
        assert_eq!(wakeup(&t, MatchTimer::Deadline), Some(3_600_000));
        assert_eq!(t.effects, [Effect::PublishFeedback]);
    }

    #[test]
    fn shared_extra_time_carries_across_games_and_ignores_later_setup_corrections() {
        let mut setup = setup_two_rounds();
        setup.rounds[0].xiaoju_seconds = 10;
        setup.match_extra_time_seconds = 5;
        let m = started(&setup, 1_000);
        assert_eq!(m.xiaoju[0].base_deadline_ms, 11_000);
        assert_eq!(m.xiaoju[0].deadline_ms, 16_000);
        assert_eq!(tick_at(&m, &setup, 11_000, &[]).next, m);

        let after = tick_at(&m, &setup, 14_000, &[ac(1, A, 13_000)]).next;
        assert_eq!(after.score_a, 1);
        assert_eq!(after.extra_time_remaining_ms, 3_000);
        assert_eq!(after.xiaoju[0].extra_time_used_ms, 2_000);
        assert_eq!(after.xiaoju[1].deadline_ms, 27_000);

        // The old AC cannot settle the newly opened game.
        assert_eq!(
            tick_at(&after, &setup, 14_500, &[ac(1, A, 13_000)]).next,
            after
        );

        setup.rounds[0].xiaoju_seconds = 100;
        setup.match_extra_time_seconds = 100;
        let after = tick_at(&after, &setup, 27_000, &[]).next;
        assert_eq!(after.extra_time_remaining_ms, 0);
        assert_eq!(after.match_extra_time_seconds, 5);
        assert_eq!(after.xiaoju[1].extra_time_used_ms, 3_000);
        assert_eq!(after.xiaoju[2].base_deadline_ms, 37_000);
        assert_eq!(after.xiaoju[2].deadline_ms, 37_000);
    }

    #[test]
    fn a_first_timeout_exhausts_the_pool_and_tiebreaks_get_only_base_time() {
        let mut setup = setup_two_rounds();
        setup.rounds[0].xiaoju_seconds = 10;
        setup.rounds[0].tiebreak_seconds = 20;
        setup.match_extra_time_seconds = 5;
        let mut m = started(&setup, 0);
        for next_deadline in [25_000, 35_000, 55_000] {
            m = settle(&m, &setup, None).next;
            assert_eq!((m.score_a, m.score_b, m.extra_time_remaining_ms), (0, 0, 0));
            assert_eq!(deadline_of(&m), next_deadline);
        }
        assert_eq!(m.phase_name(), crate::model::PhaseName::Tiebreak);
        assert_eq!(m.xiaoju[0].extra_time_used_ms, 5_000);
        assert_eq!(m.xiaoju[3].base_deadline_ms, m.xiaoju[3].deadline_ms);
    }

    #[test]
    fn reaching_the_win_target_decides_the_match_and_closes_it() {
        let setup = setup_two_rounds();
        let mut m = started(&setup, 0);
        m = settle(&m, &setup, Some(B)).next;
        let t = settle(&m, &setup, Some(B));
        let now = deadline_of(&m);
        assert_eq!(
            t.next.phase,
            Phase::Decided {
                winner: B,
                decided_at_ms: now
            }
        );
        assert_eq!(t.next.xiaoju.len(), 2, "no third game opens");
        assert_eq!(
            t.wakeups,
            [Wakeup {
                timer: MatchTimer::Closing,
                at_ms: now + CLOSING_RETRY_MS
            }]
        );
        assert_eq!(
            t.effects,
            [
                Effect::Cancel(MatchTimer::Deadline),
                Effect::PublishFeedback,
                Effect::AdvanceBracket { winner: B },
                Effect::Cancel(MatchTimer::Closing),
            ]
        );
        // Re-evaluating the closed match repeats only its closing work.
        let again = tick_at(&t.next, &setup, now + 5_000, &[]);
        assert_eq!(again.next, t.next);
        assert_eq!(
            again.effects,
            [
                Effect::PublishFeedback,
                Effect::AdvanceBracket { winner: B },
                Effect::Cancel(MatchTimer::Closing),
            ]
        );
    }

    #[test]
    fn a_level_score_plays_tiebreaks_on_new_problems_until_one_is_won() {
        let mut setup = setup_two_rounds();
        setup.rounds[0].tiebreak_seconds = 60;
        let mut m = started(&setup, 0);
        for winner in [Some(A), Some(B), None] {
            m = settle(&m, &setup, winner).next;
        }
        assert_eq!(m.game_kind(3), GameKind::Tiebreak { attempt: 0 });
        assert_eq!(m.current_problem(&setup.rounds[0], A), Some(301));
        assert_eq!(deadline_of(&m) - m.xiaoju[3].opened_at_ms, 60_000);
        m = settle(&m, &setup, None).next;
        assert_eq!(m.current_problem(&setup.rounds[0], B), Some(302));
        let t = settle(&m, &setup, Some(A));
        assert_eq!(t.next.winner(), Some(A));
    }

    #[test]
    fn exhausting_the_tiebreak_problems_goes_to_staff() {
        let setup = setup_two_rounds();
        let mut m = started(&setup, 0);
        for _ in 0..4 {
            m = settle(&m, &setup, None).next;
        }
        let t = settle(&m, &setup, None);
        assert_eq!(
            t.next.phase,
            Phase::NeedsAdjudication {
                adjudication: Adjudication::TiebreakExhausted
            }
        );
        assert!(t.effects.contains(&Effect::Cancel(MatchTimer::Closing)));
        assert!(
            !t.effects
                .iter()
                .any(|e| matches!(e, Effect::AdvanceBracket { .. }))
        );
    }

    #[test]
    fn a_game_break_runs_from_settlement_and_the_next_game_opens_at_its_end() {
        let mut setup = setup_two_rounds();
        setup.rounds[0].game_break_seconds = Some(60);
        let m = started(&setup, 0);
        let t = tick_at(&m, &setup, 40_000, &[ac(1, A, 10_000)]);
        assert_eq!(
            t.next.phase,
            Phase::Intermission {
                next_game_at_ms: 100_000
            }
        );
        assert_eq!(wakeup(&t, MatchTimer::Intermission), Some(100_000));
        assert_eq!(
            t.effects,
            [
                Effect::PublishFeedback,
                Effect::Cancel(MatchTimer::Deadline)
            ]
        );
        let early = tick_at(&t.next, &setup, 99_999, &[]);
        assert_eq!(early.next, t.next);
        let late = tick_at(&t.next, &setup, 130_000, &[]);
        assert_eq!(late.next.phase, Phase::Playing);
        assert_eq!(late.next.xiaoju[1].opened_at_ms, 100_000);
        assert_eq!(
            late.effects,
            [
                Effect::PublishFeedback,
                Effect::Cancel(MatchTimer::Intermission)
            ]
        );
    }

    #[test]
    fn the_contest_end_sends_an_unfinished_match_to_staff_but_waits_for_verdicts() {
        let mut setup = setup_two_rounds();
        setup.rounds[0].game_break_seconds = Some(60);
        let ended = Clock {
            now: 50_000,
            contest_end: Some(50_000),
        };
        let at_end = |m: &MatchState, subs: &[SubmissionRecord]| {
            transition(m, Event::Tick, &inputs(&setup, ended, subs))
                .unwrap()
                .next
        };
        let contest_ended = Phase::NeedsAdjudication {
            adjudication: Adjudication::ContestEnded,
        };
        let m = started(&setup, 0);
        assert_eq!(at_end(&m, &[]).phase, contest_ended);

        let waiting = at_end(&m, &[submission(5, A, 40_000, None)]);
        assert_eq!(waiting.phase_name(), crate::model::PhaseName::AwaitingJudge);
        assert_eq!(waiting.blocking_submission_id(), Some(5));

        let won = at_end(&m, &[ac(1, A, 40_000)]);
        assert_eq!(won.score_a, 1, "a game won before the end still counts");
        assert_eq!(won.phase, contest_ended);
        assert_eq!(won.xiaoju.len(), 1);

        let mut breaking = m.clone();
        breaking.phase = Phase::Intermission {
            next_game_at_ms: 90_000,
        };
        assert_eq!(at_end(&breaking, &[]).phase, contest_ended);
        assert_eq!(at_end(&ordering(), &[]), ordering(), "never started");
    }

    #[test]
    fn a_blocked_deadline_waits_for_the_judge_then_escalates_once_its_grace_runs_out() {
        let setup = setup_two_rounds();
        let m = started(&setup, 0);
        let deadline = deadline_of(&m);
        let subs = [submission(5, B, 5_000, None), ac(6, A, 9_000)];

        let before = tick_at(&m, &setup, deadline - 1, &subs);
        assert_eq!(
            before.next, m,
            "an older blocker only waits before the deadline"
        );

        let blocked = tick_at(&m, &setup, deadline, &subs);
        let escalate_at = deadline + 120_000;
        assert_eq!(
            blocked.next.phase,
            Phase::AwaitingJudge {
                blocking_submission_id: 5,
                escalate_at_ms: escalate_at
            }
        );
        assert_eq!(wakeup(&blocked, MatchTimer::JudgeWait), Some(escalate_at));
        assert_eq!(
            blocked.effects,
            [
                Effect::PublishFeedback,
                Effect::Cancel(MatchTimer::Deadline)
            ]
        );

        // A later tick never pushes the grace further out.
        let still = tick_at(&blocked.next, &setup, deadline + 60_000, &subs);
        assert_eq!(still.next, blocked.next);
        assert_eq!(wakeup(&still, MatchTimer::JudgeWait), Some(escalate_at));

        let stuck = tick_at(&blocked.next, &setup, escalate_at, &subs);
        assert_eq!(
            stuck.next.phase,
            Phase::NeedsAdjudication {
                adjudication: Adjudication::StuckJudge { submission_id: 5 }
            }
        );
        assert_eq!(stuck.next.blocking_submission_id(), Some(5));
        assert!(
            stuck
                .effects
                .contains(&Effect::Cancel(MatchTimer::JudgeWait))
        );
    }

    #[test]
    fn a_verdict_landing_while_awaiting_the_judge_lets_the_earlier_submitter_win() {
        let setup = setup_two_rounds();
        let m = started(&setup, 0);
        let deadline = deadline_of(&m);
        let blocked = tick_at(
            &m,
            &setup,
            deadline,
            &[submission(5, B, 5_000, None), ac(6, A, 9_000)],
        )
        .next;
        let t = tick_at(
            &blocked,
            &setup,
            deadline + 1_000,
            &[ac(5, B, 5_000), ac(6, A, 9_000)],
        );
        assert_eq!(t.next.score_b, 1);
        assert_eq!(t.next.phase, Phase::Playing);
        assert_eq!(t.next.xiaoju.len(), 2);
        assert!(t.effects.contains(&Effect::Cancel(MatchTimer::JudgeWait)));
    }

    #[test]
    fn a_blocked_tiebreak_resumes_as_a_tiebreak() {
        let setup = setup_two_rounds();
        let mut m = started(&setup, 0);
        for winner in [Some(A), Some(B), None] {
            m = settle(&m, &setup, winner).next;
        }
        let deadline = deadline_of(&m);
        let blocked = tick_at(
            &m,
            &setup,
            deadline,
            &[submission(5, A, deadline - 10, None)],
        )
        .next;
        assert_eq!(blocked.current_problem(&setup.rounds[0], A), Some(301));
        let resumed = tick_at(
            &blocked,
            &setup,
            deadline + 1,
            &[submission(5, A, deadline - 10, Some(Verdict::WrongAnswer))],
        )
        .next;
        assert_eq!(resumed.phase_name(), crate::model::PhaseName::Tiebreak);
        assert_eq!(resumed.current_problem(&setup.rounds[0], A), Some(302));
    }

    #[test]
    fn a_future_effective_ac_wakes_at_its_score_time_and_names_an_earlier_settlement() {
        let mut setup = setup_two_rounds();
        setup.penalty_seconds = 20;
        let m = started(&setup, 0);
        let mut late = ac(7, A, 10_000);
        late.effective_at_us = 50_000_000;
        let t = tick_at(&m, &setup, 20_000, &[late.clone()]);
        assert_eq!(t.next, m, "an AC is not settled before its score time");
        assert_eq!(
            wakeup(
                &t,
                MatchTimer::Effective {
                    game: 0,
                    submission_id: 7,
                    at_us: None
                }
            ),
            Some(50_000)
        );
        // An opponent error moves settlement earlier, under its own key.
        let error = submission(8, B, 15_000, Some(Verdict::WrongAnswer));
        let t = tick_at(&m, &setup, 20_000, &[late, error]);
        assert_eq!(
            wakeup(
                &t,
                MatchTimer::Effective {
                    game: 0,
                    submission_id: 7,
                    at_us: Some(30_000_000)
                }
            ),
            Some(30_000)
        );
    }

    #[test]
    fn an_award_closes_the_match_from_any_phase_but_never_overturns_a_decision() {
        let setup = setup_two_rounds();
        let award = |m: &MatchState, winner: i32| {
            transition(
                m,
                Event::Award { winner },
                &inputs(&setup, Clock::at(9_000), &[]),
            )
        };
        assert_eq!(award(&ordering(), 99), Err(Rejection::NotParticipant(99)));
        let m = started(&setup, 0);
        let blocked = tick_at(
            &m,
            &setup,
            deadline_of(&m),
            &[submission(5, B, 5_000, None)],
        )
        .next;
        let t = award(&blocked, A).unwrap();
        assert_eq!(
            t.next.phase,
            Phase::Decided {
                winner: A,
                decided_at_ms: 9_000
            }
        );
        assert_eq!(t.next.blocking_submission_id(), None);
        assert_eq!(t.effects[0], Effect::Cancel(MatchTimer::JudgeWait));
        assert_eq!(award(&t.next, B), Err(Rejection::AlreadyDecided));

        let from_ordering = award(&ordering(), B).unwrap();
        assert_eq!(
            from_ordering.effects,
            [
                Effect::Cancel(MatchTimer::Start),
                Effect::AdvanceBracket { winner: B },
                Effect::Cancel(MatchTimer::Closing),
            ]
        );
    }

    #[test]
    fn a_missing_round_definition_goes_to_staff() {
        let setup = setup_two_rounds();
        let mut m = started(&setup, 0);
        m.round = 9;
        assert_eq!(
            tick_at(&m, &setup, 0, &[]).next.phase,
            Phase::NeedsAdjudication {
                adjudication: Adjudication::SetupMissing
            }
        );
    }
}
