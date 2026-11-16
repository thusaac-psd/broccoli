//! Scoring: deciding one 小局 from its submissions, and what a match's
//! settled games mean for the match.
//!
//! Judging completion, a deadline timer and staff can all re-evaluate the
//! same match at unpredictable times and in any order, and timer delivery is
//! at-least-once. [`decide_xiaoju`] therefore records a game's result at most
//! once and reports the stored result on every later call, and
//! [`match_outcome`] is a pure reading of the settled games.

use broccoli_server_sdk::prelude::Verdict;

use crate::model::{GameKind, MatchState, RoundDef};

pub use broccoli_server_sdk::types::SubmissionLifecycle;

/// Whether a submission in this status is still IN FLIGHT (could yet
/// receive a different verdict) as far as the bracket's scoring policy is
/// concerned.
///
/// This is deliberately NOT the same as `packages/common::SubmissionStatus
/// ::is_terminal()`: that method's idea of "terminal" is a host-wide,
/// storage-lifecycle concept, where `SystemError` counts as terminal
/// (judging finished, the submission's row is in a resting state). The
/// bracket's scoring policy needs the OPPOSITE answer for `SystemError`: the
/// platform aggressively re-judges these (`max_system_error_retries`,
/// "never abandon a system fault" --
/// `packages/server/src/dispatcher/system_error_retry.rs`), so a
/// `SystemError` submission can still turn into a real verdict later and
/// must keep blocking a later AC, exactly like `Queued`/`Pending`/
/// `Compiling`/`Running`. Only `Judged` and `CompilationError` are actually
/// done as far as this decision is concerned.
fn is_in_flight(status: &SubmissionLifecycle) -> bool {
    !matches!(
        status,
        SubmissionLifecycle::Judged | SubmissionLifecycle::CompilationError
    )
}

/// Effective submission time (arrival + the player's accumulated penalty)
/// in microseconds, then the smaller submission id on an exact tie.
/// Without the id, a same-instant tie was broken by iteration order.
fn arrival_order(s: &SubmissionRecord) -> (i64, i32) {
    (s.effective_at_us, s.submission_id)
}

/// Whether `s` could still turn into a different outcome, so the 小局 must
/// wait for it: either its lifecycle `status` is still in flight (see
/// [`is_in_flight`]), or judging finished with a `SystemError` VERDICT.
///
/// The second arm is the shape the platform actually produces for a sandbox
/// fault: plugin-finalized as `status == Judged` with `verdict ==
/// SystemError`, which the SystemError-retry reaper selects and re-judges
/// exactly like `status == SystemError`. Classifying by `status` alone called
/// it terminal. Measured on a real 4-worker stack under a concurrent burst:
/// correct solutions came back `Judged/SystemError` and lost their 小局 - to
/// the opponent's later AC, or 0-0 at the deadline - purely to a platform
/// fault. A SystemError verdict is never the contestant's outcome.
pub(crate) fn submission_in_flight(s: &SubmissionRecord) -> bool {
    is_in_flight(&s.status) || matches!(s.verdict, Some(Verdict::SystemError))
}

/// The oldest submission still being judged, if any.
pub fn oldest_in_flight(subs: &[SubmissionRecord]) -> Option<i32> {
    subs.iter()
        .filter(|s| submission_in_flight(s))
        .min_by_key(|s| arrival_order(s))
        .map(|s| s.submission_id)
}

/// One submission, as `decide_xiaoju` needs to see it: which player made it,
/// to which problem, when it was SUBMITTED (not judged -- see
/// [`decide_xiaoju`]'s doc comment for why that distinction matters), its
/// verdict if judging has completed (`None` while still pending or while a
/// system fault is being retried -- see `is_in_flight`), and its host
/// submission id (surfaced to staff via [`XiaojuOutcome::AwaitingJudge`] so
/// they know which submission to rejudge).
#[derive(Debug, Clone)]
pub struct SubmissionRecord {
    pub submission_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    pub submitted_at_us: i64,
    pub effective_at_us: i64,
    pub verdict: Option<Verdict>,
    pub status: SubmissionLifecycle,
}

/// Once this real time is reached, a new opponent submission cannot beat
/// `ac`. Already admitted submissions still need their own in-flight check.
/// Use the whole game's errors, including those before shared extra time.
pub(crate) fn ac_settlement_at_us(
    m: &MatchState,
    subs: &[SubmissionRecord],
    ac: &SubmissionRecord,
    penalty_seconds: i64,
) -> i64 {
    let opponent = if ac.user_id == m.player_a {
        m.player_b
    } else {
        m.player_a
    };
    let penalty_count = subs
        .iter()
        .filter(|s| {
            s.user_id == opponent
                && crate::feedback::counts_as_penalty(s)
                && m.xiaoju
                    .last()
                    .is_some_and(|game| game.contains_submission_at_us(s.submitted_at_us))
        })
        .count() as i64;
    let delay = crate::feedback::score_delay_seconds(m, penalty_count, penalty_seconds);
    ac.effective_at_us
        .saturating_sub(delay.saturating_mul(1_000_000))
}

/// Outcome of deciding one 小局.
///
/// `Decided { winner: Option<i32> }` is deliberately a DIFFERENT shape from
/// [`MatchOutcome::Won`]: a 小局 can legitimately
/// end with NEITHER player scoring ("如果在规定时间内双方均未能通过当前题
/// 目，则该小局双方均不得分"), which this expresses as `Decided { winner:
/// None }`. A match, by contrast, always produces a winner or reaches
/// `NeedsAdjudication` -- never a scoreless terminal state. Do not
/// "harmonise" these two shapes; see [`crate::model::XiaojuState`]'s doc
/// comment for the same warning at the storage layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XiaojuOutcome {
    /// Neither player has won yet, and the deadline has not passed -- there
    /// is nothing to record. Also returned while an older submission is
    /// still in flight and could still beat the current best AC (see
    /// [`decide_xiaoju`]'s doc comment), as long as the deadline has not
    /// passed yet either.
    NotYet,
    Decided {
        winner: Option<i32>,
    },
    /// The deadline has passed, but an older submission is STILL in flight
    /// (see `is_in_flight`) and could still beat the current best AC --
    /// awarding the AC now could hand the 小局 to the wrong player once that
    /// verdict lands, but staying `NotYet` forever would let a platform
    /// fault cost a player the 小局 outright (it would never resolve without
    /// staff intervention). The contest owner's chosen policy: escalate
    /// visibly instead of either. `blocking_submission_id` names the
    /// in-flight submission staff should rejudge to unblock this.
    AwaitingJudge {
        blocking_submission_id: i32,
    },
}

