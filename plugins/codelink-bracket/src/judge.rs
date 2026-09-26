//! Judging and timer wiring: the single convergence point for the three
//! independent triggers that can advance a match.
//!
//! Quoted from `docs/superpowers/plans/2026-09-19-afternoon-bracket.md`:
//!
//! > `advance` loads the match, calls `decide_xiaoju`, then `decide_match`,
//! > then applies the outcome: cancel the old deadline, schedule the new one
//! > (`timer.schedule` with key `xiaoju:{contest}:{match}:{k}`), write the
//! > next-round slot on a decision.
//!
//! [`advance`] is that single function. A judging callback finishing
//! ([`CodelinkBracketJudge::finalize`]) and a 小局 deadline firing
//! (`on_timer`) call NOTHING else -- every automatic, evidence-driven
//! advancement funnels through `advance`, so a duplicate delivery of either
//! is harmless: `decide_xiaoju` and `decide_match` are both idempotent (see
//! `decide.rs`'s module doc comment), and `advance` adds no state of its own
//! beyond replaying them.
//!
//! Staff's `/force-decide` (Task 10's `routes::handle_force_decide`) has two
//! distinct behaviours, matching the spec's "force expiry / resolve
//! adjudication": with no explicit winner in the request, it simply calls
//! `advance` too (a manual nudge at the CURRENT real time -- useful if a
//! timer failed to fire). With an explicit winner, it calls [`force_decide`]
//! instead, a genuinely different, non-idempotent-with-`advance` path:
//! neither `decide_xiaoju` nor `decide_match` can ever invent a winner
//! without either an accepted submission or an elapsed deadline, so a
//! mid-match withdrawal or a `NeedsAdjudication` resolution -- which by
//! definition have neither -- cannot be expressed by replaying them.
//!
//! # Timer key discipline
//!
//! One timer per 小局: `xiaoju:{contest}:{match_id}:{index}`
//! ([`xiaoju_timer_key`]). When a 小局 ends EARLY (an AC before its
//! deadline), `step` cancels that 小局's timer before opening the next
//! one or deciding the match. Without this, a stale timer for the OLD 小局
//! would still be scheduled to fire at the old deadline, land on the NEW
//! (or no longer existent) 小局, and -- because `advance` re-reads whatever
//! is currently open -- decide it prematurely using a deadline that was
//! never actually set for it. See `tests::deciding_a_xiaoju_early_cancels_its_timer_before_scheduling_the_next`
//! for the regression test, and the module's own break/restore proof
//! recorded in the Task 9 report.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::db::Params;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::evaluator::{
    CaseOutcome, ContestJudge, DetachedEval, JudgeProgress, JudgeStep,
};
use broccoli_server_sdk::types::*;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde::{Deserialize, Serialize};

use crate::bracket;
use crate::decide::{self, MatchOutcome, SubmissionLifecycle, SubmissionRecord, XiaojuOutcome};
use crate::model::{MatchPhase, MatchState, RoundDef, Setup, XiaojuState};
use crate::storage;

/// The storage/timer key for one 小局's deadline. Shared by `step` (which
/// schedules and cancels it) and `on_timer` (which parses it back out of
/// the fired timer's key).
pub fn xiaoju_timer_key(contest: i32, match_id: u8, index: u8) -> String {
    format!("xiaoju:{contest}:{match_id}:{index}")
}

/// Parse a `xiaoju:{contest}:{match_id}:{index}` key back into
/// `(contest, match_id)`. The 小局 index itself is not needed by
/// `on_timer` -- `advance` always re-reads whichever 小局 is CURRENTLY
/// open, not the one named in the fired timer (see the module doc comment
/// on why a stale timer must be cancelled rather than trusted).
pub fn parse_xiaoju_timer_key(key: &str) -> Option<(i32, u8)> {
    let mut parts = key.split(':');
    if parts.next() != Some("xiaoju") {
        return None;
    }
    let contest: i32 = parts.next()?.parse().ok()?;
    let match_id: u8 = parts.next()?.parse().ok()?;
    // A trailing index segment must still be present (even though unused)
    // for this to be recognised as a well-formed key at all.
    parts.next()?;
    Some((contest, match_id))
}

/// The storage/timer key for the escalation grace timer scheduled when a
/// match enters `MatchPhase::AwaitingJudge` (see [`decide::XiaojuOutcome::AwaitingJudge`]
/// and `step`). Deliberately a DIFFERENT namespace from
/// [`xiaoju_timer_key`] -- the two must never be mistaken for each other
/// (see the module doc comment's "Timer key discipline" section), since a
/// match can have both a live 小局 timer and a live escalation timer
/// pending at once (whichever fires first wins the race, and both must be
/// individually cancellable without touching the other).
pub fn judgewait_timer_key(contest: i32, match_id: u8, index: u8) -> String {
    format!("judgewait:{contest}:{match_id}:{index}")
}

/// Parse a `judgewait:{contest}:{match_id}:{index}` key back into
/// `(contest, match_id)`. Mirrors [`parse_xiaoju_timer_key`] exactly,
/// including discarding the index -- the same
/// `a_timer_key_carries_no_authority_over_which_xiaoju_is_decided`
/// invariant applies here: [`escalate`] re-reads the match's CURRENT state
/// rather than trusting anything encoded in the fired timer's key.
pub fn parse_judgewait_timer_key(key: &str) -> Option<(i32, u8)> {
    let mut parts = key.split(':');
    if parts.next() != Some("judgewait") {
        return None;
    }
    let contest: i32 = parts.next()?.parse().ok()?;
    let match_id: u8 = parts.next()?.parse().ok()?;
    parts.next()?;
    Some((contest, match_id))
}

/// Timer key for a match's automatic start (see [`try_autostart`]). One
/// per match: rescheduling it simply replaces the earlier fire time.
pub fn start_timer_key(contest: i32, match_id: u8) -> String {
    format!("start:{contest}:{match_id}")
}

/// Parse a `start:{contest}:{match_id}` key.
pub fn parse_start_timer_key(key: &str) -> Option<(i32, u8)> {
    let mut parts = key.split(':');
    if parts.next() != Some("start") {
        return None;
    }
    let contest: i32 = parts.next()?.parse().ok()?;
    let match_id: u8 = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((contest, match_id))
}

#[derive(Debug, Deserialize)]
struct ContestStartRow {
    start_ms: f64,
}

/// The contest's start time, epoch ms. Round 1 cannot start before it.
pub(crate) fn contest_start_ms(host: &Host, contest: i32) -> Result<i64, SdkError> {
    let mut p = Params::new();
    let sql = format!(
        "SELECT EXTRACT(EPOCH FROM start_time) * 1000 AS start_ms FROM contest WHERE id = {}",
        p.bind(contest)
    );
    let row: Option<ContestStartRow> = host.db.query_one_with_args(&sql, &p.into_args())?;
    row.map(|r| r.start_ms as i64)
        .ok_or_else(|| SdkError::Other("contest not found".into()))
}

/// Start `match_id` now if it is due (see [`bracket::start_at_ms`]),
/// otherwise make sure its start timer fires when it will be. Called after
/// every ranking and from the start timer; idempotent and safe to race,
/// because the actual start re-checks the live document inside the
/// compare-and-set and only one caller can move it out of `Ordering`.
pub fn try_autostart(host: &Host, contest: i32, match_id: u8) -> Result<(), SdkError> {
    let Some(setup) = storage::load_setup(host, contest)? else {
        return Ok(());
    };
    let Some(current) = storage::load_match(host, contest, match_id)? else {
        return Ok(());
    };
    let matches = storage::load_all_matches(host, contest)?;
    let contest_start = contest_start_ms(host, contest)?;
    let Some(at) = bracket::start_at_ms(&setup, &matches, &current, contest_start) else {
        return Ok(());
    };
    let now = now_ms(host)?;
    if now < at {
        return host
            .timer
            .schedule(at, &start_timer_key(contest, match_id), "");
    }
    storage::update_match(host, contest, match_id, |m| {
        if bracket::start_at_ms(&setup, &matches, m, contest_start).is_none() {
            return Ok(()); // someone else started it, or it is no longer due
        }
        m.state = MatchPhase::InProgress;
        open_xiaoju(host, contest, match_id, &setup, now, m)
    })?;
    Ok(())
}

/// The problem `player` should currently be submitting to in match `m`,
/// mirroring `gate.rs::check`'s `expected` derivation exactly (see that
/// module's doc comment for why the two must never drift apart: this
/// decides who WINS a 小局, `gate.rs` decides what may be SUBMITTED to it,
/// and the two must agree on the current problem or a valid submission
/// could be judged against the wrong 小局).
fn current_problem(m: &MatchState, round_def: &RoundDef, player: i32) -> Option<i32> {
    match m.state {
        MatchPhase::Tiebreak => round_def.tiebreak.get(m.tiebreak_index).copied(),
        MatchPhase::InProgress => current_regular_problem(m, player),
        // A blocked 小局 freezes `m.xiaoju`/`m.tiebreak_index` exactly as
        // they were when the block began -- only `m.state` moved to
        // `AwaitingJudge`, which erases whether the blocked 小局 was
        // regular or an 附加赛 attempt. Recover that from the blocked
        // 小局's own INDEX instead (`decide::is_regular_xiaoju_index`), not
        // from `m.state`, since both `step` (re-deciding it) and
        // `fetch_subs` (re-querying its submissions) must keep resolving
        // to the SAME problem the block is actually about.
        MatchPhase::AwaitingJudge => {
            let index = m.xiaoju.last()?.index;
            if decide::is_regular_xiaoju_index(index) {
                current_regular_problem(m, player)
            } else {
                round_def.tiebreak.get(m.tiebreak_index).copied()
            }
        }
        _ => None,
    }
}

