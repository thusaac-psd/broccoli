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
use serde::{Deserialize, Serialize};

use crate::model::{MatchPhase, MatchState, Setup};

/// The 3 regular 小局 (index 0..3); 附加赛 (tiebreak) attempts are appended
/// only once `m.state` becomes `Tiebreak`. Shared with `judge.rs`, which
/// needs the same boundary both to classify `current_problem` during
/// `MatchPhase::AwaitingJudge` (see that variant's doc comment for why
/// `m.state` alone no longer distinguishes regular from tiebreak there) and
/// to restore `m.state` once a blocked 小局 resolves.
pub(crate) const REGULAR_XIAOJU_COUNT: usize = 3;

/// Whether xiaoju `index` belongs to the 3 regular 小局 (vs. an 附加赛
/// attempt). See [`REGULAR_XIAOJU_COUNT`].
pub(crate) fn is_regular_xiaoju_index(index: u8) -> bool {
    (index as usize) < REGULAR_XIAOJU_COUNT
}

/// A submission's host-wide lifecycle status, as read back from the raw
/// `submission.status` column (see `judge.rs::fetch_subs`).
///
/// This is a PLUGIN-LOCAL mirror of `packages/common::SubmissionStatus`, not
/// a re-export of it: this WASM guest crate depends only on
/// `broccoli-server-sdk`, which exposes a DIFFERENT, narrower type under the
/// same name (`broccoli_types::persistence::SubmissionStatus`, with only
/// `Compiling`/`Running`/`Judged`/`CompilationError` -- the subset a plugin
/// may WRITE via `host.submission.update`), not the full 7-variant
/// host-lifecycle enum a submission can be READ back as. Variant spellings
/// must stay in exact sync with `packages/common::SubmissionStatus`
/// (`#[serde(rename_all = "PascalCase")]` there too) since both sides
/// serialize the same underlying Postgres enum column as text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SubmissionLifecycle {
    Queued,
    Pending,
    Compiling,
    Running,
    Judged,
    CompilationError,
    SystemError,
}

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
fn submission_in_flight(s: &SubmissionRecord) -> bool {
    is_in_flight(&s.status) || matches!(s.verdict, Some(Verdict::SystemError))
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
    pub submitted_at_ms: i64,
    pub verdict: Option<Verdict>,
    pub status: SubmissionLifecycle,
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
/// OTHER submission older than `t` is still IN FLIGHT (see `is_in_flight`
/// -- not simply `verdict.is_none()`: a terminally-failed submission can
/// also have no verdict written, see that function's doc comment), the 小局
/// is not yet decidable -- that submission could still turn out accepted
/// and, having been submitted earlier, would have to win instead.
/// `decide_xiaoju` returns [`XiaojuOutcome::NotYet`] in that case BEFORE the
/// deadline, rather than awarding a winner it might have to revoke once the
/// verdict arrives. Once the deadline has passed while still blocked, it
/// returns [`XiaojuOutcome::AwaitingJudge`] instead -- see that variant's
/// doc comment for why neither awarding the AC nor staying `NotYet` forever
/// is acceptable at that point.
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
            // The OLDEST in-flight submission older than the best AC: if it
            // is still blocking once the deadline passes, that is the one
            // staff should rejudge first -- it has been stuck the longest.
            let blocker = subs
                .iter()
                .filter(|s| submission_in_flight(s) && s.submitted_at_ms < ac.submitted_at_ms)
                .min_by_key(|s| s.submitted_at_ms);
            if let Some(blocker) = blocker {
                if now_ms < deadline_ms {
                    return XiaojuOutcome::NotYet;
                }
                return XiaojuOutcome::AwaitingJudge {
                    blocking_submission_id: blocker.submission_id,
                };
            }
            Some(ac.user_id)
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
            let blocker = subs
                .iter()
                .filter(|s| submission_in_flight(s))
                .min_by_key(|s| s.submitted_at_ms);
            if let Some(blocker) = blocker {
                return XiaojuOutcome::AwaitingJudge {
                    blocking_submission_id: blocker.submission_id,
                };
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

/// Outcome of deciding a whole match: the 3 regular 小局, and any 附加赛
/// (tiebreak) that follows a level score.
///
/// `Decided { winner: i32 }` is deliberately a DIFFERENT shape from
/// [`XiaojuOutcome::Decided`]'s `winner: Option<i32>`: a MATCH always ends
/// with a winner or escalates to [`MatchOutcome::NeedsAdjudication`], never
/// scorelessly -- even though an individual 小局 or 附加赛 attempt can be.
/// See [`XiaojuOutcome`]'s doc comment for the same warning from the other
/// side; do not "harmonise" these two shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchOutcome {
    NotYet,
    Decided { winner: i32 },
    OpenTiebreak { problem_id: i32 },
    NeedsAdjudication,
}

/// Decide a match from its accumulated 小局 state.
///
/// After the 3 regular 小局, higher `score_a`/`score_b` wins. A LEVEL score
/// -- **including 0-0** -- opens the round's first 附加赛 problem:
/// `if score_a == score_b && score_a > 0` is a real bug shape here, not a
/// hypothetical one, because it silently leaves a 0-0 match undecided
/// forever instead of sending it to a tiebreak (see
/// `a_zero_zero_match_also_goes_to_a_tiebreak` below).
///
/// A 附加赛 attempt that ends scorelessly does not replay the same problem;
/// it opens the NEXT problem in `RoundDef::tiebreak`'s order (see that
/// field's doc comment for why). Exhausting the list without a decision
/// reaches [`MatchOutcome::NeedsAdjudication`] rather than looping forever
/// waiting for a 附加赛 problem that does not exist.
///
/// Idempotent for the same reason [`decide_xiaoju`] is: a match already
/// `Decided` or `NeedsAdjudication` returns its stored outcome without
/// touching state again, since judging completion, a timer, and a staff
/// force-decide can all call this redundantly.
pub fn decide_match(m: &mut MatchState, setup: &Setup) -> MatchOutcome {
    match m.state {
        MatchPhase::Decided => {
            return MatchOutcome::Decided {
                // `m.winner` is always `Some` once `m.state == Decided` --
                // this function is the only writer of both, together.
                winner: m.winner.unwrap_or(m.player_a),
            };
        }
        MatchPhase::NeedsAdjudication => return MatchOutcome::NeedsAdjudication,
        _ => {}
    }

    let Some(round_def) = setup.rounds.get(m.round.saturating_sub(1) as usize) else {
        // No round definition to find 附加赛 problems in -- cannot safely
        // proceed. Escalate rather than guess or panic.
        m.state = MatchPhase::NeedsAdjudication;
        return MatchOutcome::NeedsAdjudication;
    };

    if m.state == MatchPhase::Tiebreak {
        let Some(current) = m.xiaoju.last() else {
            return MatchOutcome::NotYet;
        };
        let decided = current.decided;
        let tiebreak_winner = current.winner;
        if !decided {
            return MatchOutcome::NotYet;
        }
        return match tiebreak_winner {
            Some(winner) => {
                m.state = MatchPhase::Decided;
                m.winner = Some(winner);
                MatchOutcome::Decided { winner }
            }
            None => {
                let next_index = m.tiebreak_index + 1;
                match round_def.tiebreak.get(next_index) {
                    Some(&problem_id) => {
                        m.tiebreak_index = next_index;
                        MatchOutcome::OpenTiebreak { problem_id }
                    }
                    None => {
                        m.state = MatchPhase::NeedsAdjudication;
                        MatchOutcome::NeedsAdjudication
                    }
                }
            }
        };
    }

    // Regular phase: waiting on the 3 regular 小局 (index 0..3; 附加赛
    // attempts are appended only once `m.state` becomes `Tiebreak`, so
    // nothing here needs to filter them out).
    let all_regular_decided =
        m.xiaoju.len() == REGULAR_XIAOJU_COUNT && m.xiaoju.iter().all(|x| x.decided);
    if !all_regular_decided {
        return MatchOutcome::NotYet;
    }

    match m.score_a.cmp(&m.score_b) {
        std::cmp::Ordering::Greater => {
            m.state = MatchPhase::Decided;
            m.winner = Some(m.player_a);
            MatchOutcome::Decided { winner: m.player_a }
        }
        std::cmp::Ordering::Less => {
            m.state = MatchPhase::Decided;
            m.winner = Some(m.player_b);
            MatchOutcome::Decided { winner: m.player_b }
        }
        std::cmp::Ordering::Equal => match round_def.tiebreak.first() {
            Some(&problem_id) => {
                m.state = MatchPhase::Tiebreak;
                m.tiebreak_index = 0;
                MatchOutcome::OpenTiebreak { problem_id }
            }
            None => {
                m.state = MatchPhase::NeedsAdjudication;
                MatchOutcome::NeedsAdjudication
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MatchPhase, RoundDef, XiaojuState};

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
        sub_with_status(0, user_id, problem_id, submitted_at_ms, verdict, status)
    }

    fn sub_with_status(
        submission_id: i32,
        user_id: i32,
        problem_id: i32,
        submitted_at_ms: i64,
        verdict: Option<Verdict>,
        status: SubmissionLifecycle,
    ) -> SubmissionRecord {
        SubmissionRecord {
            submission_id,
            user_id,
            problem_id,
            submitted_at_ms,
            verdict,
            status,
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

    #[test]
    fn a_system_error_submission_older_than_a_later_ac_still_blocks() {
        // A `SystemError` submission has NO verdict written to it (see
        // `mark_submission_system_error_with_epoch` and the stuck-job
        // handler -- neither ever sets `submission.verdict`), but the
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
        assert_eq!(decide_xiaoju(&mut m, &subs, 9_999), XiaojuOutcome::NotYet);
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
        assert_eq!(decide_xiaoju(&mut m, &subs, 9_999), XiaojuOutcome::NotYet);
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
            decide_xiaoju(&mut m, &subs, 5_001),
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
            decide_xiaoju(&mut m, &subs, 9_999),
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
            decide_xiaoju(&mut m, &subs, 5_001),
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
            decide_xiaoju(&mut m, &subs, 5_001),
            XiaojuOutcome::Decided { winner: Some(10) }
        );
    }

    fn setup() -> Setup {
        Setup {
            rounds: vec![RoundDef {
                group_a: [101, 102, 103],
                group_b: [201, 202, 203],
                tiebreak: vec![301, 302],
            }],
            xiaoju_seconds: 1_800,
            round_intermission_seconds: 300,
            escalation_grace_seconds: 120,
        }
    }

    /// A match that has finished its 3 regular 小局 with the given winners
    /// (`None` = that 小局 scored nobody), still in `InProgress` -- exactly
    /// the state `decide_match` is called in right after the 3rd 小局's
    /// `decide_xiaoju` call returns `Decided`.
    fn match_after_xiaoju(winners: &[Option<i32>; 3]) -> MatchState {
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
                }
            })
            .collect();
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            xiaoju,
            score_a,
            score_b,
            state: MatchPhase::InProgress,
            winner: None,
            tiebreak_index: 0,
            decided_at_ms: 0,
            awaiting_submission_id: None,
            xiaoju_seconds: 0,
        }
    }

    /// A match already in `Tiebreak`, whose 附加赛 attempt at
    /// `tiebreak_index` has been decided scorelessly (both regular 小局
    /// finished level 1-1, then this 附加赛 attempt scored nobody too).
    fn match_with_scoreless_tiebreak(tiebreak_index: usize) -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            xiaoju: vec![
                XiaojuState {
                    index: 0,
                    opened_at_ms: 0,
                    deadline_ms: i64::MAX,
                    winner: Some(10),
                    decided: true,
                },
                XiaojuState {
                    index: 1,
                    opened_at_ms: 0,
                    deadline_ms: i64::MAX,
                    winner: Some(20),
                    decided: true,
                },
                XiaojuState {
                    index: 2,
                    opened_at_ms: 0,
                    deadline_ms: i64::MAX,
                    winner: None,
                    decided: true,
                },
                XiaojuState {
                    index: 3 + tiebreak_index as u8,
                    opened_at_ms: 0,
                    deadline_ms: i64::MAX,
                    winner: None,
                    decided: true,
                },
            ],
            score_a: 1,
            score_b: 1,
            state: MatchPhase::Tiebreak,
            winner: None,
            tiebreak_index,
            decided_at_ms: 0,
            awaiting_submission_id: None,
            xiaoju_seconds: 0,
        }
    }

    #[test]
    fn higher_score_after_three_xiaoju_wins() {
        let mut m = match_after_xiaoju(&[Some(10), Some(20), Some(10)]); // 2-1
        assert_eq!(
            decide_match(&mut m, &setup()),
            MatchOutcome::Decided { winner: 10 }
        );
    }

    #[test]
    fn a_scoreless_xiaoju_counts_for_neither_side() {
        // 2-0 with one 小局 scoring nobody still decides the match.
        let mut m = match_after_xiaoju(&[Some(10), None, Some(10)]);
        assert_eq!(
            decide_match(&mut m, &setup()),
            MatchOutcome::Decided { winner: 10 }
        );
    }

    #[test]
    fn a_level_score_opens_the_first_tiebreak_problem() {
        let mut m = match_after_xiaoju(&[Some(10), Some(20), None]); // 1-1
        assert_eq!(
            decide_match(&mut m, &setup()),
            MatchOutcome::OpenTiebreak {
                problem_id: setup().rounds[0].tiebreak[0]
            }
        );
    }

    #[test]
    fn a_zero_zero_match_also_goes_to_a_tiebreak() {
        // 0-0 is level too - an `if score_a == score_b && score_a > 0` guard
        // would miss it and leave the match undecided forever.
        let mut m = match_after_xiaoju(&[None, None, None]);
        assert!(matches!(
            decide_match(&mut m, &setup()),
            MatchOutcome::OpenTiebreak { .. }
        ));
    }

    #[test]
    fn a_scoreless_tiebreak_opens_the_next_tiebreak_problem() {
        let mut m = match_with_scoreless_tiebreak(0);
        assert_eq!(
            decide_match(&mut m, &setup()),
            MatchOutcome::OpenTiebreak {
                problem_id: setup().rounds[0].tiebreak[1]
            }
        );
    }

    #[test]
    fn exhausting_the_tiebreak_list_needs_adjudication_rather_than_hanging() {
        let mut m = match_with_scoreless_tiebreak(setup().rounds[0].tiebreak.len() - 1);
        assert_eq!(
            decide_match(&mut m, &setup()),
            MatchOutcome::NeedsAdjudication
        );
    }

    #[test]
    fn deciding_a_decided_match_is_a_no_op() {
        let mut m = match_after_xiaoju(&[Some(10), Some(10), Some(10)]);
        let first = decide_match(&mut m, &setup());
        assert_eq!(decide_match(&mut m, &setup()), first);
    }
}
