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
//! ([`AfternoonBracketJudge::finalize`]) and a 小局 deadline firing
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

use crate::decide::{self, MatchOutcome, SubmissionRecord, XiaojuOutcome};
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

/// The problem `player` should currently be submitting to in match `m`,
/// mirroring `gate.rs::check`'s `expected` derivation exactly (see that
/// module's doc comment for why the two must never drift apart: this
/// decides who WINS a 小局, `gate.rs` decides what may be SUBMITTED to it,
/// and the two must agree on the current problem or a valid submission
/// could be judged against the wrong 小局).
fn current_problem(m: &MatchState, round_def: &RoundDef, player: i32) -> Option<i32> {
    match m.state {
        MatchPhase::Tiebreak => round_def.tiebreak.get(m.tiebreak_index).copied(),
        MatchPhase::InProgress => {
            let order = if player == m.player_a {
                m.order_a
            } else if player == m.player_b {
                m.order_b
            } else {
                None
            };
            let index = m.xiaoju.last()?.index;
            order.and_then(|o| o.get(index as usize).copied())
        }
        _ => None,
    }
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
    user_id: i32,
    problem_id: i32,
    submitted_at_ms: f64,
    verdict: Option<Verdict>,
}

/// Fetch every submission relevant to deciding `m`'s currently open 小局:
/// both players' submissions to their OWN current problem (see
/// [`current_problem`]), scoped to this contest. A player with no current
/// problem (e.g. between phases) contributes no clause and yields no rows
/// for that side.
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
        "SELECT user_id, problem_id, \
         EXTRACT(EPOCH FROM created_at) * 1000 AS submitted_at_ms, verdict \
         FROM submission WHERE contest_id = {} AND ({})",
        p.bind(contest),
        clauses.join(" OR ")
    );
    let rows: Vec<SubRow> = host.db.query_with_args(&sql, &p.into_args())?;
    Ok(rows
        .into_iter()
        .map(|r| SubmissionRecord {
            user_id: r.user_id,
            problem_id: r.problem_id,
            submitted_at_ms: r.submitted_at_ms as i64,
            verdict: r.verdict,
        })
        .collect())
}

/// Open the next 小局 (regular or 附加赛 -- both live in `m.xiaoju`, see
/// that field's doc comment) and schedule its deadline timer. `pub(crate)`
/// because Task 10's `routes::handle_start` also calls this directly, to
/// open the FIRST regular 小局 the moment a match leaves `Ordering`.
pub(crate) fn open_xiaoju(
    host: &Host,
    contest: i32,
    match_id: u8,
    setup: &Setup,
    now: i64,
    m: &mut MatchState,
) -> Result<(), SdkError> {
    let index = m.xiaoju.len() as u8;
    let deadline_ms = now + setup.xiaoju_seconds * 1_000;
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
    if m.state != MatchPhase::InProgress && m.state != MatchPhase::Tiebreak {
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
    let XiaojuOutcome::Decided { .. } = outcome else {
        return Ok(());
    };

    // The 小局 that just concluded (or was already concluded by an earlier,
    // successful `step` -- see the module doc comment) must have its
    // deadline cancelled BEFORE anything opens next, so a stale
    // redelivery of THIS timer cannot land on the next 小局.
    host.timer
        .cancel(&xiaoju_timer_key(contest, match_id, current_index))?;

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
        return Err("this match has already been decided".to_string());
    }
    m.state = MatchPhase::Decided;
    m.winner = Some(winner);
    m.decided_at_ms = now;
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
            host.timer
                .cancel(&xiaoju_timer_key(contest, match_id, current.index))?;
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
pub struct AfternoonBracketJudge;

impl ContestJudge for AfternoonBracketJudge {
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
            .info("afternoon-bracket: No test cases found; marking as SystemError (not a solve)");
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
        AfternoonBracketJudge,
        "on_afternoon_bracket_eval_result",
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
    DetachedEval::<AfternoonBracketJudge>::handle_callback(host, input)
}

/// Best-effort recovery when a detached callback fails mid-stream. Mirrors
/// `plugins/icpc/src/evaluate.rs::recover_detached_callback_error`.
pub fn recover_eval_callback_error(host: &Host, state_value: &serde_json::Value) {
    DetachedEval::<AfternoonBracketJudge>::recover(host, state_value);
}

/// `handle_afternoon_bracket_submission` / `handle_afternoon_bracket_code_run`
/// plugin_fn entry points, registered by name via `host.registry.register_contest_type`
/// in `lib.rs::init`. Live here (not `lib.rs`) following the same convention
/// as `gate.rs::check_submission` -- a `#[plugin_fn]` needs no re-export,
/// only the enclosing module declared `pub mod judge;` so it compiles into
/// the crate. Mirrors `plugins/icpc/src/lib.rs::handle_icpc_submission`
/// exactly, minus the standalone/practice-mode branch: every afternoon-bracket
/// submission belongs to a contest, and [`AfternoonBracketJudge::finalize`]'s
/// `resolve_and_advance` already no-ops gracefully on a missing `contest_id`.
#[cfg(target_arch = "wasm32")]
#[plugin_fn]
pub fn handle_afternoon_bracket_submission(input: String) -> FnResult<String> {
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
pub fn handle_afternoon_bracket_code_run(input: String) -> FnResult<String> {
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
pub fn on_afternoon_bracket_eval_result(input: String) -> FnResult<String> {
    let host = Host::new();
    let input: DetachedEvaluateCallbackInput = serde_json::from_str(&input)?;
    let state_snapshot = input.state.clone();
    let output = match handle_eval_result_callback(&host, input) {
        Ok(out) => out,
        Err(SdkError::StaleEpoch) => {
            let _ = host
                .log
                .info("afternoon-bracket: detached callback epoch stale, cancelling");
            DetachedEvaluateCallbackOutput::cancel(state_snapshot)
        }
        Err(e) => {
            let _ = host.log.info(&format!(
                "afternoon-bracket: detached callback failed ({e:?}); finalizing as SystemError"
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
    match parse_xiaoju_timer_key(&input.key) {
        Some((contest, match_id)) => {
            if let Err(e) = advance(&host, contest, match_id) {
                let _ = host.log.info(&format!(
                    "afternoon-bracket: on_timer advance failed for key {}: {e:?}",
                    input.key
                ));
            }
        }
        None => {
            let _ = host.log.info(&format!(
                "afternoon-bracket: on_timer got an unparseable key: {}",
                input.key
            ));
        }
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
            "user_id": user_id,
            "problem_id": 0,
            "submitted_at_ms": submitted_at_ms as f64,
            "verdict": "Accepted",
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
}