/// The problem `player` must solve at the currently open REGULAR 小局 (the
/// order imposed on them by their opponent, indexed by `m.xiaoju.last()`'s
/// index). Shared by [`current_problem`]'s `InProgress` arm and its
/// `AwaitingJudge` arm when the blocked 小局 turns out to be a regular one.
fn current_regular_problem(m: &MatchState, player: i32) -> Option<i32> {
    let order = if player == m.player_a {
        m.order_a
    } else if player == m.player_b {
        m.order_b
    } else {
        return None;
    };
    let index = m.xiaoju.last()?.index;
    order.and_then(|o| o.get(index as usize).copied())
}

#[derive(Debug, Deserialize)]
struct NowRow {
    now_ms: f64,
}

/// The current server time in Unix epoch milliseconds, via a `NOW()` query
/// -- following the codebase-wide `EXTRACT(EPOCH FROM ...) * 1000` raw-SQL
/// convention for epoch-ms columns (see `plugins/icpc/src/lib.rs`,
/// `plugins/cooldown/src/lib.rs`), which deserializes as `f64`, not `i64`.
/// `pub(crate)`: `routes::handle_start` (Task 10) needs the same "current
/// server time" reading to evaluate the round-intermission gate.
pub(crate) fn now_ms(host: &Host) -> Result<i64, SdkError> {
    let row: Option<NowRow> = host
        .db
        .query_one("SELECT EXTRACT(EPOCH FROM NOW()) * 1000 AS now_ms")?;
    let row = row.ok_or_else(|| SdkError::Other("NOW() query returned no row".into()))?;
    Ok(row.now_ms as i64)
}

#[derive(Debug, Deserialize)]
struct SubRow {
    id: i32,
    user_id: i32,
    problem_id: i32,
    submitted_at_us: i64,
    verdict: Option<Verdict>,
    status: SubmissionLifecycle,
}

/// Fetch every submission relevant to deciding `m`'s currently open 小局:
/// both players' submissions to their OWN current problem (see
/// [`current_problem`]), scoped to this contest. A player with no current
/// problem (e.g. between phases) contributes no clause and yields no rows
/// for that side.
///
/// Reads `status` alongside `verdict`: a submission whose judging failed
/// with a platform fault (`SystemError`) writes NO verdict (see
/// `mark_submission_system_error_with_epoch` and the stuck-job handler in
/// `packages/server`), so `verdict.is_none()` alone cannot tell "still being
/// judged" apart from "terminally failed, no verdict ever coming" -- see
/// `decide::is_in_flight`'s doc comment. `status::text AS status` follows
/// the codebase-wide convention for reading a Postgres enum column as plain
/// text (see `plugins/icpc/src/lib.rs`); it deserializes into
/// [`decide::SubmissionLifecycle`], not the host's own `SubmissionStatus`
/// (this WASM guest cannot depend on `packages/common` -- see that type's
/// doc comment).
fn fetch_subs(
    host: &Host,
    contest: i32,
    m: &MatchState,
    round_def: &RoundDef,
) -> Result<Vec<SubmissionRecord>, SdkError> {
    let prob_a = current_problem(m, round_def, m.player_a);
    let prob_b = current_problem(m, round_def, m.player_b);

    let mut p = Params::new();
    let mut clauses = Vec::new();
    if let Some(pid) = prob_a {
        clauses.push(format!(
            "(user_id = {} AND problem_id = {})",
            p.bind(m.player_a),
            p.bind(pid)
        ));
    }
    if let Some(pid) = prob_b {
        clauses.push(format!(
            "(user_id = {} AND problem_id = {})",
            p.bind(m.player_b),
            p.bind(pid)
        ));
    }
    if clauses.is_empty() {
        return Ok(Vec::new());
    }

    let sql = format!(
        "SELECT id, user_id, problem_id, \
         (EXTRACT(EPOCH FROM created_at) * 1000000)::bigint AS submitted_at_us, verdict, \
         status::text AS status \
         FROM submission WHERE contest_id = {} AND ({})",
        p.bind(contest),
        clauses.join(" OR ")
    );
    let rows: Vec<SubRow> = host.db.query_with_args(&sql, &p.into_args())?;
    Ok(rows
        .into_iter()
        .map(|r| SubmissionRecord {
            submission_id: r.id,
            user_id: r.user_id,
            problem_id: r.problem_id,
            submitted_at_us: r.submitted_at_us,
            verdict: r.verdict,
            status: r.status,
        })
        .collect())
}

/// Open the next 小局 (regular or 附加赛 -- both live in `m.xiaoju`, see
/// that field's doc comment) and schedule its deadline timer. `pub(crate)`
/// because Task 10's `routes::handle_start` also calls this directly, to
/// open the FIRST regular 小局 the moment a match leaves `Ordering`.
///
/// On this match's very FIRST call (`m.xiaoju_seconds == 0`, the "not yet
/// pinned" sentinel -- see that field's doc comment), the live
/// `setup.xiaoju_seconds` is snapshotted into `m.xiaoju_seconds` and used
/// for every 小局 this match opens from then on, including ones opened by
/// LATER calls after a `/setup` call has rewritten the shared `Setup`
/// document's `xiaoju_seconds`. This is what makes a second `/setup` call
/// safe to accept unconditionally instead of rejecting it (see
/// `setup::handle_setup`'s doc comment): the pacing a match actually
/// started under cannot be rewritten out from under it mid-play.
pub(crate) fn open_xiaoju(
    host: &Host,
    contest: i32,
    match_id: u8,
    setup: &Setup,
    now: i64,
    m: &mut MatchState,
) -> Result<(), SdkError> {
    if m.xiaoju_seconds == 0 {
        m.xiaoju_seconds = setup.xiaoju_seconds;
    }
    let index = m.xiaoju.len() as u8;
    let deadline_ms = now + m.xiaoju_seconds * 1_000;
    m.xiaoju.push(XiaojuState {
        index,
        opened_at_ms: now,
        deadline_ms,
        winner: None,
        decided: false,
    });
    host.timer
        .schedule(deadline_ms, &xiaoju_timer_key(contest, match_id, index), "")
}

/// Write a decided match's winner into its next-round feeder slot, and try
/// to create the next-round match if that was the second of its two
/// feeders. A no-op for a final-round match (no next round to feed).
fn write_next_round_slot(
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
    storage::create_match_if_both_slots_filled(host, contest, setup, next_round, m.pos >> 1)
}

