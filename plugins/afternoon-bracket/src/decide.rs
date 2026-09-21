//! The bracket's state machine: deciding one 小局, then a whole match.
//!
//! Two idempotent functions (quoted from
//! `docs/superpowers/specs/2026-09-19-afternoon-bracket-design.md`):
//!
//! > Idempotency is load-bearing, not incidental: timer delivery is
//! > at-least-once, and three independent triggers converge here.
//!
//! Judging completion, a 小局 deadline timer, and a staff force-decide can
//! all land on the same match at unpredictable times and in any order; both
//! [`decide_xiaoju`] and (once Task 8 adds it) `decide_match` must produce
//! the SAME answer no matter how many times, or in what order relative to
//! each other, they are re-driven.

use broccoli_server_sdk::prelude::Verdict;

use crate::model::MatchState;

/// One submission, as `decide_xiaoju` needs to see it: which player made it,
/// to which problem, when it was SUBMITTED (not judged -- see
/// [`decide_xiaoju`]'s doc comment for why that distinction matters), and
/// its verdict if judging has completed (`None` while still pending).
#[derive(Debug, Clone)]
pub struct SubmissionRecord {
    pub user_id: i32,
    pub problem_id: i32,
    pub submitted_at_ms: i64,
    pub verdict: Option<Verdict>,
}

/// Outcome of deciding one 小局.
///
/// `Decided { winner: Option<i32> }` is deliberately a DIFFERENT shape from
/// `MatchOutcome::Decided { winner: i32 }` (Task 8): a 小局 can legitimately
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
    /// still pending judgment and could still beat the current best AC (see
    /// [`decide_xiaoju`]'s doc comment).
    NotYet,
    Decided {
        winner: Option<i32>,
    },
}

/// Decide the currently open 小局 (`m.xiaoju.last()`) from the submissions
/// known so far. No-op (returns the stored outcome without re-scoring) if
/// that 小局 is already decided -- see the module doc comment on why that is
/// load-bearing, not a nicety.
///
/// # The winner is the earliest SUBMISSION, not the earliest JUDGED result
///
/// Quoted from the spec:
///
/// > Winner = whichever player has an accepted submission to their own
/// > current problem with the earliest submission time -- not the earliest
/// > judged time. Judging latency varies with problem and queue depth, so
/// > ranking by judged time would let the player who submitted second win
/// > whenever their problem happened to judge faster. The race is between
/// > the players, not their judges.
///
/// A consequence: if the best AC found so far was submitted at `t`, and some
/// OTHER submission older than `t` is still pending (`verdict: None`), the
/// 小局 is not yet decidable -- that pending submission could still turn out
/// accepted and, having been submitted earlier, would have to win instead.
/// `decide_xiaoju` returns [`XiaojuOutcome::NotYet`] in that case rather than
/// awarding a winner it might have to revoke once the pending verdict
/// arrives.
///
/// Does not validate that `problem_id` is the player's actual current
/// problem -- that is `gate.rs`'s job (submissions to the wrong problem are
/// rejected before they exist at all); this function trusts every record it
/// is handed.
pub fn decide_xiaoju(m: &mut MatchState, subs: &[SubmissionRecord], now_ms: i64) -> XiaojuOutcome {
    let Some(current) = m.xiaoju.last() else {
        // Nothing open to decide. Callers (Task 9's `advance`) are expected
        // to only call this while a 小局 is open, but there is no unsafe
        // state to fall into here, so fail quiet rather than panic.
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

    let best_ac = subs
        .iter()
        .filter(|s| s.verdict.as_ref().is_some_and(Verdict::is_accepted))
        .min_by_key(|s| s.submitted_at_ms);

    let winner = match best_ac {
        Some(ac) => {
            let blocked_by_an_older_pending_submission = subs
                .iter()
                .any(|s| s.verdict.is_none() && s.submitted_at_ms < ac.submitted_at_ms);
            if blocked_by_an_older_pending_submission {
                return XiaojuOutcome::NotYet;
            }
            Some(ac.user_id)
        }
        None => {
            if now_ms < deadline_ms {
                return XiaojuOutcome::NotYet;
            }
            // 如果在规定时间内双方均未能通过当前题目，则该小局双方均不得分
            None
        }
    };

    if let Some(slot) = m.xiaoju.last_mut() {
        slot.winner = winner;
        slot.decided = true;
    }
    match winner {
        Some(uid) if uid == m.player_a => m.score_a += 1,
        Some(uid) if uid == m.player_b => m.score_b += 1,
        _ => {}
    }

    XiaojuOutcome::Decided { winner }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MatchPhase, XiaojuState};

    fn match_in_progress_at_xiaoju(index: u8) -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            xiaoju: vec![XiaojuState {
                index,
                opened_at_ms: 0,
                deadline_ms: i64::MAX,
                winner: None,
                decided: false,
            }],
            state: MatchPhase::InProgress,
            ..Default::default()
        }
    }

    fn match_in_progress_at_xiaoju_with_deadline(index: u8, deadline_ms: i64) -> MatchState {
        let mut m = match_in_progress_at_xiaoju(index);
        m.xiaoju[0].deadline_ms = deadline_ms;
        m
    }

    fn sub(
        user_id: i32,
        problem_id: i32,
        submitted_at_ms: i64,
        verdict: Option<Verdict>,
    ) -> SubmissionRecord {
        SubmissionRecord {
            user_id,
            problem_id,
            submitted_at_ms,
            verdict,
        }
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
            decide_xiaoju(&mut m, &subs, 9_999),
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
        assert_eq!(decide_xiaoju(&mut m, &subs, 9_999), XiaojuOutcome::NotYet);
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
            decide_xiaoju(&mut m, &subs, 9_999),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    #[test]
    fn neither_passing_before_the_deadline_scores_nobody() {
        // 如果在规定时间内双方均未能通过当前题目，则该小局双方均不得分
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [sub(10, 103, 1_000, Some(Verdict::WrongAnswer))];
        assert_eq!(
            decide_xiaoju(&mut m, &subs, 5_001),
            XiaojuOutcome::Decided { winner: None }
        );
    }

    #[test]
    fn before_the_deadline_with_no_ac_is_not_yet_decided() {
        let mut m = match_in_progress_at_xiaoju_with_deadline(0, 5_000);
        let subs = [sub(10, 103, 1_000, Some(Verdict::WrongAnswer))];
        assert_eq!(decide_xiaoju(&mut m, &subs, 4_999), XiaojuOutcome::NotYet);
    }

    #[test]
    fn deciding_an_already_decided_xiaoju_is_a_no_op() {
        // Load-bearing: timer delivery is at-least-once and three triggers
        // call this. A second call must not flip or double-count the
        // result.
        let mut m = match_in_progress_at_xiaoju(0);
        let subs = [sub(10, 103, 1_000, Some(Verdict::Accepted))];
        let first = decide_xiaoju(&mut m, &subs, 9_999);
        let score_after_first = (m.score_a, m.score_b);
        let second = decide_xiaoju(&mut m, &subs, 9_999);
        assert_eq!(first, second);
        assert_eq!(
            (m.score_a, m.score_b),
            score_after_first,
            "score must not double-count"
        );
    }
}