/// Decide the currently open 小局 (`m.xiaoju.last()`) from the submissions
/// known so far. No-op (returns the stored outcome without re-scoring) if
/// that 小局 is already decided -- see the module doc comment on why that is
/// load-bearing, not a nicety.
///
/// # The winner is the earliest effective submission, independent of judge latency
///
/// Each score is fixed at admission: real submission time plus the configured
/// penalty for prior errors in this game. While submissions are open, an AC
/// waits until now plus the opponent's accumulated penalty reaches its score
/// time, so a future opponent submission cannot overtake it. Once
/// the real cutoff is reached, compare all timely submissions even if their
/// score times exceed that cutoff. Penalties never reduce the submission window.
///
/// An unresolved submission only blocks an AC with a later effective arrival.
/// After the deadline, such a blocker enters `AwaitingJudge`.
///
/// `cutoff_ms` is the game's deadline, or the contest end if that comes
/// first: submissions close then. Without an AC, an early contest-end cutoff
/// is left to the caller's staff adjudication rather than scored as a normal
/// timeout, since the game's own deadline has not passed.
///
/// Does not validate that `problem_id` is the player's actual current
/// problem -- that is `gate.rs`'s job (submissions to the wrong problem are
/// rejected before they exist at all); this function trusts every record it
/// is handed.
pub fn decide_xiaoju(
    m: &mut MatchState,
    subs: &[SubmissionRecord],
    now_ms: i64,
    cutoff_ms: i64,
    penalty_seconds: i64,
) -> XiaojuOutcome {
    let Some(current) = m.xiaoju.last() else {
        // Nothing open to decide. Callers only decide while a 小局 is open,
        // but there is no unsafe state to fall into here, so fail quiet
        // rather than panic.
        return XiaojuOutcome::NotYet;
    };
    if current.decided {
        // Idempotent no-op: report the stored outcome without touching the
        // score again. See the module doc comment on why this matters.
        return XiaojuOutcome::Decided {
            winner: current.winner,
        };
    }
    // Copy out what's needed before taking a mutable borrow of `m` below --
    // `current` is a shared borrow of `m.xiaoju`'s last element.
    let deadline_ms = current.deadline_ms;
    let base_deadline_ms = current.base_deadline_ms;
    let regular = m.is_regular_game(current.index);
    let eligible = subs.iter().filter(|s| {
        current.contains_submission_at_us(s.submitted_at_us)
            && i128::from(s.submitted_at_us) < i128::from(cutoff_ms) * 1_000
    });
    let best_ac = eligible
        .clone()
        .filter(|s| {
            !submission_in_flight(s) && s.verdict.as_ref().is_some_and(Verdict::is_accepted)
        })
        .min_by_key(|s| arrival_order(s));

    let (winner, ended_at_ms) = match best_ac {
        Some(ac) => {
            // A future submission ties or loses once now + the opponent's
            // penalty reaches this AC's score. A newly admitted submission has
            // a larger id on an exact tie; existing contenders are checked below.
            if now_ms < cutoff_ms
                && now_ms.saturating_mul(1_000) < ac_settlement_at_us(m, subs, ac, penalty_seconds)
            {
                return XiaojuOutcome::NotYet;
            }
            // The OLDEST in-flight submission older than the best AC: if it
            // is still blocking once the deadline passes, that is the one
            // staff should rejudge first -- it has been stuck the longest.
            let blocker = eligible
                .clone()
                .filter(|s| submission_in_flight(s) && arrival_order(s) < arrival_order(ac))
                .min_by_key(|s| arrival_order(s));
            if let Some(blocker) = blocker {
                if now_ms < deadline_ms {
                    return XiaojuOutcome::NotYet;
                }
                return XiaojuOutcome::AwaitingJudge {
                    blocking_submission_id: blocker.submission_id,
                };
            }
            (Some(ac.user_id), ac.submitted_at_us / 1_000)
        }
        None => {
            if now_ms < deadline_ms {
                return XiaojuOutcome::NotYet;
            }
            // The deadline has passed with no accepted submission ANYWHERE.
            // Before scoring this 小局 scoreless, check the same thing the
            // `Some(ac)` arm above checks: is a submission still in flight?
            //
            // This arm used to decide scoreless immediately, which made the
            // platform-fault policy only half-implemented. A player whose
            // opponent had an AC was protected; a player whose submission was
            // the ONLY one -- and was stuck -- was not, even though that
            // submission might yet come back Accepted and win outright. And
            // because deciding sets `decided = true`, the idempotency guard
            // meant a later Accepted verdict could never revisit it: the 小局
            // was lost permanently to a fault that was not the player's.
            //
            // A stuck submission is a platform fault and must never cost a
            // player the 小局 (see the spec's platform-fault section). Escalate
            // exactly as the sibling arm does, so staff can force a rejudge,
            // and let the escalation timer fall through to NeedsAdjudication
            // if it stays stuck.
            let blocker = eligible
                .filter(|s| submission_in_flight(s))
                .min_by_key(|s| arrival_order(s));
            if let Some(blocker) = blocker {
                return XiaojuOutcome::AwaitingJudge {
                    blocking_submission_id: blocker.submission_id,
                };
            }
            // 如果在规定时间内双方均未能通过当前题目，则该小局双方均不得分
            (None, deadline_ms)
        }
    };

    // Charge only when recording the result, under the caller's CAS update.
    // The decided guard above makes redelivery harmless, and the winning
    // actual submission time excludes both penalties and judging delays.
    let extra_used_ms = if regular {
        ended_at_ms
            .saturating_sub(base_deadline_ms)
            .clamp(0, m.extra_time_remaining_ms)
    } else {
        0
    };
    m.extra_time_remaining_ms -= extra_used_ms;
    if let Some(slot) = m.xiaoju.last_mut() {
        slot.winner = winner;
        slot.decided = true;
        slot.extra_time_used_ms = extra_used_ms;
    }
    match winner {
        Some(uid) if uid == m.player_a => m.score_a += 1,
        Some(uid) if uid == m.player_b => m.score_b += 1,
        _ => {}
    }

    XiaojuOutcome::Decided { winner }
}