/// The core per-attempt state transition, run inside `storage::update_match`'s
/// compare-and-set closure. Idempotent: `decide_xiaoju`/`decide_match` are
/// themselves idempotent (see `decide.rs`), so calling this redundantly --
/// which at-least-once timer delivery and overlapping triggers both cause in
/// practice -- never double-scores or double-advances.
fn step(
    host: &Host,
    contest: i32,
    match_id: u8,
    setup: &Setup,
    m: &mut MatchState,
) -> Result<(), SdkError> {
    if m.state != MatchPhase::InProgress
        && m.state != MatchPhase::Tiebreak
        && m.state != MatchPhase::AwaitingJudge
    {
        return Ok(());
    }
    let Some(current) = m.xiaoju.last() else {
        return Ok(());
    };
    let current_index = current.index;

    let Some(round_def) = setup.rounds.get(m.round.saturating_sub(1) as usize) else {
        // No round definition to resolve a current problem in -- cannot
        // safely proceed. `decide_match` independently makes the same
        // lookup and self-escalates to `NeedsAdjudication`.
        decide::decide_match(m, setup);
        return Ok(());
    };

    let now = now_ms(host)?;
    let subs = fetch_subs(host, contest, m, round_def)?;
    let outcome = decide::decide_xiaoju(m, &subs, now);

    match outcome {
        XiaojuOutcome::NotYet => Ok(()),
        XiaojuOutcome::AwaitingJudge {
            blocking_submission_id,
        } => {
            // The 小局's own deadline has already passed (that is the only
            // way `decide_xiaoju` returns this) -- that timer has either
            // already fired (this call IS its callback) or is about to;
            // either way it has done its job and must not linger.
            host.timer
                .cancel(&xiaoju_timer_key(contest, match_id, current_index))?;

            let entering_now = m.state != MatchPhase::AwaitingJudge;
            m.state = MatchPhase::AwaitingJudge;
            m.awaiting_submission_id = Some(blocking_submission_id);

            // Schedule the escalation timer only on FIRST entry. A later
            // re-delivery (a judging retry still in flight, a duplicate
            // trigger) must not keep pushing the grace period further out
            // -- see `tests::advance_is_idempotent_while_awaiting_judge_and_does_not_reschedule_escalation`.
            if entering_now {
                let escalate_at_ms = now + setup.escalation_grace_seconds * 1_000;
                host.timer.schedule(
                    escalate_at_ms,
                    &judgewait_timer_key(contest, match_id, current_index),
                    "",
                )?;
            }
            Ok(())
        }
        XiaojuOutcome::Decided { .. } => {
            // The 小局 that just concluded (or was already concluded by an
            // earlier, successful `step` -- see the module doc comment)
            // must have its deadline cancelled BEFORE anything opens next,
            // so a stale redelivery of THIS timer cannot land on the next
            // 小局.
            host.timer
                .cancel(&xiaoju_timer_key(contest, match_id, current_index))?;

            if m.state == MatchPhase::AwaitingJudge {
                // The block just resolved (the blocking submission's
                // verdict landed) -- the escalation timer must not fire
                // later and clobber whatever `decide_match` below produces.
                host.timer
                    .cancel(&judgewait_timer_key(contest, match_id, current_index))?;
                m.awaiting_submission_id = None;
                // `AwaitingJudge` erased whether this 小局 was regular or
                // an 附加赛 attempt (see `current_problem`'s doc comment on
                // the same problem) -- `decide_match` needs that restored
                // BEFORE it runs, since it branches on `m.state ==
                // Tiebreak` vs. everything else, and `AwaitingJudge` is
                // neither.
                m.state = if decide::is_regular_xiaoju_index(current_index) {
                    MatchPhase::InProgress
                } else {
                    MatchPhase::Tiebreak
                };
            }

            match decide::decide_match(m, setup) {
                MatchOutcome::NotYet | MatchOutcome::OpenTiebreak { .. } => {
                    open_xiaoju(host, contest, match_id, setup, now, m)
                }
                MatchOutcome::Decided { winner } => {
                    m.decided_at_ms = now;
                    write_next_round_slot(host, contest, setup, m, winner)
                }
                MatchOutcome::NeedsAdjudication => Ok(()),
            }
        }
    }
}

/// Drive match `match_id` forward from whatever state it is currently in.
/// The single convergence point for all three triggers described in the
/// module doc comment -- callers never touch `decide_xiaoju`/`decide_match`
/// directly.
///
/// A no-op if `/setup` has not run, or if `match_id` has never been created
/// -- the latter guard matters because `storage::update_match` (via
/// `Storage::modify`) treats a missing key as `MatchState::default()` and
/// would otherwise silently CREATE a bogus `Pending`-phase match document
/// for a stale or malformed timer/route call naming an id that was never
/// assigned.
pub fn advance(host: &Host, contest: i32, match_id: u8) -> Result<(), SdkError> {
    let Some(setup) = storage::load_setup(host, contest)? else {
        return Ok(());
    };
    if storage::load_match(host, contest, match_id)?.is_none() {
        return Ok(());
    }
    storage::update_match(host, contest, match_id, |m| {
        step(host, contest, match_id, &setup, m)
    })?;
    Ok(())
}

/// The escalation grace timer fired: `match_id` has been
/// `MatchPhase::AwaitingJudge` for longer than `Setup::escalation_grace_seconds`
/// without the blocking submission resolving. Moves it to
/// `MatchPhase::NeedsAdjudication` for staff to resolve (via the existing
/// `/force-decide` route, or by rejudging the submission named in
/// `awaiting_submission_id` through the platform's own rejudge endpoint --
/// this plugin deliberately does not grow a new capability to requeue a
/// submission itself, see the module's design notes).
///
/// A no-op if the match has since left `AwaitingJudge` by any other path
/// (the blocking submission's verdict landed and `step` already resolved
/// it, or staff force-decided it) -- a stale timer degenerating into a
/// harmless no-op, same discipline as a stale [`xiaoju_timer_key`] firing
/// (see the module doc comment). `pub(crate)`: only `on_timer` calls this
/// in production; tests call it directly since `on_timer` itself is
/// wasm32-gated.
///
/// Mirrors [`advance`]'s guard shape: a no-op (not an error) if `/setup`
/// has not run or `match_id` was never created, for the same
/// `Storage::modify`-defaulting reason documented there.
///
/// `pub` (not `pub(crate)`), matching [`advance`]/[`force_decide`]: its
/// only production caller is `on_timer`, which is `wasm32`-gated, so a
/// native (non-`wasm32`) build has no non-test caller to make a
/// `pub(crate)` item "used" -- see this crate's `["cdylib", "rlib"]`
/// crate-type, under which a fully `pub` item is exempt from the
/// `dead_code` lint as library API surface.
pub fn escalate(host: &Host, contest: i32, match_id: u8) -> Result<(), SdkError> {
    if storage::load_setup(host, contest)?.is_none() {
        return Ok(());
    }
    if storage::load_match(host, contest, match_id)?.is_none() {
        return Ok(());
    }
    storage::update_match(host, contest, match_id, |m| {
        if m.state != MatchPhase::AwaitingJudge {
            return Ok(());
        }
        m.state = MatchPhase::NeedsAdjudication;
        Ok(())
    })?;
    Ok(())
}

/// Resolve `(contest, user, problem)` -- as produced by a judged submission
/// -- to the match it belongs to, then [`advance`] it. A no-op if the
/// problem does not resolve to a round, or the user has no match in that
/// round (mirrors `gate.rs::resolve_rejection`'s same resolution, which is
/// deliberately not shared: that one REJECTS on failure to resolve, this
/// one simply has nothing to advance).
fn resolve_and_advance(
    host: &Host,
    contest_id: i32,
    user_id: i32,
    problem_id: i32,
) -> Result<(), SdkError> {
    let Some(setup) = storage::load_setup(host, contest_id)? else {
        return Ok(());
    };
    let Some(round_index) = setup.rounds.iter().position(|r| {
        r.group_a.contains(&problem_id)
            || r.group_b.contains(&problem_id)
            || r.tiebreak.contains(&problem_id)
    }) else {
        return Ok(());
    };
    let round = (round_index + 1) as u8;

    let matches = storage::load_all_matches(host, contest_id)?;
    let Some(m) = storage::find_players_match(&matches, round, user_id) else {
        return Ok(());
    };
    advance(host, contest_id, storage::match_id_for(m.round, m.pos))
}

/// The rejection message [`apply_force_decide`] returns once a match is
/// already `Decided`. A shared constant (rather than an inline literal) so
/// `routes::handle_force_decide` can pattern-match on it to distinguish "you
/// lost a race with another force-decide" from a genuine internal error when
/// this surfaces from inside [`force_decide`]'s CAS retry closure -- the
/// same technique `ordering::LEFT_ORDERING_PHASE_MSG` uses for
/// `ordering::handle_order`.
pub(crate) const ALREADY_DECIDED_MSG: &str = "this match has already been decided";

/// Pure precondition + mutation for a staff-forced match decision. `winner`
/// must be one of the two players, and the match must not already be
/// `Decided` -- a forced decision does not retroactively overturn a real
/// one, since that would corrupt whatever next-round slot the first
/// decision already wrote. Mirrors `ordering::record_order`'s pure-fallible
/// shape so `routes::handle_force_decide` can probe-validate a clone before
/// entering the CAS retry loop, exactly as `ordering::handle_order` does --
/// see that function's comment for why.
pub(crate) fn apply_force_decide(m: &mut MatchState, winner: i32, now: i64) -> Result<(), String> {
    if winner != m.player_a && winner != m.player_b {
        return Err(format!(
            "player {winner} is not a participant in this match"
        ));
    }
    if m.state == MatchPhase::Decided {
        return Err(ALREADY_DECIDED_MSG.to_string());
    }
    m.state = MatchPhase::Decided;
    m.winner = Some(winner);
    m.decided_at_ms = now;
    // A decided match is not "awaiting" anything -- clear it even if the
    // match was forced out of `AwaitingJudge`, so `GET /matches/{id}`
    // never shows a stale blocking-submission id next to a final result.
    m.awaiting_submission_id = None;
    Ok(())
}

/// Staff override: award `match_id` to `winner` directly. See the module
/// doc comment for why this is a genuinely separate path from [`advance`],
/// not an alternate way of calling it. Cancels whichever 小局 is currently
/// open (if any) so its stale deadline cannot fire after the forced
/// decision, matching `step`'s same discipline.
///
/// A no-op-with-error (not a silently-created bogus match) if `/setup` has
/// not run or `match_id` was never created -- see [`advance`]'s doc comment
/// on why `Storage::modify`'s missing-key-defaulting makes that guard
/// necessary. `routes::handle_force_decide` also checks existence itself
/// first (to report a proper 404 instead of this function's 500-mapped
/// `SdkError`), so this is defense in depth, not the only guard.
pub fn force_decide(host: &Host, contest: i32, match_id: u8, winner: i32) -> Result<(), SdkError> {
    let Some(setup) = storage::load_setup(host, contest)? else {
        return Err(SdkError::Other(
            "the bracket has not been set up yet".into(),
        ));
    };
    if storage::load_match(host, contest, match_id)?.is_none() {
        return Err(SdkError::Other("match not found".into()));
    }

    let updated = storage::update_match(host, contest, match_id, |m| {
        if let Some(current) = m.xiaoju.last() {
            let index = current.index;
            host.timer
                .cancel(&xiaoju_timer_key(contest, match_id, index))?;
            if m.state == MatchPhase::AwaitingJudge {
                host.timer
                    .cancel(&judgewait_timer_key(contest, match_id, index))?;
            }
        }
        let now = now_ms(host)?;
        apply_force_decide(m, winner, now).map_err(SdkError::Other)
    })?;
    write_next_round_slot(host, contest, &setup, &updated, winner)
}

/// Bracket judging policy for the shared detached-evaluate driver: binary
/// AC/non-AC, mirroring `plugins/icpc/src/evaluate.rs`'s `IcpcJudge`
/// exactly for `score`/`next_step` (a 小局 has exactly one problem to
/// solve, same as an ICPC problem: every test case must pass).
#[derive(Serialize, Deserialize)]
pub struct CodelinkBracketJudge;

impl ContestJudge for CodelinkBracketJudge {
    fn score(&self, result: &TestCaseVerdict) -> CaseOutcome {
        let score = if result.verdict == Verdict::Accepted {
            1.0
        } else {
            0.0
        };
        CaseOutcome::from_verdict(result, score)
    }

    fn next_step(&mut self, progress: &JudgeProgress<'_>) -> JudgeStep {
        match progress.last {
            Some(outcome) if outcome.verdict != Verdict::Accepted => JudgeStep::short_circuit(),
            _ => JudgeStep::Continue,
        }
    }

    fn finalize(&self, host: &Host, progress: &JudgeProgress<'_>) -> Result<(), SdkError> {
        // Adapted from `plugins/icpc/src/persist.rs::persist_and_track`
        // (read verbatim as the template for this): compute the terminal
        // verdict/status/score from the recorded outcomes and write them to
        // the submission row, gated on `judge_epoch` so a stale judgement
        // cannot clobber a newer one's result.
        let non_skipped: Vec<&CaseOutcome> = progress
            .outcomes
            .iter()
            .filter(|o| !o.verdict.is_skipped_or_cancelled())
            .collect();
        let verdict = if non_skipped.is_empty() {
            Verdict::SystemError
        } else {
            non_skipped
                .iter()
                .map(|o| o.verdict.clone())
                .max_by_key(|v| v.severity())
                .unwrap_or(Verdict::SystemError)
        };
        let max_time = non_skipped.iter().filter_map(|o| o.time_used).max();
        let max_memory = non_skipped.iter().filter_map(|o| o.memory_used).max();
        let is_ce = verdict == Verdict::CompileError;
        // Mirrors `persist_and_track`'s `is_accepted && !non_skipped.is_empty()`
        // gate: `verdict == Accepted` after the max-severity aggregation
        // above already implies every non-skipped outcome was Accepted AND
        // that the set was non-empty (an empty set forces `SystemError`,
        // never `Accepted`), so no separate emptiness check is needed here.
        let is_accepted = !is_ce && verdict == Verdict::Accepted;
        let score = if is_accepted { 1.0 } else { 0.0 };
        let status = if is_ce {
            SubmissionStatus::CompilationError
        } else {
            SubmissionStatus::Judged
        };
        let db_verdict = if is_ce { None } else { Some(verdict) };
        let compile_output = if is_ce {
            progress
                .outcomes
                .iter()
                .find(|o| o.verdict == Verdict::CompileError)
                .and_then(|o| o.message.clone())
        } else {
            None
        };

        let req = progress.request;
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: req.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(status),
            verdict: Some(db_verdict),
            score: Some(score),
            time_used: Some(max_time),
            memory_used: Some(max_memory),
            compile_output: Some(compile_output),
            error_code: None,
            error_message: None,
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }

        // A submission genuinely outside any contest has nothing to
        // advance; `unwrap_or_default()` would have silently treated it as
        // contest 0 instead.
        let Some(contest_id) = req.contest_id else {
            return Ok(());
        };
        resolve_and_advance(host, contest_id, req.user_id, req.problem_id)
    }
}

/// Start detached judging for a bracket submission. Mirrors
/// `plugins/icpc/src/lib.rs::run_judge` exactly, including its guard
/// against a misconfigured problem with zero test cases: crediting that as
/// `Accepted` would hand a free 小局 win to whoever submitted first, so it
/// is surfaced as a terminal `SystemError` instead of ever reaching the
/// judge.
pub fn evaluate_detached(
    host: &Host,
    req: &OnSubmissionInput,
) -> Result<OnSubmissionOutput, SdkError> {
    if req.test_cases.is_empty() {
        let _ = host
            .log
            .info("codelink-bracket: No test cases found; marking as SystemError (not a solve)");
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: req.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::SystemError)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            compile_output: None,
            error_code: Some(Some("NO_TEST_CASES".to_string())),
            error_message: Some(Some(
                "Problem has no test cases; cannot be judged".to_string(),
            )),
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }
        return Ok(OnSubmissionOutput {
            success: true,
            error_message: None,
        });
    }

    DetachedEval::start(
        host,
        req,
        &req.test_cases,
        CodelinkBracketJudge,
        "on_codelink_bracket_eval_result",
        1,
    )?;
    Ok(OnSubmissionOutput {
        success: true,
        error_message: None,
    })
}

/// Handle one detached-evaluate result callback. Mirrors
/// `plugins/icpc/src/evaluate.rs::handle_detached_eval_callback`.
pub fn handle_eval_result_callback(
    host: &Host,
    input: DetachedEvaluateCallbackInput,
) -> Result<DetachedEvaluateCallbackOutput, SdkError> {
    DetachedEval::<CodelinkBracketJudge>::handle_callback(host, input)
}

/// Best-effort recovery when a detached callback fails mid-stream. Mirrors
/// `plugins/icpc/src/evaluate.rs::recover_detached_callback_error`.
pub fn recover_eval_callback_error(host: &Host, state_value: &serde_json::Value) {
    DetachedEval::<CodelinkBracketJudge>::recover(host, state_value);
}

/// `handle_codelink_bracket_submission` / `handle_codelink_bracket_code_run`
/// plugin_fn entry points, registered by name via `host.registry.register_contest_type`
/// in `lib.rs::init`. Live here (not `lib.rs`) following the same convention
/// as `gate.rs::check_submission` -- a `#[plugin_fn]` needs no re-export,
/// only the enclosing module declared `pub mod judge;` so it compiles into
/// the crate. Mirrors `plugins/icpc/src/lib.rs::handle_icpc_submission`
/// exactly, minus the standalone/practice-mode branch: every codelink-bracket
/// submission belongs to a contest, and [`CodelinkBracketJudge::finalize`]'s
/// `resolve_and_advance` already no-ops gracefully on a missing `contest_id`.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_codelink_bracket_submission(input: String) -> FnResult<String> {
    let host = Host::new();
    let req: OnSubmissionInput = serde_json::from_str(&input)?;
    let output = match evaluate_detached(&host, &req) {
        Ok(out) => out,
        Err(SdkError::StaleEpoch) => OnSubmissionOutput {
            success: true,
            error_message: None,
        },
        Err(e) => OnSubmissionOutput {
            success: false,
            error_message: Some(format!("{e:?}")),
        },
    };
    Ok(serde_json::to_string(&output)?)
}

#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_codelink_bracket_code_run(input: String) -> FnResult<String> {
    let host = Host::new();
    Ok(broccoli_server_sdk::evaluator::handle_code_run(
        &host, &input,
    )?)
}