/// What a match's settled games mean for the match.
///
/// `Won(i32)` is deliberately a DIFFERENT shape from
/// [`XiaojuOutcome::Decided`]'s `winner: Option<i32>`: a MATCH always ends
/// with a winner or goes to staff, never scorelessly -- even though an
/// individual 小局 or 附加赛 attempt can. Do not "harmonise" these two shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOutcome {
    /// Another game follows: the next regular game, or the next tiebreak
    /// attempt (see [`MatchState::game_kind`] for which).
    NextGame,
    Won(i32),
    /// Every tiebreak problem ended scoreless.
    TiebreaksExhausted,
}

/// Read a match's outcome from its games, or `None` while a game is still
/// unsettled (or none has been played).
///
/// Reaching the stage's FT target ends a match once the current game is
/// settled. Otherwise, after the BO game cap, the higher score wins. A LEVEL
/// score -- **including 0-0** -- goes to the round's first 附加赛 problem:
/// `if score_a == score_b && score_a > 0` would silently leave a 0-0 match
/// undecided forever instead (see `a_zero_zero_match_also_goes_to_a_tiebreak`).
///
/// A scoreless 附加赛 attempt does not replay the same problem; the next
/// attempt uses the NEXT problem in `RoundDef::tiebreak`'s order (see that
/// field's doc comment for why). Exhausting the list without a decision is
/// [`MatchOutcome::TiebreaksExhausted`] rather than a wait for a 附加赛
/// problem that does not exist.
pub fn match_outcome(m: &MatchState, round_def: &RoundDef) -> Option<MatchOutcome> {
    let last = m.xiaoju.last()?;
    if m.xiaoju.iter().any(|game| !game.decided) {
        return None;
    }
    if let GameKind::Tiebreak { attempt } = m.game_kind(last.index) {
        return Some(match last.winner {
            Some(winner) => MatchOutcome::Won(winner),
            None if attempt + 1 < round_def.tiebreak.len() => MatchOutcome::NextGame,
            None => MatchOutcome::TiebreaksExhausted,
        });
    }
    let target_reached = usize::from(m.score_a.max(m.score_b)) >= round_def.win_target();
    if !target_reached && m.xiaoju.len() < m.group_a.len() {
        return Some(MatchOutcome::NextGame);
    }
    Some(match m.score_a.cmp(&m.score_b) {
        std::cmp::Ordering::Greater => MatchOutcome::Won(m.player_a),
        std::cmp::Ordering::Less => MatchOutcome::Won(m.player_b),
        std::cmp::Ordering::Equal if round_def.tiebreak.is_empty() => {
            MatchOutcome::TiebreaksExhausted
        }
        std::cmp::Ordering::Equal => MatchOutcome::NextGame,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decide with the game's own deadline as the cutoff.
    fn at_deadline(
        m: &mut MatchState,
        subs: &[SubmissionRecord],
        now_ms: i64,
        penalty_seconds: i64,
    ) -> XiaojuOutcome {
        let cutoff_ms = m.xiaoju.last().map_or(now_ms, |game| game.deadline_ms);
        decide_xiaoju(m, subs, now_ms, cutoff_ms, penalty_seconds)
    }
    use crate::model::{Phase, Setup, XiaojuState};

    fn match_in_progress_at_xiaoju(index: u8) -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            xiaoju: vec![XiaojuState {
                index,
                opened_at_ms: 0,
                deadline_ms: i64::MAX,
                winner: None,
                decided: false,
                ..Default::default()
            }],
            phase: Phase::Playing,
            ..Default::default()
        }
    }

    fn match_in_progress_at_xiaoju_with_deadline(index: u8, deadline_ms: i64) -> MatchState {
        let mut m = match_in_progress_at_xiaoju(index);
        m.xiaoju[0].deadline_ms = deadline_ms;
        m
    }

    fn game_with_extra_time() -> MatchState {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 3_000);
        m.xiaoju[0].opened_at_ms = 1_000;
        m.xiaoju[0].base_deadline_ms = 2_000;
        m.extra_time_remaining_ms = 1_000;
        m
    }

    #[test]
    fn extra_time_is_charged_once_at_the_winning_submission_time() {
        for (submitted_at_us, used) in [
            (1_500_123, 0),
            (2_000_000, 0),
            (2_250_000, 250),
            (2_999_999, 999),
        ] {
            let mut m = game_with_extra_time();
            let subs = [sub(10, 103, submitted_at_us, Some(Verdict::Accepted))];
            // The verdict arrives long after the final deadline.
            let outcome = at_deadline(&mut m, &subs, 10_000, 20);
            assert_eq!(outcome, XiaojuOutcome::Decided { winner: Some(10) });
            assert_eq!(m.extra_time_remaining_ms, 1_000 - used);
            assert_eq!(m.xiaoju[0].extra_time_used_ms, used);
            let settled = m.clone();
            assert_eq!(at_deadline(&mut m, &subs, 20_000, 20), outcome);
            assert_eq!(m, settled, "a repeated callback must not charge twice");
        }
    }

    #[test]
    fn timeout_waits_for_eligible_judging_then_charges_only_the_actual_extra_time() {
        let mut m = game_with_extra_time();
        let mut subs = [sub(20, 203, 2_400_000, None)];
        assert!(matches!(
            at_deadline(&mut m, &subs, 3_000, 20),
            XiaojuOutcome::AwaitingJudge { .. }
        ));
        assert_eq!(m.extra_time_remaining_ms, 1_000);
        assert!(!m.xiaoju[0].decided);
        subs[0].verdict = Some(Verdict::Accepted);
        subs[0].status = SubmissionLifecycle::Judged;
        assert_eq!(
            at_deadline(&mut m, &subs, 8_000, 20),
            XiaojuOutcome::Decided { winner: Some(20) }
        );
        assert_eq!(m.extra_time_remaining_ms, 600);
    }

    #[test]
    fn neither_score_penalties_nor_judging_delays_spend_extra_time() {
        let mut m = game_with_extra_time();
        let mut accepted = sub(10, 103, 1_500_000, Some(Verdict::Accepted));
        accepted.effective_at_us = 2_500_000;
        let subs = [accepted];
        assert_eq!(at_deadline(&mut m, &subs, 2_400, 20), XiaojuOutcome::NotYet);
        assert_eq!(m.extra_time_remaining_ms, 1_000);
        assert_eq!(
            at_deadline(&mut m, &subs, 8_000, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
        assert_eq!(m.extra_time_remaining_ms, 1_000);
        assert_eq!(m.xiaoju[0].extra_time_used_ms, 0);
    }

    #[test]
    fn score_times_beyond_the_cutoff_can_win_without_consuming_penalty_time() {
        for effective_at_us in [3_000_000, 3_000_001, 20_000_000] {
            let mut m = game_with_extra_time();
            let mut accepted = sub(10, 103, 2_500_000, Some(Verdict::Accepted));
            accepted.effective_at_us = effective_at_us;
            assert_eq!(
                at_deadline(&mut m, &[accepted], 3_000, 20),
                XiaojuOutcome::Decided { winner: Some(10) }
            );
            assert_eq!(m.extra_time_remaining_ms, 500);
            assert_eq!(m.xiaoju[0].extra_time_used_ms, 500);
        }
    }

    #[test]
    fn contest_cutoff_can_settle_a_penalized_ac_without_waiting_for_its_score_time() {
        let mut m = game_with_extra_time();
        let mut accepted = sub(10, 103, 2_200_000, Some(Verdict::Accepted));
        accepted.effective_at_us = 20_000_000;
        assert_eq!(
            decide_xiaoju(&mut m, &[accepted], 2_500, 2_500, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
        assert_eq!(m.extra_time_remaining_ms, 800);
    }

    #[test]
    fn at_the_cutoff_compare_penalized_scores_even_when_both_exceed_the_window() {
        let mut m = game_with_extra_time();
        let mut a = sub(10, 103, 2_200_000, Some(Verdict::Accepted));
        a.effective_at_us = 11_000_000;
        let mut b = sub(20, 203, 2_500_000, Some(Verdict::Accepted));
        b.effective_at_us = 10_000_000;
        assert_eq!(
            at_deadline(&mut m, &[a, b], 3_000, 20),
            XiaojuOutcome::Decided { winner: Some(20) }
        );
        assert_eq!(m.extra_time_remaining_ms, 500);
    }

    #[test]
    fn out_of_window_submissions_neither_win_nor_block_settlement() {
        for verdict in [None, Some(Verdict::Accepted)] {
            let mut m = game_with_extra_time();
            let subs = [
                sub(10, 103, 999_999, verdict.clone()),
                sub(20, 203, 3_000_000, verdict),
            ];
            assert_eq!(
                at_deadline(&mut m, &subs, 3_000, 20),
                XiaojuOutcome::Decided { winner: None }
            );
            assert_eq!(m.extra_time_remaining_ms, 0);
            assert_eq!(m.xiaoju[0].extra_time_used_ms, 1_000);
        }
    }

    #[test]
    fn a_tiebreak_does_not_charge_the_regular_games_allowance() {
        let mut m = game_with_extra_time();
        m.xiaoju[0].index = 3;
        at_deadline(&mut m, &[], 3_000, 20);
        assert_eq!(m.extra_time_remaining_ms, 1_000);
        assert_eq!(m.xiaoju[0].extra_time_used_ms, 0);
    }

    fn sub(
        user_id: i32,
        problem_id: i32,
        submitted_at_us: i64,
        verdict: Option<Verdict>,
    ) -> SubmissionRecord {
        // `status` mirrors the verdict for these older tests, which predate
        // the in-flight/terminal distinction: `Some(verdict)` implies
        // judging finished (`Judged`), `None` implies still pending
        // (`Pending`) -- both in-flight-vs-terminal classifications that
        // agree with the old buggy `verdict.is_none()` check, so none of
        // these call sites need editing.
        let status = if verdict.is_some() {
            SubmissionLifecycle::Judged
        } else {
            SubmissionLifecycle::Pending
        };
        sub_with_status(0, user_id, problem_id, submitted_at_us, verdict, status)
    }

    fn sub_with_status(
        submission_id: i32,
        user_id: i32,
        problem_id: i32,
        submitted_at_us: i64,
        verdict: Option<Verdict>,
        status: SubmissionLifecycle,
    ) -> SubmissionRecord {
        SubmissionRecord {
            submission_id,
            user_id,
            problem_id,
            submitted_at_us,
            effective_at_us: submitted_at_us,
            verdict,
            status,
        }
    }

    #[test]
    fn a_penalized_ac_waits_and_an_opponent_can_still_beat_it() {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 600_000);
        let mut a = sub(10, 103, 250_000_000, Some(Verdict::Accepted)); // 04:10
        a.effective_at_us += 20_000_000; // 04:30
        assert_eq!(
            at_deadline(&mut m, &[a.clone()], 251_000, 20),
            XiaojuOutcome::NotYet
        );
        let b = sub(20, 203, 260_000_000, Some(Verdict::Accepted)); // 04:20
        assert_eq!(
            at_deadline(&mut m, &[a, b], 261_000, 20),
            XiaojuOutcome::Decided { winner: Some(20) }
        );
    }

    fn race_with_penalties(
        a_errors: usize,
        b_errors: usize,
        penalty_seconds: i64,
    ) -> (MatchState, Vec<SubmissionRecord>) {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 960_000);
        m.xiaoju[0].base_deadline_ms = 360_000;
        m.extra_time_remaining_ms = 600_000;
        let mut subs = Vec::new();
        for (user, problem, count) in [(10, 103, a_errors), (20, 203, b_errors)] {
            for _ in 0..count {
                let id = subs.len() as i32 + 1;
                subs.push(sub_with_status(
                    id,
                    user,
                    problem,
                    i64::from(id) * 1_000_000,
                    Some(Verdict::WrongAnswer),
                    SubmissionLifecycle::Judged,
                ));
            }
        }
        let mut ac = sub(10, 103, 390_000_000, Some(Verdict::Accepted));
        ac.submission_id = 100;
        ac.effective_at_us += a_errors as i64 * penalty_seconds * 1_000_000;
        subs.push(ac);
        (m, subs)
    }

    #[test]
    fn opponent_penalties_allow_immediate_settlement_in_shared_extra_time() {
        for winner in [10, 20] {
            let (mut m, mut subs) = race_with_penalties(1, 2, 20);
            // One B error is in base time, the other in shared extra time.
            subs[2].submitted_at_us = 380_000_000;
            subs[2].effective_at_us = 400_000_000;
            if winner == 20 {
                for sub in &mut subs {
                    sub.user_id = 30 - sub.user_id;
                }
            }
            assert_eq!(
                at_deadline(&mut m, &subs, 391_000, 20),
                XiaojuOutcome::Decided {
                    winner: Some(winner)
                }
            );
            assert_eq!(m.xiaoju[0].extra_time_used_ms, 30_000);
            assert_eq!(m.extra_time_remaining_ms, 570_000);
        }
    }

    #[test]
    fn settlement_waits_exactly_until_the_opponent_can_no_longer_overtake() {
        for penalty_seconds in [7, 20] {
            let (mut m, subs) = race_with_penalties(2, 1, penalty_seconds);
            let at_ms = 390_000 + penalty_seconds * 1_000;
            assert_eq!(
                at_deadline(&mut m, &subs, at_ms - 1, penalty_seconds),
                XiaojuOutcome::NotYet
            );
            assert_eq!(
                at_deadline(&mut m, &subs, at_ms, penalty_seconds),
                XiaojuOutcome::Decided { winner: Some(10) }
            );
        }
    }

    #[test]
    fn early_settlement_still_waits_for_existing_candidates_and_system_faults() {
        for effective_at_us in [390_000_000, 410_000_000] {
            for (status, verdict) in [
                (SubmissionLifecycle::Pending, None),
                (SubmissionLifecycle::SystemError, None),
                (SubmissionLifecycle::Judged, Some(Verdict::SystemError)),
            ] {
                let (mut m, mut subs) = race_with_penalties(1, 2, 20);
                let mut pending = sub_with_status(99, 20, 203, 350_000_000, verdict, status);
                pending.effective_at_us = effective_at_us;
                subs.push(pending);
                assert_eq!(
                    at_deadline(&mut m, &subs, 391_000, 20),
                    XiaojuOutcome::NotYet
                );
                let pending = subs.last_mut().unwrap();
                pending.status = SubmissionLifecycle::Judged;
                pending.verdict = Some(Verdict::Accepted);
                assert_eq!(
                    at_deadline(&mut m, &subs, 392_000, 20),
                    XiaojuOutcome::Decided { winner: Some(20) }
                );
            }
        }
    }

    #[test]
    fn a_pending_submission_that_cannot_beat_the_ac_does_not_prevent_early_settlement() {
        let (mut m, mut subs) = race_with_penalties(1, 2, 20);
        let mut pending = sub(20, 203, 380_000_000, None);
        pending.effective_at_us = 420_000_000;
        subs.push(pending);
        assert_eq!(
            at_deadline(&mut m, &subs, 391_000, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn both_acs_can_settle_before_either_effective_time() {
        let (mut m, mut subs) = race_with_penalties(2, 2, 20);
        let mut b = sub(20, 203, 400_000_000, Some(Verdict::Accepted));
        b.effective_at_us = 440_000_000;
        subs.push(b);
        assert_eq!(
            at_deadline(&mut m, &subs, 401_000, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn early_settlement_uses_only_current_game_terminal_errors() {
        let (m, mut subs) = race_with_penalties(1, 0, 20);
        let ac = subs.last().unwrap().clone();
        for (at, status, verdict) in [
            (-1, SubmissionLifecycle::Judged, Verdict::WrongAnswer),
            (
                960_000_000,
                SubmissionLifecycle::Judged,
                Verdict::WrongAnswer,
            ),
            (
                380_000_000,
                SubmissionLifecycle::Pending,
                Verdict::WrongAnswer,
            ),
            (
                380_000_000,
                SubmissionLifecycle::CompilationError,
                Verdict::CompileError,
            ),
            (
                380_000_000,
                SubmissionLifecycle::Judged,
                Verdict::SystemError,
            ),
        ] {
            subs.push(sub_with_status(99, 20, 203, at, Some(verdict), status));
        }
        assert_eq!(ac_settlement_at_us(&m, &subs, &ac, 20), 410_000_000);
        subs.push(sub(20, 203, 380_000_000, Some(Verdict::TimeLimitExceeded)));
        assert_eq!(ac_settlement_at_us(&m, &subs, &ac, 20), 390_000_000);
        assert_eq!(ac_settlement_at_us(&m, &subs, &ac, 0), 410_000_000);
        let mut tiebreak = m;
        tiebreak.xiaoju[0].index = 3;
        assert_eq!(ac_settlement_at_us(&tiebreak, &subs, &ac, 20), 410_000_000);
    }

    #[test]
    fn four_errors_shift_a_five_minute_submission_to_six_twenty() {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 600_000);
        let mut a = sub(10, 103, 300_000_000, Some(Verdict::Accepted));
        a.effective_at_us += 4 * 20_000_000;
        assert_eq!(
            at_deadline(&mut m, &[a.clone()], 379_999, 20),
            XiaojuOutcome::NotYet
        );
        assert_eq!(
            at_deadline(&mut m, &[a], 380_000, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn an_earlier_real_submission_with_later_effective_time_does_not_block() {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 600_000);
        let mut a = sub(10, 103, 60_000_000, None);
        a.effective_at_us += 20_000_000;
        let b = sub(20, 203, 61_000_000, Some(Verdict::Accepted));
        assert_eq!(
            at_deadline(&mut m, &[a, b], 62_000, 20),
            XiaojuOutcome::Decided { winner: Some(20) }
        );
    }

    #[test]
    fn timely_submissions_with_score_times_after_the_deadline_can_win_or_block() {
        for verdict in [Some(Verdict::Accepted), None] {
            let mut m = match_in_progress_at_xiaoju_with_deadline(0, 360_000);
            let expected = if verdict.is_some() {
                XiaojuOutcome::Decided { winner: Some(10) }
            } else {
                XiaojuOutcome::AwaitingJudge {
                    blocking_submission_id: 0,
                }
            };
            let mut a = sub(10, 103, 350_000_000, verdict);
            a.effective_at_us += 20_000_000;
            assert_eq!(at_deadline(&mut m, &[a], 360_000, 20), expected);
        }
    }

    #[test]
    fn an_exact_timestamp_tie_goes_to_the_smaller_submission_id() {
        // Same rule as the morning round (codelink-qualifier): order by submission time,
        // and on an exact tie the smaller submission id wins. The two halves of
        // the event must agree on who "got there first". Supplied with the
        // HIGHER id first so an arbitrary (iteration-order) tie-break fails.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub_with_status(
                9,
                20,
                203,
                1_000,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
            sub_with_status(
                4,
                10,
                103,
                1_000,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(
            at_deadline(&mut m, &subs, 9_999, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn an_older_pending_submission_at_the_same_instant_but_smaller_id_still_blocks() {
        // The blocking rule uses the same total order: a pending submission
        // that sorts BEFORE the best AC (same instant, smaller id) could still
        // beat it, so the 小局 must wait.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub_with_status(4, 10, 103, 1_000, None, SubmissionLifecycle::Pending),
            sub_with_status(
                9,
                20,
                203,
                1_000,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(at_deadline(&mut m, &subs, 9_999, 20), XiaojuOutcome::NotYet);
    }

    #[test]
    fn the_earliest_accepted_submission_wins_even_if_it_judged_second() {
        // A submits first, B second; B's verdict arrives first. A must win.
        // A test asserting only "the first AC observed wins" passes with the
        // buggy judged-time rule, so the inversion must be explicit.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub(
                /* A */ 10,
                103,
                /* submitted */ 1_000,
                Some(Verdict::Accepted),
            ),
            sub(
                /* B */ 20,
                203,
                /* submitted */ 1_500,
                Some(Verdict::Accepted),
            ),
        ];
        assert_eq!(
            at_deadline(&mut m, &subs, 9_999, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn an_earlier_pending_submission_blocks_the_decision() {
        // B has an AC at t=1500. A has an UNJUDGED submission at t=1000 that
        // could still be accepted and beat it. Deciding now would award B a
        // 小局 that may have to be revoked.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub(10, 103, 1_000, None),
            sub(20, 203, 1_500, Some(Verdict::Accepted)),
        ];
        assert_eq!(at_deadline(&mut m, &subs, 9_999, 20), XiaojuOutcome::NotYet);
    }

    #[test]
    fn a_later_pending_submission_does_not_block_the_decision() {
        // Boundary against over-correcting: a pending submission NEWER than
        // the best AC cannot beat it, so it must not stall the 小局.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub(10, 103, 1_000, Some(Verdict::Accepted)),
            sub(20, 203, 2_000, None),
        ];
        assert_eq!(
            at_deadline(&mut m, &subs, 9_999, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn neither_passing_before_the_deadline_scores_nobody() {
        // 如果在规定时间内双方均未能通过当前题目，则该小局双方均不得分
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [sub(10, 103, 1_000, Some(Verdict::WrongAnswer))];
        assert_eq!(
            at_deadline(&mut m, &subs, 5_001, 20),
            XiaojuOutcome::Decided { winner: None }
        );
    }

    #[test]
    fn before_the_deadline_with_no_ac_is_not_yet_decided() {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [sub(10, 103, 1_000, Some(Verdict::WrongAnswer))];
        assert_eq!(at_deadline(&mut m, &subs, 4_999, 20), XiaojuOutcome::NotYet);
    }

    #[test]
    fn deciding_an_already_decided_xiaoju_is_a_no_op() {
        // Load-bearing: timer delivery is at-least-once and three triggers
        // call this. A second call must not flip or double-count the
        // result.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [sub(10, 103, 1_000, Some(Verdict::Accepted))];
        let first = at_deadline(&mut m, &subs, 9_999, 20);
        let score_after_first = (m.score_a, m.score_b);
        let second = at_deadline(&mut m, &subs, 9_999, 20);
        assert_eq!(first, second);
        assert_eq!(
            (m.score_a, m.score_b),
            score_after_first,
            "score must not double-count"
        );
    }

    #[test]
    fn a_system_error_submission_older_than_a_later_ac_still_blocks() {
        // A `SystemError` submission has NO verdict written to it (see
        // the host's dispatch-failure and stuck-job
        // paths -- neither ever sets `submission.verdict`), but the
        // platform aggressively re-judges these
        // (`max_system_error_retries`): it is still IN FLIGHT, not a
        // terminal "not accepted" outcome, so an older one must still be
        // able to beat a later AC once it finally judges.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub_with_status(1, 10, 103, 1_000, None, SubmissionLifecycle::SystemError),
            sub_with_status(
                2,
                20,
                203,
                1_500,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(at_deadline(&mut m, &subs, 9_999, 20), XiaojuOutcome::NotYet);
    }

    #[test]
    fn a_judged_submission_with_a_system_error_verdict_still_blocks_a_later_ac() {
        // The shape measured on a real 4-worker stack under concurrent load:
        // the platform finalizes an infra fault as `status == Judged` with
        // `verdict == SystemError` (plugin-finalized), NOT as
        // `status == SystemError`. Classifying by status alone called that
        // terminal, so a correct solution that hit a sandbox fault simply
        // lost the 小局 to the opponent's later AC. The SystemError-retry
        // reaper re-judges exactly this shape, so it is still in flight.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub_with_status(
                1,
                10,
                103,
                1_000,
                Some(Verdict::SystemError),
                SubmissionLifecycle::Judged,
            ),
            sub_with_status(
                2,
                20,
                203,
                1_500,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(at_deadline(&mut m, &subs, 9_999, 20), XiaojuOutcome::NotYet);
    }

    #[test]
    fn a_lone_system_error_verdict_escalates_at_the_deadline_rather_than_scoring_nobody() {
        // Measured: both players' correct submissions came back
        // `Judged/SystemError` and the 小局 would have been scored 0-0 at the
        // deadline - both players robbed by a platform fault, silently. It
        // must escalate to AwaitingJudge so staff (or the reaper) can
        // re-judge, exactly like any other in-flight submission.
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [sub_with_status(
            7,
            10,
            103,
            1_000,
            Some(Verdict::SystemError),
            SubmissionLifecycle::Judged,
        )];
        assert_eq!(
            at_deadline(&mut m, &subs, 5_001, 20),
            XiaojuOutcome::AwaitingJudge {
                blocking_submission_id: 7
            }
        );
    }

    #[test]
    fn a_compilation_error_submission_older_than_a_later_ac_does_not_block() {
        // `CompilationError` also has NO verdict written to it, but it IS
        // terminal -- it can never turn into an accepted submission, so it
        // must not be able to hold a later AC hostage. A naive
        // `verdict.is_none()` in-flight check (the bug this pins) cannot
        // tell this case apart from the SystemError one above; this is the
        // designated break/restore-proof test.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [
            sub_with_status(
                1,
                10,
                103,
                1_000,
                None,
                SubmissionLifecycle::CompilationError,
            ),
            sub_with_status(
                2,
                20,
                203,
                1_500,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(
            at_deadline(&mut m, &subs, 9_999, 20),
            XiaojuOutcome::Decided { winner: Some(20) }
        );
    }

    #[test]
    fn a_blocked_deadline_passing_yields_awaiting_judge_and_records_the_blocker() {
        // Same shape as `an_earlier_pending_submission_blocks_the_decision`,
        // but the deadline has now passed. Awarding the later AC would
        // break "a platform fault must never cost a player the 小局"; the
        // contest owner chose escalation instead of a silent win, so this
        // must surface as a new, visible state -- not `Decided`, and not a
        // forever-`NotYet` hang either.
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [
            sub_with_status(77, 10, 103, 1_000, None, SubmissionLifecycle::SystemError),
            sub_with_status(
                2,
                20,
                203,
                2_000,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(
            at_deadline(&mut m, &subs, 5_001, 20),
            XiaojuOutcome::AwaitingJudge {
                blocking_submission_id: 77
            }
        );
    }

    #[test]
    fn a_verdict_landing_while_awaiting_judge_decides_normally_and_the_earlier_submitter_wins() {
        // The blocking submission's verdict finally lands, and it is
        // accepted: the earliest-submitted rule must still hold -- the
        // EARLIER submitter (the one that had been blocking) wins the 小局,
        // not the later AC that was provisionally ahead while blocked.
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [
            sub_with_status(
                77,
                10,
                103,
                1_000,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
            sub_with_status(
                2,
                20,
                203,
                2_000,
                Some(Verdict::Accepted),
                SubmissionLifecycle::Judged,
            ),
        ];
        assert_eq!(
            at_deadline(&mut m, &subs, 5_001, 20),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    fn setup() -> Setup {
        Setup {
            stage_clocks: Default::default(),
            third_place: None,
            seeds: (1..=16).collect(),
            rounds: vec![RoundDef {
                start_at_ms: None,
                post_order_wait_seconds: None,
                game_break_seconds: None,
                match_extra_time_seconds: None,
                wins_to_advance: None,
                group_a: vec![101, 102, 103],
                group_b: vec![201, 202, 203],
                tiebreak: vec![301, 302],
                xiaoju_seconds: 1_800,
                tiebreak_seconds: 1_800,
                break_before_seconds: 0,
            }],
            escalation_grace_seconds: 120,
            penalty_seconds: crate::model::default_penalty_seconds(),
            ..Default::default()
        }
    }

    /// A match whose regular 小局 so far ended with the given winners (`None`
    /// = that 小局 scored nobody), as `match_outcome` reads it right after
    /// `decide_xiaoju` settles the last of them.
    fn match_after_xiaoju(winners: &[Option<i32>]) -> MatchState {
        let mut score_a = 0u8;
        let mut score_b = 0u8;
        let xiaoju = winners
            .iter()
            .enumerate()
            .map(|(i, &winner)| {
                match winner {
                    Some(10) => score_a += 1,
                    Some(20) => score_b += 1,
                    _ => {}
                }
                XiaojuState {
                    index: i as u8,
                    opened_at_ms: 0,
                    deadline_ms: i64::MAX,
                    winner,
                    decided: true,
                    ..Default::default()
                }
            })
            .collect();
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            xiaoju,
            score_a,
            score_b,
            phase: Phase::Playing,
            ..Default::default()
        }
    }

    /// A match whose regular games finished level 1-1 and whose first
    /// `attempts` 附加赛 attempts all scored nobody.
    fn match_with_scoreless_tiebreaks(attempts: usize) -> MatchState {
        let mut m = match_after_xiaoju(&[Some(10), Some(20), None]);
        for _ in 0..attempts {
            m.xiaoju.push(XiaojuState {
                index: m.xiaoju.len() as u8,
                deadline_ms: i64::MAX,
                decided: true,
                ..Default::default()
            });
        }
        m
    }

    fn outcome(m: &MatchState, setup: &Setup) -> Option<MatchOutcome> {
        match_outcome(m, &setup.rounds[0])
    }

    #[test]
    fn two_zero_finishes_bo3_ft2_for_either_player_without_a_third_game() {
        for winner in [10, 20] {
            let m = match_after_xiaoju(&[Some(winner), Some(winner)]);
            assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::Won(winner)));
        }
    }

    #[test]
    fn bo5_ft3_finishes_after_three_wins_but_not_two() {
        let mut config = setup();
        config.rounds[0].group_a.extend([104, 105]);
        config.rounds[0].group_b.extend([204, 205]);
        assert_eq!(config.rounds[0].win_target(), 3);
        for wins in [2, 3] {
            let mut m = match_after_xiaoju(&vec![Some(10); wins]);
            m.group_a = config.rounds[0].group_a.clone();
            m.group_b = config.rounds[0].group_b.clone();
            let expected = if wins == 3 {
                MatchOutcome::Won(10)
            } else {
                MatchOutcome::NextGame
            };
            assert_eq!(outcome(&m, &config), Some(expected));
        }
    }

    #[test]
    fn ft_is_independent_of_bo_and_can_preserve_the_play_all_rule() {
        let mut config = setup();
        config.rounds[0].wins_to_advance = Some(1);
        let m = match_after_xiaoju(&[Some(20)]);
        assert_eq!(outcome(&m, &config), Some(MatchOutcome::Won(20)));
        config.rounds[0].wins_to_advance = Some(3);
        let m = match_after_xiaoju(&[Some(10), Some(10)]);
        assert_eq!(outcome(&m, &config), Some(MatchOutcome::NextGame));
    }

    #[test]
    fn scoreless_games_use_the_bo_cap_without_counting_toward_ft() {
        let mut m = match_after_xiaoju(&[Some(10), None]);
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::NextGame));
        m.xiaoju.push(XiaojuState {
            index: 2,
            decided: true,
            ..Default::default()
        });
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::Won(10)));
    }

    #[test]
    fn an_unsettled_game_cannot_trigger_ft() {
        let mut m = match_after_xiaoju(&[Some(10), Some(10)]);
        m.xiaoju.last_mut().unwrap().decided = false;
        assert_eq!(outcome(&m, &setup()), None);
    }

    #[test]
    fn higher_score_after_three_xiaoju_wins() {
        let m = match_after_xiaoju(&[Some(10), Some(20), Some(10)]); // 2-1
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::Won(10)));
    }

    #[test]
    fn a_scoreless_xiaoju_counts_for_neither_side() {
        // 2-0 with one 小局 scoring nobody still decides the match.
        let m = match_after_xiaoju(&[Some(10), None, Some(10)]);
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::Won(10)));
    }

    #[test]
    fn a_level_score_continues_with_the_first_tiebreak_problem() {
        let m = match_after_xiaoju(&[Some(10), Some(20), None]); // 1-1
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::NextGame));
        assert_eq!(m.game_kind(3), GameKind::Tiebreak { attempt: 0 });
    }

    #[test]
    fn a_zero_zero_match_also_goes_to_a_tiebreak() {
        // 0-0 is level too - an `if score_a == score_b && score_a > 0` guard
        // would miss it and leave the match undecided forever.
        let m = match_after_xiaoju(&[None, None, None]);
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::NextGame));
    }

    #[test]
    fn a_scoreless_tiebreak_continues_with_the_next_tiebreak_problem() {
        let m = match_with_scoreless_tiebreaks(1);
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::NextGame));
        assert_eq!(m.game_kind(4), GameKind::Tiebreak { attempt: 1 });
    }

    #[test]
    fn a_tiebreak_winner_wins_the_match() {
        let mut m = match_with_scoreless_tiebreaks(1);
        m.xiaoju.last_mut().unwrap().winner = Some(20);
        assert_eq!(outcome(&m, &setup()), Some(MatchOutcome::Won(20)));
    }

    #[test]
    fn exhausting_the_tiebreak_list_needs_adjudication_rather_than_hanging() {
        let m = match_with_scoreless_tiebreaks(setup().rounds[0].tiebreak.len());
        assert_eq!(
            outcome(&m, &setup()),
            Some(MatchOutcome::TiebreaksExhausted)
        );
    }

    #[test]
    fn a_settlement_retried_after_a_lost_write_charges_extra_time_once() {
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::storage;
        use crate::test_support::{seed, setup_two_rounds};
        use broccoli_server_sdk::Host;

        let host = Host::mock();
        let mut setup = setup_two_rounds();
        setup.rounds[0].xiaoju_seconds = 10;
        setup.match_extra_time_seconds = 5;
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 15_000);
        m.xiaoju[0].base_deadline_ms = 10_000;
        m.extra_time_remaining_ms = 5_000;
        seed(&host, 7, &setup, 0, &m);
        let subs = [SubmissionRecord {
            submission_id: 1,
            user_id: m.player_a,
            problem_id: 103,
            submitted_at_us: 12_000_000,
            effective_at_us: 12_000_000,
            verdict: Some(Verdict::Accepted),
            status: SubmissionLifecycle::Judged,
        }];
        let raced = AtomicBool::new(false);
        let settled = storage::update_match(&host, 7, 0, |snapshot| {
            if !raced.swap(true, Ordering::SeqCst) {
                // Another callback commits between this attempt's read and CAS.
                storage::update_match(&host, 7, 0, |other| {
                    at_deadline(other, &subs, 14_000, 20);
                    Ok(())
                })?;
            }
            at_deadline(snapshot, &subs, 14_000, 20);
            Ok(())
        })
        .unwrap();
        assert_eq!(settled.score_a, 1);
        assert_eq!(settled.extra_time_remaining_ms, 3_000);
        assert_eq!(settled.xiaoju[0].extra_time_used_ms, 2_000);
    }
}