/// The detached-evaluate result callback, named by [`evaluate_detached`]'s
/// `DetachedEval::start` call. Mirrors `plugins/icpc/src/lib.rs::on_icpc_eval_result`
/// exactly: a stale epoch cancels quietly, any other failure best-effort
/// recovers the in-flight session before also cancelling.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn on_codelink_bracket_eval_result(input: String) -> FnResult<String> {
    let host = Host::new();
    let input: DetachedEvaluateCallbackInput = serde_json::from_str(&input)?;
    let state_snapshot = input.state.clone();
    let output = match handle_eval_result_callback(&host, input) {
        Ok(out) => out,
        Err(SdkError::StaleEpoch) => {
            let _ = host
                .log
                .info("codelink-bracket: detached callback epoch stale, cancelling");
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
        Err(e) => {
            let _ = host.log.info(&format!(
                "codelink-bracket: detached callback failed ({e:?}); finalizing as SystemError"
            ));
            recover_eval_callback_error(&host, &state_snapshot);
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
    };
    Ok(serde_json::to_string(&output)?)
}

/// `[[server.timers]] function = "on_timer"` entry point (see `lib.rs`).
/// Parses `(contest, match_id)` out of the fired timer's key and calls
/// [`advance`] -- never trusts the timer to still describe the CURRENT
/// state (see the module doc comment on stale-timer cancellation).
#[cfg(target_arch = "wasm32")]
#[extism_pdk::plugin_fn]
pub fn on_timer(input: String) -> extism_pdk::FnResult<String> {
    #[derive(Deserialize)]
    struct TimerCallbackInput {
        key: String,
    }
    let host = Host::new();
    let input: TimerCallbackInput = serde_json::from_str(&input)?;
    if let Some((contest, match_id)) = parse_xiaoju_timer_key(&input.key) {
        if let Err(e) = advance(&host, contest, match_id) {
            let _ = host.log.info(&format!(
                "codelink-bracket: on_timer advance failed for key {}: {e:?}",
                input.key
            ));
        }
    } else if let Some((contest, match_id)) = parse_start_timer_key(&input.key) {
        if let Err(e) = try_autostart(&host, contest, match_id) {
            let _ = host.log.info(&format!(
                "codelink-bracket: on_timer autostart failed for key {}: {e:?}",
                input.key
            ));
        }
    } else if let Some((contest, match_id)) = parse_judgewait_timer_key(&input.key) {
        if let Err(e) = escalate(&host, contest, match_id) {
            let _ = host.log.info(&format!(
                "codelink-bracket: on_timer escalate failed for key {}: {e:?}",
                input.key
            ));
        }
    } else {
        let _ = host.log.info(&format!(
            "codelink-bracket: on_timer got an unparseable key: {}",
            input.key
        ));
    }
    Ok("ok".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RoundDef;

    fn setup_two_rounds() -> Setup {
        Setup {
            rounds: vec![
                RoundDef {
                    group_a: [101, 102, 103],
                    group_b: [201, 202, 203],
                    tiebreak: vec![301, 302],
                },
                RoundDef {
                    group_a: [401, 402, 403],
                    group_b: [501, 502, 503],
                    tiebreak: vec![601],
                },
            ],
            xiaoju_seconds: 1_800,
            round_intermission_seconds: 600,
            escalation_grace_seconds: 120,
        }
    }

    fn match_in_progress() -> MatchState {
        MatchState {
            round: 1,
            pos: 0,
            player_a: 10,
            player_b: 20,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            state: MatchPhase::InProgress,
            xiaoju: vec![XiaojuState {
                index: 0,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: false,
            }],
            ..Default::default()
        }
    }

    fn queue_now(host: &Host, now: i64) {
        host.db
            .queue_query_result(serde_json::json!([{ "now_ms": now as f64 }]));
    }

    fn queue_subs(host: &Host, rows: Vec<serde_json::Value>) {
        host.db.queue_query_result(serde_json::Value::Array(rows));
    }

    fn ac_row(user_id: i32, submitted_at_ms: i64) -> serde_json::Value {
        serde_json::json!({
            "id": user_id,
            "user_id": user_id,
            "problem_id": 0,
            "submitted_at_us": submitted_at_ms * 1000,
            "verdict": "Accepted",
            "status": "Judged",
        })
    }

    /// Like `ac_row`, but with an explicit submission id, verdict, and
    /// status -- needed to construct `SystemError`/still-in-flight rows,
    /// and rows whose blocked-then-resolved id must be pinned to a value
    /// distinct from `user_id` (`ac_row` conflates the two).
    fn row_with_status(
        id: i32,
        user_id: i32,
        submitted_at_ms: i64,
        verdict: Option<&str>,
        status: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "user_id": user_id,
            "problem_id": 0,
            "submitted_at_us": submitted_at_ms * 1000,
            "verdict": verdict,
            "status": status,
        })
    }

    // -- xiaoju_timer_key / parse_xiaoju_timer_key --

    #[test]
    fn xiaoju_timer_key_round_trips_through_parse() {
        let key = xiaoju_timer_key(7, 3, 1);
        assert_eq!(key, "xiaoju:7:3:1");
        assert_eq!(parse_xiaoju_timer_key(&key), Some((7, 3)));
    }

    #[test]
    fn parse_xiaoju_timer_key_rejects_a_foreign_key() {
        assert_eq!(parse_xiaoju_timer_key("setup:7"), None);
        assert_eq!(parse_xiaoju_timer_key("xiaoju:7:3"), None, "missing index");
        assert_eq!(parse_xiaoju_timer_key("xiaoju:not-a-number:3:0"), None);
    }

    #[test]
    fn a_timer_key_carries_no_authority_over_which_xiaoju_is_decided() {
        // Load-bearing invariant, pinned here because the code that relies on
        // it is not obviously connected to it.
        //
        // `Storage::modify` re-runs its whole closure on a CAS retry, side
        // effects included. So two concurrent triggers on one match can leave
        // an ORPHANED timer: attempt 1 schedules `xiaoju:7:3:1`, its CAS
        // loses, and the retry (seeing fresher state) schedules
        // `xiaoju:7:3:2` instead - with nothing having cancelled key `:1`.
        // That stale timer fires later, while the match is on a different
        // 小局.
        //
        // It is harmless ONLY because the index segment is inert: `on_timer`
        // extracts `(contest, match_id)` and discards the index, then calls
        // `advance`, which re-derives the current 小局 and its deadline from
        // stored state. A stale timer therefore degenerates into an extra
        // no-op `advance`.
        //
        // If anyone ever "improves" the parser to return the index and acts
        // on it, orphaned timers stop being harmless and start deciding the
        // wrong 小局. This test is what should fail first.
        let for_xiaoju_0 = parse_xiaoju_timer_key("xiaoju:7:3:0");
        let for_xiaoju_2 = parse_xiaoju_timer_key("xiaoju:7:3:2");

        assert_eq!(
            for_xiaoju_0, for_xiaoju_2,
            "two keys differing only in 小局 index must be indistinguishable \
             to the callback - the index must not reach any decision"
        );
        assert_eq!(for_xiaoju_0, Some((7, 3)));
    }

    // -- judgewait_timer_key / parse_judgewait_timer_key --

    #[test]
    fn judgewait_timer_key_round_trips_through_parse() {
        let key = judgewait_timer_key(7, 3, 1);
        assert_eq!(key, "judgewait:7:3:1");
        assert_eq!(parse_judgewait_timer_key(&key), Some((7, 3)));
    }

    #[test]
    fn parse_judgewait_timer_key_rejects_a_foreign_key() {
        // In particular, a `xiaoju:` key must not parse as a `judgewait:`
        // one -- the two namespaces must never collide (see the module doc
        // comment's "Timer key discipline" section).
        assert_eq!(parse_judgewait_timer_key("xiaoju:7:3:0"), None);
        assert_eq!(
            parse_judgewait_timer_key("judgewait:7:3"),
            None,
            "missing index"
        );
        assert_eq!(
            parse_judgewait_timer_key("judgewait:not-a-number:3:0"),
            None
        );
    }

    // -- current_problem --

    #[test]
    fn current_problem_uses_the_imposed_order_at_the_open_xiaoju_index() {
        let round_def = &setup_two_rounds().rounds[0];
        let m = match_in_progress();
        assert_eq!(current_problem(&m, round_def, 10), Some(103));
        assert_eq!(current_problem(&m, round_def, 20), Some(203));
    }

    #[test]
    fn current_problem_is_none_for_a_non_participant() {
        let round_def = &setup_two_rounds().rounds[0];
        let m = match_in_progress();
        assert_eq!(current_problem(&m, round_def, 999), None);
    }

    #[test]
    fn current_problem_during_tiebreak_is_the_shared_tiebreak_problem() {
        let round_def = &setup_two_rounds().rounds[0];
        let mut m = match_in_progress();
        m.state = MatchPhase::Tiebreak;
        m.tiebreak_index = 1;
        assert_eq!(current_problem(&m, round_def, 10), Some(302));
        assert_eq!(current_problem(&m, round_def, 20), Some(302));
    }

    #[test]
    fn current_problem_while_awaiting_judge_on_a_regular_xiaoju_uses_the_imposed_order() {
        // A match blocked mid-regular-小局 must still resolve to the SAME
        // problem it was blocked on -- `fetch_subs` needs this to re-check
        // whether the blocking submission's verdict has landed yet.
        let round_def = &setup_two_rounds().rounds[0];
        let mut m = match_in_progress();
        m.state = MatchPhase::AwaitingJudge;
        assert_eq!(current_problem(&m, round_def, 10), Some(103));
        assert_eq!(current_problem(&m, round_def, 20), Some(203));
    }

    #[test]
    fn current_problem_while_awaiting_judge_on_a_tiebreak_uses_the_shared_tiebreak_problem() {
        // `m.state` alone can no longer distinguish "blocked mid-regular"
        // from "blocked mid-tiebreak" once both collapse to
        // `AwaitingJudge` -- this must fall back to the blocked 小局's own
        // INDEX (`decide::is_regular_xiaoju_index`), not `m.state`.
        let round_def = &setup_two_rounds().rounds[0];
        let mut m = match_in_progress();
        m.state = MatchPhase::AwaitingJudge;
        m.tiebreak_index = 1;
        m.xiaoju = vec![XiaojuState {
            index: 3,
            opened_at_ms: 0,
            deadline_ms: 1_000_000,
            winner: None,
            decided: false,
        }];
        assert_eq!(current_problem(&m, round_def, 10), Some(302));
        assert_eq!(current_problem(&m, round_def, 20), Some(302));
    }

    // -- now_ms --

    #[test]
    fn now_ms_reads_the_queued_row() {
        let host = Host::mock();
        queue_now(&host, 1_234_567);
        assert_eq!(now_ms(&host).unwrap(), 1_234_567);
    }

    // -- open_xiaoju --

    #[test]
    fn open_xiaoju_appends_a_xiaoju_and_schedules_its_deadline() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.xiaoju.clear();

        open_xiaoju(&host, 7, 0, &setup, 1_000, &mut m).unwrap();

        assert_eq!(m.xiaoju.len(), 1);
        assert_eq!(m.xiaoju[0].index, 0);
        assert_eq!(m.xiaoju[0].opened_at_ms, 1_000);
        assert_eq!(m.xiaoju[0].deadline_ms, 1_000 + 1_800_000);
        assert!(!m.xiaoju[0].decided);
        let key = xiaoju_timer_key(7, 0, 0);
        assert!(host.timer.is_scheduled(&key));
        assert_eq!(host.timer.scheduled_at(&key), Some(1_000 + 1_800_000));
    }

    #[test]
    fn open_xiaoju_pins_xiaoju_seconds_on_first_open_and_ignores_a_later_setup_change() {
        // Defect 3 fix: the FIRST open_xiaoju call for a match snapshots the
        // live setup.xiaoju_seconds into m.xiaoju_seconds, and every LATER
        // call for the SAME match keeps using that pinned value even if the
        // caller passes in a `Setup` whose xiaoju_seconds has since changed
        // -- exactly what a second `/setup` call mid-contest would do.
        let host = Host::mock();
        let mut setup = setup_two_rounds();
        setup.xiaoju_seconds = 3;
        let mut m = match_in_progress();
        m.xiaoju.clear();
        m.xiaoju_seconds = 0; // sentinel: not yet pinned

        open_xiaoju(&host, 7, 0, &setup, 1_000, &mut m).unwrap();
        assert_eq!(
            m.xiaoju_seconds, 3,
            "the first open should pin the then-live setup value"
        );
        assert_eq!(m.xiaoju[0].deadline_ms, 1_000 + 3_000);

        // A second /setup call rewrites the shared Setup document...
        let mut reconfigured = setup.clone();
        reconfigured.xiaoju_seconds = 300;

        // ...but opening this match's NEXT 小局 must still use the 3s it
        // actually started under, not the new 300s.
        open_xiaoju(&host, 7, 0, &reconfigured, 2_000, &mut m).unwrap();
        assert_eq!(
            m.xiaoju_seconds, 3,
            "a pinned value must not change on later opens"
        );
        assert_eq!(m.xiaoju[1].deadline_ms, 2_000 + 3_000);
    }

    #[test]
    fn open_xiaoju_negative_control_an_unstarted_match_still_picks_up_a_setup_correction() {
        // Negative control for the pin above: a match with xiaoju_seconds
        // still 0 (never opened a 小局) must pick up whatever the CURRENT
        // `Setup` says, including a value corrected by a second `/setup`
        // call that ran before this match started -- the "still allowed to
        // correct a bracket nobody has started playing yet" half of the
        // rule documented on `setup::handle_setup`.
        let host = Host::mock();
        let mut corrected_setup = setup_two_rounds();
        corrected_setup.xiaoju_seconds = 120; // staff's corrected value

        let mut m = match_in_progress();
        m.xiaoju.clear();
        m.xiaoju_seconds = 0; // sentinel: never opened a 小局 yet

        open_xiaoju(&host, 7, 0, &corrected_setup, 1_000, &mut m).unwrap();
        assert_eq!(
            m.xiaoju_seconds, 120,
            "an unstarted match must pick up the corrected setup value"
        );
        assert_eq!(m.xiaoju[0].deadline_ms, 1_000 + 120_000);
    }

    // -- advance / step: guards --

    #[test]
    fn advance_is_a_noop_when_setup_has_not_run() {
        let host = Host::mock();
        // No setup, no match -- must not error or create anything.
        advance(&host, 7, 0).unwrap();
        assert!(storage::load_match(&host, 7, 0).unwrap().is_none());
    }

    #[test]
    fn advance_is_a_noop_for_a_match_id_that_was_never_created() {
        let host = Host::mock();
        host.storage
            .set(&[(
                storage::setup_key(7).as_str(),
                serde_json::to_string(&setup_two_rounds()).unwrap().as_str(),
            )])
            .unwrap();

        advance(&host, 7, 5).unwrap();

        // `Storage::modify` would otherwise have materialised a bogus
        // default `MatchState` at this key.
        assert!(storage::load_match(&host, 7, 5).unwrap().is_none());
    }

    #[test]
    fn advance_is_a_noop_for_a_match_that_is_not_in_progress_or_tiebreak() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        host.storage
            .set(&[(
                storage::setup_key(7).as_str(),
                serde_json::to_string(&setup).unwrap().as_str(),
            )])
            .unwrap();
        let mut m = match_in_progress();
        m.state = MatchPhase::Ordering;
        m.xiaoju.clear();
        host.storage
            .set(&[(
                storage::match_key(7, 0).as_str(),
                serde_json::to_string(&m).unwrap().as_str(),
            )])
            .unwrap();

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Ordering);
    }

    // -- step, via advance: the core state machine --

    fn seed(host: &Host, contest: i32, setup: &Setup, match_id: u8, m: &MatchState) {
        host.storage
            .set(&[(
                storage::setup_key(contest).as_str(),
                serde_json::to_string(setup).unwrap().as_str(),
            )])
            .unwrap();
        host.storage
            .set(&[(
                storage::match_key(contest, match_id).as_str(),
                serde_json::to_string(m).unwrap().as_str(),
            )])
            .unwrap();
    }

    #[test]
    fn deciding_a_xiaoju_early_cancels_its_timer_before_scheduling_the_next() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress());

        queue_now(&host, 500_000);
        queue_subs(&host, vec![ac_row(10, 100)]);

        advance(&host, 7, 0).unwrap();

        let old_key = xiaoju_timer_key(7, 0, 0);
        assert!(
            host.timer.was_cancelled(&old_key),
            "the just-decided 小局's timer must be cancelled"
        );
        let new_key = xiaoju_timer_key(7, 0, 1);
        assert!(
            host.timer.is_scheduled(&new_key),
            "the next 小局's timer must be scheduled"
        );

        let m = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(m.score_a, 1);
        assert_eq!(m.xiaoju.len(), 2);
        assert!(m.xiaoju[0].decided);
        assert!(!m.xiaoju[1].decided);
    }

    #[test]
    fn advance_is_idempotent_when_called_twice_with_no_new_submissions() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress());

        queue_now(&host, 500_000);
        queue_subs(&host, vec![ac_row(10, 100)]);
        advance(&host, 7, 0).unwrap();
        let after_first = storage::load_match(&host, 7, 0).unwrap().unwrap();

        // Second call: nothing new queued for the (now-open) second 小局,
        // and no submission rows either -- decide_xiaoju must return
        // NotYet, not re-score.
        queue_now(&host, 500_100);
        queue_subs(&host, vec![]);
        advance(&host, 7, 0).unwrap();
        let after_second = storage::load_match(&host, 7, 0).unwrap().unwrap();

        assert_eq!(after_first, after_second);
    }

    #[test]
    fn deciding_the_third_xiaoju_with_a_higher_score_decides_the_match_and_writes_the_slot() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.score_a = 1;
        m.score_b = 0;
        m.xiaoju = vec![
            XiaojuState {
                index: 0,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: Some(10),
                decided: true,
            },
            XiaojuState {
                index: 1,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: true,
            },
            XiaojuState {
                index: 2,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: false,
            },
        ];
        seed(&host, 7, &setup, 0, &m);

        // The third 小局's deadline is 1_000_000: `now` must reach it (not
        // merely approach it -- `decide_xiaoju` only decides a scoreless
        // 小局 once `now_ms >= deadline_ms`) for it to be decided scoreless.
        queue_now(&host, 1_000_000);
        // Neither player accepted the third problem before the deadline.
        queue_subs(&host, vec![]);

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Decided);
        assert_eq!(reloaded.winner, Some(10));
        assert_eq!(reloaded.decided_at_ms, 1_000_000);

        let slot = host
            .storage
            .get(&[storage::slot_key(7, 2, 0).as_str()])
            .unwrap();
        assert_eq!(
            slot.get(&storage::slot_key(7, 2, 0)),
            Some(&"10".to_string())
        );
    }

    #[test]
    fn a_level_score_after_the_third_xiaoju_opens_a_tiebreak() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.score_a = 1;
        m.score_b = 1;
        m.xiaoju = vec![
            XiaojuState {
                index: 0,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: Some(10),
                decided: true,
            },
            XiaojuState {
                index: 1,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: Some(20),
                decided: true,
            },
            XiaojuState {
                index: 2,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: false,
            },
        ];
        seed(&host, 7, &setup, 0, &m);

        // See the sibling test above: `now` must reach the third 小局's
        // 1_000_000 deadline for it to be decided scoreless.
        queue_now(&host, 1_000_000);
        queue_subs(&host, vec![]);

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Tiebreak);
        assert_eq!(reloaded.tiebreak_index, 0);
        assert_eq!(reloaded.xiaoju.len(), 4);
        assert_eq!(reloaded.xiaoju[3].index, 3);
    }

    #[test]
    fn deciding_the_final_round_match_does_not_write_a_slot() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.round = 2;
        m.score_a = 0;
        m.score_b = 1;
        m.xiaoju = vec![
            XiaojuState {
                index: 0,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: Some(20),
                decided: true,
            },
            XiaojuState {
                index: 1,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: true,
            },
            XiaojuState {
                index: 2,
                opened_at_ms: 0,
                deadline_ms: 1_000_000,
                winner: None,
                decided: false,
            },
        ];
        seed(&host, 7, &setup, 0, &m);

        // See the sibling tests above: `now` must reach the third 小局's
        // 1_000_000 deadline for it to be decided scoreless.
        queue_now(&host, 1_000_000);
        queue_subs(&host, vec![]);

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Decided);
        // Round 2 is the last round in `setup_two_rounds()` -- no round 3
        // slot should exist.
        let slot = host
            .storage
            .get(&[storage::slot_key(7, 3, 0).as_str()])
            .unwrap();
        assert!(slot.is_empty());
    }

    #[test]
    fn exhausting_the_tiebreak_list_without_a_decision_escalates_to_needs_adjudication() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::Tiebreak;
        m.tiebreak_index = 0; // round 1's only extra tiebreak problem is index 1
        m.xiaoju = vec![XiaojuState {
            index: 3,
            opened_at_ms: 0,
            deadline_ms: 1_000_000,
            winner: None,
            decided: false,
        }];
        seed(&host, 7, &setup, 0, &m);

        // First attempt: scoreless -> advances to the next tiebreak problem.
        queue_now(&host, 1_000_000);
        queue_subs(&host, vec![]);
        advance(&host, 7, 0).unwrap();
        let after_first = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(after_first.state, MatchPhase::Tiebreak);
        assert_eq!(after_first.tiebreak_index, 1);

        // Second attempt (the list has only 2 entries): scoreless again ->
        // no further tiebreak problem exists. The 4th 小局 that
        // `open_xiaoju` scheduled after the first attempt has a deadline of
        // `1_000_000 + xiaoju_seconds * 1000` (1_800_000), i.e. 2_800_000 --
        // `now` must reach that too.
        queue_now(&host, 2_800_000);
        queue_subs(&host, vec![]);
        advance(&host, 7, 0).unwrap();
        let after_second = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(after_second.state, MatchPhase::NeedsAdjudication);
    }

    // -- step, via advance: AwaitingJudge (platform-fault escalation policy) --

    #[test]
    fn advance_enters_awaiting_judge_when_the_deadline_passes_while_blocked_and_schedules_escalation()
     {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress()); // deadline_ms = 1_000_000

        queue_now(&host, 1_000_000); // deadline reached
        queue_subs(
            &host,
            vec![
                // Blocker: older than the AC below, still in flight.
                row_with_status(1, 10, 100, None, "SystemError"),
                ac_row(20, 500),
            ],
        );

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::AwaitingJudge);
        assert_eq!(reloaded.awaiting_submission_id, Some(1));

        assert!(
            host.timer.was_cancelled(&xiaoju_timer_key(7, 0, 0)),
            "the deadline timer that just fired has done its job"
        );
        let escalation_key = judgewait_timer_key(7, 0, 0);
        assert!(host.timer.is_scheduled(&escalation_key));
        assert_eq!(
            host.timer.scheduled_at(&escalation_key),
            Some(1_000_000 + 120_000),
            "escalation fires escalation_grace_seconds (120) after now, not after the original deadline"
        );
    }

    #[test]
    fn advance_is_idempotent_while_awaiting_judge_and_does_not_reschedule_escalation() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::AwaitingJudge;
        m.awaiting_submission_id = Some(1);
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 1_000_100);
        queue_subs(
            &host,
            vec![
                row_with_status(1, 10, 100, None, "SystemError"),
                ac_row(20, 500),
            ],
        );
        advance(&host, 7, 0).unwrap();

        // A second, later re-delivery must not push the escalation timer
        // further out -- otherwise a platform fault that keeps getting
        // re-observed (e.g. every judging retry) could delay escalation
        // indefinitely, defeating the grace period entirely.
        let escalation_key = judgewait_timer_key(7, 0, 0);
        let scheduled_after_first = host.timer.scheduled_at(&escalation_key);

        queue_now(&host, 1_000_200);
        queue_subs(
            &host,
            vec![
                row_with_status(1, 10, 100, None, "SystemError"),
                ac_row(20, 500),
            ],
        );
        advance(&host, 7, 0).unwrap();

        assert_eq!(
            host.timer.scheduled_at(&escalation_key),
            scheduled_after_first
        );
        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::AwaitingJudge);
    }

    #[test]
    fn advance_resolves_out_of_awaiting_judge_once_the_blocking_verdict_lands_and_the_earlier_submitter_wins()
     {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::AwaitingJudge;
        m.awaiting_submission_id = Some(1);
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 1_000_500);
        // The blocker's verdict has landed: accepted, and still earlier
        // than the other player's submission -- per the earliest-submitted
        // rule, player 10 must win even though player 20's AC was
        // provisionally ahead while blocked.
        queue_subs(
            &host,
            vec![
                row_with_status(1, 10, 100, Some("Accepted"), "Judged"),
                row_with_status(2, 20, 500, Some("Accepted"), "Judged"),
            ],
        );

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.score_a, 1);
        assert_eq!(reloaded.score_b, 0);
        assert_eq!(reloaded.xiaoju[0].winner, Some(10));
        assert!(
            host.timer.was_cancelled(&judgewait_timer_key(7, 0, 0)),
            "the escalation timer must not fire after the block resolved"
        );
        // Only the first of 3 regular 小局 is decided -- the match opens the
        // next one and stays InProgress, exactly like the non-blocked path.
        assert_eq!(reloaded.state, MatchPhase::InProgress);
        assert!(host.timer.is_scheduled(&xiaoju_timer_key(7, 0, 1)));
    }

    #[test]
    fn advance_resolves_out_of_awaiting_judge_during_a_tiebreak_back_into_tiebreak() {
        // Same as the regular-小局 case above, but the blocked 小局 was an
        // 附加赛 attempt: `m.state` must be restored to `Tiebreak`, not
        // `InProgress`, before `decide_match` runs -- otherwise `decide_match`
        // would (wrongly) evaluate this as if only 1 of 3 regular 小局 were
        // ever recorded.
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.score_a = 1;
        m.score_b = 1;
        m.state = MatchPhase::AwaitingJudge;
        m.awaiting_submission_id = Some(1);
        m.tiebreak_index = 0;
        m.xiaoju = vec![XiaojuState {
            index: 3,
            opened_at_ms: 0,
            deadline_ms: 1_000_000,
            winner: None,
            decided: false,
        }];
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 1_000_500);
        queue_subs(
            &host,
            vec![
                row_with_status(1, 10, 100, Some("Accepted"), "Judged"),
                row_with_status(2, 20, 500, Some("Accepted"), "Judged"),
            ],
        );

        advance(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Decided);
        assert_eq!(reloaded.winner, Some(10));
    }

    // -- escalate --

    #[test]
    fn escalate_moves_a_still_blocked_match_to_needs_adjudication() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::AwaitingJudge;
        m.awaiting_submission_id = Some(1);
        seed(&host, 7, &setup, 0, &m);

        escalate(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::NeedsAdjudication);
    }

    #[test]
    fn escalate_is_a_noop_when_the_block_has_already_cleared() {
        // Stale timer: the match resolved out of `AwaitingJudge` (a verdict
        // landing, or a staff force-decide) before the escalation grace
        // period elapsed. Same inertness discipline as a stale
        // `xiaoju_timer_key` firing -- see the module doc comment.
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress()); // state: InProgress

        escalate(&host, 7, 0).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::InProgress);
    }

    #[test]
    fn escalate_is_a_noop_for_a_match_id_that_was_never_created() {
        let host = Host::mock();
        host.storage
            .set(&[(
                storage::setup_key(7).as_str(),
                serde_json::to_string(&setup_two_rounds()).unwrap().as_str(),
            )])
            .unwrap();
        escalate(&host, 7, 5).unwrap();
        assert!(storage::load_match(&host, 7, 5).unwrap().is_none());
    }

    // -- resolve_and_advance --

    #[test]
    fn resolve_and_advance_is_a_noop_for_a_problem_outside_the_bracket() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress());
        // Problem 999 belongs to no round -- must not error.
        resolve_and_advance(&host, 7, 10, 999).unwrap();
    }

    #[test]
    fn resolve_and_advance_finds_the_players_match_and_drives_it() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress());

        queue_now(&host, 500_000);
        queue_subs(&host, vec![ac_row(10, 100)]);

        // Player 10's current problem in round 1 is 103 (see
        // `match_in_progress`'s `order_a`).
        resolve_and_advance(&host, 7, 10, 103).unwrap();

        let m = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(m.score_a, 1);
    }

    // -- apply_force_decide / force_decide --

    #[test]
    fn apply_force_decide_rejects_a_winner_who_is_not_a_participant() {
        let mut m = match_in_progress();
        let err = apply_force_decide(&mut m, 999, 1_000).unwrap_err();
        assert!(err.contains("not a participant"), "got: {err}");
        assert_eq!(
            m.state,
            MatchPhase::InProgress,
            "a rejection must not mutate state"
        );
    }

    #[test]
    fn apply_force_decide_rejects_a_match_that_is_already_decided() {
        let mut m = match_in_progress();
        m.state = MatchPhase::Decided;
        m.winner = Some(10);
        let err = apply_force_decide(&mut m, 20, 1_000).unwrap_err();
        assert!(err.contains("already been decided"), "got: {err}");
    }

    #[test]
    fn apply_force_decide_awards_the_match_and_stamps_decided_at_ms() {
        let mut m = match_in_progress();
        apply_force_decide(&mut m, 20, 42_000).unwrap();
        assert_eq!(m.state, MatchPhase::Decided);
        assert_eq!(m.winner, Some(20));
        assert_eq!(m.decided_at_ms, 42_000);
    }

    #[test]
    fn force_decide_resolves_needs_adjudication_and_writes_the_next_round_slot() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::NeedsAdjudication;
        m.xiaoju.clear(); // no 小局 open -- nothing for force_decide to cancel
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 5_000_000);
        force_decide(&host, 7, 0, 10).unwrap();

        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Decided);
        assert_eq!(reloaded.winner, Some(10));
        assert_eq!(reloaded.decided_at_ms, 5_000_000);

        let slot = host
            .storage
            .get(&[storage::slot_key(7, 2, 0).as_str()])
            .unwrap();
        assert_eq!(
            slot.get(&storage::slot_key(7, 2, 0)),
            Some(&"10".to_string())
        );
    }

    #[test]
    fn force_decide_surfaces_the_already_decided_message_when_it_loses_a_race() {
        // Simulates the losing side of a concurrent force-decide (two staff
        // clicking "force decide" on the same match near-simultaneously, or
        // a retried request): by the time `force_decide`'s CAS closure
        // reloads the LATEST state, another writer has already decided the
        // match. `force_decide` must surface EXACTLY
        // `SdkError::Other(ALREADY_DECIDED_MSG)` (not some other message
        // shape) for `routes::map_force_decide_error` to be able to map it
        // to a 409 instead of letting it leak through as a bare 500 via the
        // generic `SdkError` -> `ApiError` conversion.
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::Decided;
        m.winner = Some(10);
        m.decided_at_ms = 1_000;
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 2_000);
        let err = force_decide(&host, 7, 0, 20).unwrap_err();
        match err {
            SdkError::Other(msg) => assert_eq!(msg, ALREADY_DECIDED_MSG),
            other => panic!("expected SdkError::Other(ALREADY_DECIDED_MSG), got {other:?}"),
        }
    }

    #[test]
    fn force_decide_cancels_the_currently_open_xiaojus_timer() {
        // A withdrawal mid-match still has a live 小局 deadline scheduled;
        // forcing the decision must not leave that stale timer armed.
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &match_in_progress());

        queue_now(&host, 5_000_000);
        force_decide(&host, 7, 0, 10).unwrap();

        assert!(host.timer.was_cancelled(&xiaoju_timer_key(7, 0, 0)));
    }

    #[test]
    fn force_decide_cancels_the_escalation_timer_when_forcing_an_awaiting_judge_match() {
        // Mirrors `force_decide_cancels_the_currently_open_xiaojus_timer`:
        // a staff override on an `AwaitingJudge` match must not leave the
        // escalation timer armed to fire later and clobber the state the
        // staff member just set.
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = match_in_progress();
        m.state = MatchPhase::AwaitingJudge;
        m.awaiting_submission_id = Some(1);
        seed(&host, 7, &setup, 0, &m);

        queue_now(&host, 5_000_000);
        force_decide(&host, 7, 0, 10).unwrap();

        assert!(host.timer.was_cancelled(&judgewait_timer_key(7, 0, 0)));
        let reloaded = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(reloaded.state, MatchPhase::Decided);
        assert_eq!(
            reloaded.awaiting_submission_id, None,
            "a decided match must not still claim to be awaiting a submission"
        );
    }

    #[test]
    fn force_decide_is_a_noop_error_for_a_match_id_that_was_never_created() {
        let host = Host::mock();
        host.storage
            .set(&[(
                storage::setup_key(7).as_str(),
                serde_json::to_string(&setup_two_rounds()).unwrap().as_str(),
            )])
            .unwrap();
        assert!(force_decide(&host, 7, 5, 10).is_err());
        assert!(storage::load_match(&host, 7, 5).unwrap().is_none());
    }

    // -- try_autostart: matches start by themselves --

    fn ranked_match(round: u8, a: i32, b: i32) -> MatchState {
        MatchState {
            round,
            player_a: a,
            player_b: b,
            group_a: [101, 102, 103],
            group_b: [201, 202, 203],
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            state: MatchPhase::Ordering,
            ..Default::default()
        }
    }

    fn queue_contest_start(host: &Host, start_ms: i64) {
        host.db
            .queue_query_result(serde_json::json!([{ "start_ms": start_ms as f64 }]));
    }

    #[test]
    fn a_ranked_match_starts_by_itself_once_the_contest_is_running() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &ranked_match(1, 10, 20));
        queue_contest_start(&host, 1_000);
        queue_now(&host, 5_000);

        try_autostart(&host, 7, 0).unwrap();

        let m = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(m.state, MatchPhase::InProgress);
        assert_eq!(m.xiaoju.len(), 1, "game 1 opened");
        assert!(host.timer.is_scheduled(&xiaoju_timer_key(7, 0, 0)));
    }

    #[test]
    fn a_match_ranked_before_the_contest_starts_is_timed_for_the_start() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        seed(&host, 7, &setup, 0, &ranked_match(1, 10, 20));
        queue_contest_start(&host, 60_000);
        queue_now(&host, 5_000);

        try_autostart(&host, 7, 0).unwrap();

        let m = storage::load_match(&host, 7, 0).unwrap().unwrap();
        assert_eq!(m.state, MatchPhase::Ordering);
        assert_eq!(
            host.timer.scheduled_at(&start_timer_key(7, 0)),
            Some(60_000)
        );
    }

    #[test]
    fn a_later_match_waits_for_the_later_players_break() {
        let host = Host::mock();
        let setup = setup_two_rounds(); // 600 s break
        let won = |pos: u8, winner: i32, at: i64| MatchState {
            round: 1,
            pos,
            winner: Some(winner),
            decided_at_ms: at,
            state: MatchPhase::Decided,
            ..Default::default()
        };
        seed(
            &host,
            7,
            &setup,
            storage::match_id_for(1, 0),
            &won(0, 10, 100_000),
        );
        seed(
            &host,
            7,
            &setup,
            storage::match_id_for(1, 1),
            &won(1, 20, 300_000),
        );
        let next = storage::match_id_for(2, 0);
        seed(&host, 7, &setup, next, &ranked_match(2, 10, 20));
        queue_contest_start(&host, 0);
        queue_now(&host, 400_000);

        try_autostart(&host, 7, next).unwrap();

        let m = storage::load_match(&host, 7, next).unwrap().unwrap();
        assert_eq!(m.state, MatchPhase::Ordering, "player 20 is still resting");
        assert_eq!(
            host.timer.scheduled_at(&start_timer_key(7, next)),
            Some(300_000 + 600_000)
        );
    }

    #[test]
    fn a_match_missing_a_ranking_waits_without_a_timer() {
        let host = Host::mock();
        let setup = setup_two_rounds();
        let mut m = ranked_match(1, 10, 20);
        m.order_b = None;
        seed(&host, 7, &setup, 0, &m);
        queue_contest_start(&host, 0);

        try_autostart(&host, 7, 0).unwrap();

        assert_eq!(
            storage::load_match(&host, 7, 0).unwrap().unwrap().state,
            MatchPhase::Ordering
        );
        assert!(!host.timer.is_scheduled(&start_timer_key(7, 0)));
    }

    #[test]
    fn start_timer_key_round_trips_and_rejects_other_keys() {
        assert_eq!(parse_start_timer_key(&start_timer_key(7, 3)), Some((7, 3)));
        assert_eq!(parse_start_timer_key("xiaoju:7:3:0"), None);
        assert_eq!(parse_start_timer_key("start:7:3:extra"), None);
    }
}
