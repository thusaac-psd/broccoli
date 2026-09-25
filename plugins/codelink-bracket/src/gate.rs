//! `[[server.hooks]] topic = "before_submission" scope = "resource"` --
//! reject a submission unless it targets the submitter's OWN current 小局
//! problem (or the active 附加赛 problem) in a live match.
//!
//! Quoted from `docs/superpowers/specs/2026-09-19-afternoon-bracket-design.md`:
//!
//! > `[[server.hooks]] topic = "before_submission"` rejects unless **all**
//! > hold:
//! >
//! > - the target contest is this format's contest,
//! > - V is a player in a match whose state is `InProgress` or `Tiebreak`,
//! > - the problem is V's current 小局 problem (or the active 附加赛
//! >   problem),
//! > - the 小局 is open -- started, and not yet decided.
//!
//! Visibility (`visibility.rs`) already hides a problem the viewer should
//! not see, but that is advisory only: a player can still POST an arbitrary
//! problem id directly, bypassing whatever the UI shows. This gate is the
//! enforcement point, independent of visibility.

use crate::model::MatchPhase;

// See `visibility.rs`'s identical comment: these imports are used only by
// the host wiring below, which is gated `#[cfg(any(target_arch = "wasm32",
// test))]` -- an ungated import here would be flagged unused by a plain
// (non-wasm32, non-test) `cargo build`/`clippy` pass.
#[cfg(any(target_arch = "wasm32", test))]
use crate::model::{MatchState, RoundDef};
#[cfg(any(target_arch = "wasm32", test))]
use crate::storage;
#[cfg(any(target_arch = "wasm32", test))]
use broccoli_server_sdk::prelude::*;

/// Everything [`check`] needs about ONE match to decide whether `viewer` may
/// submit to `problem_id` right now. Deliberately narrower than
/// [`crate::model::MatchState`], mirroring [`crate::visibility::VisibilityCtx`]:
/// built from a real match by the host wiring below; tests construct it
/// directly.
#[derive(Debug, Clone)]
pub struct GateCtx {
    pub player_a: i32,
    pub player_b: i32,
    /// The order player A must solve their own group in, as ranked by B.
    /// `None` before B has ranked it (still `Ordering`).
    pub order_a: Option<[i32; 3]>,
    /// The order player B must solve their own group in, as ranked by A.
    pub order_b: Option<[i32; 3]>,
    pub state: MatchPhase,
    /// Index of the currently open 小局 (regular or 附加赛 -- both live in
    /// the same `xiaoju` log, see [`crate::model::MatchState::xiaoju`]).
    /// `None` before any 小局 has opened.
    pub current_xiaoju_index: Option<u8>,
    /// Whether the currently open 小局 has already been decided. Distinct
    /// from `current_xiaoju_index` being `None`: an index can be `Some` for
    /// a 小局 that has since been decided but whose successor has not opened
    /// yet (a brief window between `decide_xiaoju` deciding it and the next
    /// one being scheduled).
    pub current_xiaoju_decided: bool,
    /// The active 附加赛 problem, if `state == Tiebreak`. During 附加赛 both
    /// players face the SAME problem ("双方将面对相同的题目"), unlike the
    /// regular 小局 phase where each player has their own current problem
    /// from their own imposed order.
    pub tiebreak_problem: Option<i32>,
}

/// A rejected submission's reason, carrying a machine-readable `code` so the
/// frontend can distinguish "wrong problem" from "小局 closed" from
/// "eliminated" rather than showing one generic error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub code: String,
    pub message: String,
}

impl Rejection {
    fn new(code: &str, message: &str) -> Self {
        Rejection {
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    fn not_your_problem() -> Self {
        Self::new(
            "NOT_YOUR_PROBLEM",
            "This is not your current problem in this 小局.",
        )
    }

    fn xiaoju_closed() -> Self {
        Self::new("XIAOJU_CLOSED", "This 小局 has already been decided.")
    }

    /// Produced by the host wiring below, never by [`check`]: `problem_id`
    /// does not resolve to a round this viewer has a match in at all (either
    /// the problem is not part of this bracket, or it belongs to a round the
    /// viewer never played -- e.g. an eliminated player probing a later
    /// round's problem id directly).
    #[cfg(any(target_arch = "wasm32", test))]
    fn not_in_match() -> Self {
        Self::new(
            "NOT_IN_MATCH",
            "You are not a participant in a match for this problem.",
        )
    }

    /// Produced by the host wiring below: the viewer DOES have a match for
    /// this round, but it is not currently `InProgress` or `Tiebreak` --
    /// covers both "not started yet" (`Pending`/`Ordering`) and "no longer
    /// accepting submissions" (`Decided`/`NeedsAdjudication`). The spec's
    /// wording ("has not started") only names the former, but the latter is
    /// just as much "not open for submission right now" and the plan does
    /// not allocate it a fifth code.
    #[cfg(any(target_arch = "wasm32", test))]
    fn match_not_started() -> Self {
        Self::new(
            "MATCH_NOT_STARTED",
            "This match is not currently accepting submissions.",
        )
    }
}

/// Decide whether `viewer` may submit to `problem_id` right now. See the
/// module doc comment for the rule this implements (the last two of its four
/// bullets -- "V is a player in a match whose state is InProgress or
/// Tiebreak" is enforced by the host wiring below, before a [`GateCtx`] is
/// even built: a `GateCtx` always describes a live match).
pub fn check(ctx: &GateCtx, viewer: i32, problem_id: i32) -> Result<(), Rejection> {
    // What SHOULD `viewer` be submitting to right now? During 附加赛 both
    // players face the same shared problem; during the regular 小局 phase
    // each player's current problem is their OWN position in their OWN
    // imposed order (`order_a`/`order_b`) at the current 小局 index. Any
    // other phase (Pending, Ordering, Decided, NeedsAdjudication) has no
    // current problem at all -- the host wiring below is responsible for
    // never building a `GateCtx` in those phases, but `check` still treats
    // "no expected problem" as a hard rejection rather than trusting that.
    let expected = match ctx.state {
        MatchPhase::Tiebreak => ctx.tiebreak_problem,
        MatchPhase::InProgress => {
            let order = if viewer == ctx.player_a {
                ctx.order_a
            } else if viewer == ctx.player_b {
                ctx.order_b
            } else {
                None
            };
            match (order, ctx.current_xiaoju_index) {
                (Some(order), Some(index)) => order.get(index as usize).copied(),
                _ => None,
            }
        }
        _ => None,
    };

    if expected != Some(problem_id) {
        return Err(Rejection::not_your_problem());
    }

    if ctx.current_xiaoju_decided {
        return Err(Rejection::xiaoju_closed());
    }

    Ok(())
}

/// Build a [`GateCtx`] from a real match and the `RoundDef` that owns it.
/// Mirrors `visibility.rs`'s `ctx_from_match`.
#[cfg(any(target_arch = "wasm32", test))]
fn gate_ctx_from_match(m: &MatchState, round_def: &RoundDef) -> GateCtx {
    let current = m.xiaoju.last();
    GateCtx {
        player_a: m.player_a,
        player_b: m.player_b,
        order_a: m.order_a,
        order_b: m.order_b,
        state: m.state,
        current_xiaoju_index: current.map(|x| x.index),
        current_xiaoju_decided: current.map(|x| x.decided).unwrap_or(false),
        tiebreak_problem: round_def.tiebreak.get(m.tiebreak_index).copied(),
    }
}

/// Core decision logic for the `before_submission` hook. Exercised directly
/// by tests via `Host::mock()`; the thin `check_submission` wrapper below
/// adapts it to the WASM ABI.
///
/// Resolves `event.problem_id` to a round the same way `visibility.rs`
/// resolves a problem-visibility query: scan `Setup.rounds` for the round
/// whose group or 附加赛 list contains it, then find `event.user_id`'s one
/// match in that round. A missing setup, an unresolvable round, or no match
/// for this viewer all reject as `NOT_IN_MATCH` -- there is no "fail open"
/// case for a submission gate, unlike visibility's "no opinion" `Allow`
/// default: a gate that cannot positively confirm the four conditions must
/// not let the submission through.
#[cfg(any(target_arch = "wasm32", test))]
fn resolve_rejection(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<Option<Rejection>, SdkError> {
    let Some(contest_id) = event.contest_id else {
        return Ok(Some(Rejection::not_in_match()));
    };
    let Some(setup) = storage::load_setup(host, contest_id)? else {
        return Ok(Some(Rejection::not_in_match()));
    };
    let Some(round_index) = setup.rounds.iter().position(|r| {
        r.group_a.contains(&event.problem_id)
            || r.group_b.contains(&event.problem_id)
            || r.tiebreak.contains(&event.problem_id)
    }) else {
        // Not part of this bracket at all this round -- certainly not
        // "yours" to submit to.
        return Ok(Some(Rejection::not_your_problem()));
    };
    let round_def = &setup.rounds[round_index];
    let round = (round_index + 1) as u8;

    let matches = storage::load_all_matches(host, contest_id)?;
    let Some(m) = storage::find_players_match(&matches, round, event.user_id) else {
        return Ok(Some(Rejection::not_in_match()));
    };

    if m.state != MatchPhase::InProgress && m.state != MatchPhase::Tiebreak {
        return Ok(Some(Rejection::match_not_started()));
    }

    let ctx = gate_ctx_from_match(m, round_def);
    Ok(check(&ctx, event.user_id, event.problem_id).err())
}

#[cfg(any(target_arch = "wasm32", test))]
fn check_submission_response(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<HookResponse, SdkError> {
    match resolve_rejection(host, event)? {
        None => Ok(HookResponse::pass()),
        Some(rejection) => Ok(HookResponse::reject(
            rejection.code,
            rejection.message,
            400,
            None,
        )),
    }
}

#[cfg(target_arch = "wasm32")]
#[extism_pdk::plugin_fn]
pub fn check_submission(input: String) -> extism_pdk::FnResult<String> {
    let host = Host::new();
    let event: BeforeSubmissionEvent = serde_json::from_str(&input)?;
    let response = check_submission_response(&host, &event)?;
    Ok(serde_json::to_string(&response)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate_in_progress_at_xiaoju(index: u8, order_a: [i32; 3]) -> GateCtx {
        GateCtx {
            player_a: 10,
            player_b: 20,
            order_a: Some(order_a),
            order_b: Some([203, 201, 202]),
            state: MatchPhase::InProgress,
            current_xiaoju_index: Some(index),
            current_xiaoju_decided: false,
            tiebreak_problem: None,
        }
    }

    fn gate_with_decided_xiaoju(index: u8) -> GateCtx {
        GateCtx {
            player_a: 10,
            player_b: 20,
            order_a: Some([103, 101, 102]),
            order_b: Some([203, 201, 202]),
            state: MatchPhase::InProgress,
            current_xiaoju_index: Some(index),
            current_xiaoju_decided: true,
            tiebreak_problem: None,
        }
    }

    #[test]
    fn rejects_a_submission_to_a_problem_that_is_not_the_current_xiaoju_problem() {
        let ctx = gate_in_progress_at_xiaoju(0, [103, 101, 102]);
        assert_eq!(check(&ctx, 10, 101).unwrap_err().code, "NOT_YOUR_PROBLEM");
    }

    #[test]
    fn rejects_a_submission_to_the_opponents_problem() {
        // Visibility already denies reading it, but the gate must not rely
        // on that: a player could still POST the id directly.
        let ctx = gate_in_progress_at_xiaoju(0, [103, 101, 102]);
        assert_eq!(check(&ctx, 10, 201).unwrap_err().code, "NOT_YOUR_PROBLEM");
    }

    #[test]
    fn rejects_a_submission_after_the_xiaoju_is_decided() {
        let ctx = gate_with_decided_xiaoju(0);
        assert_eq!(check(&ctx, 10, 103).unwrap_err().code, "XIAOJU_CLOSED");
    }

    #[test]
    fn accepts_the_current_problem_from_the_right_player() {
        // Negative control for the three rejections above.
        let ctx = gate_in_progress_at_xiaoju(0, [103, 101, 102]);
        assert!(check(&ctx, 10, 103).is_ok());
    }

    // -- Host wiring: `check_submission_response`, real storage, no hand-
    // built `GateCtx`. --

    fn seeded_setup() -> crate::model::Setup {
        crate::model::Setup {
            rounds: vec![RoundDef {
                group_a: [101, 102, 103],
                group_b: [201, 202, 203],
                tiebreak: vec![319],
            }],
            xiaoju_seconds: 1_800,
            round_intermission_seconds: 600,
            escalation_grace_seconds: 120,
        }
    }

    fn seed_match(
        host: &Host,
        contest_id: i32,
        state: MatchPhase,
        xiaoju: Vec<crate::model::XiaojuState>,
    ) {
        host.storage
            .set(&[(
                storage::setup_key(contest_id).as_str(),
                serde_json::to_string(&seeded_setup()).unwrap().as_str(),
            )])
            .unwrap();
        host.storage
            .set(&[(
                storage::match_key(contest_id, 0).as_str(),
                serde_json::to_string(&MatchState {
                    round: 1,
                    pos: 0,
                    player_a: 10,
                    player_b: 20,
                    group_a: [101, 102, 103],
                    group_b: [201, 202, 203],
                    order_a: Some([103, 101, 102]),
                    order_b: Some([203, 201, 202]),
                    xiaoju,
                    state,
                    ..Default::default()
                })
                .unwrap()
                .as_str(),
            )])
            .unwrap();
    }

    fn open_xiaoju(index: u8, decided: bool) -> Vec<crate::model::XiaojuState> {
        vec![crate::model::XiaojuState {
            index,
            opened_at_ms: 0,
            deadline_ms: 9_999,
            winner: None,
            decided,
        }]
    }

    fn event(user_id: i32, problem_id: i32, contest_id: Option<i32>) -> BeforeSubmissionEvent {
        BeforeSubmissionEvent {
            user_id,
            problem_id,
            contest_id,
            language: "cpp".to_string(),
            file_count: 1,
        }
    }

    fn rejection_code(resp: HookResponse) -> String {
        match resp {
            HookResponse::Reject { code, .. } => code,
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn check_submission_response_accepts_the_right_players_current_problem() {
        let host = Host::mock();
        seed_match(&host, 7, MatchPhase::InProgress, open_xiaoju(0, false));
        let resp = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        assert!(matches!(resp, HookResponse::Pass));
    }

    #[test]
    fn check_submission_response_rejects_a_player_with_no_match_this_round_as_not_in_match() {
        let host = Host::mock();
        seed_match(&host, 7, MatchPhase::InProgress, open_xiaoju(0, false));
        // 999 is not player_a (10) or player_b (20) of the seeded match.
        let resp = check_submission_response(&host, &event(999, 103, Some(7))).unwrap();
        assert_eq!(rejection_code(resp), "NOT_IN_MATCH");
    }

    #[test]
    fn check_submission_response_rejects_before_the_match_has_started() {
        let host = Host::mock();
        seed_match(&host, 7, MatchPhase::Ordering, vec![]);
        let resp = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        assert_eq!(rejection_code(resp), "MATCH_NOT_STARTED");
    }

    #[test]
    fn check_submission_response_rejects_a_problem_not_in_the_bracket_at_all() {
        let host = Host::mock();
        seed_match(&host, 7, MatchPhase::InProgress, open_xiaoju(0, false));
        let resp = check_submission_response(&host, &event(10, 999, Some(7))).unwrap();
        assert_eq!(rejection_code(resp), "NOT_YOUR_PROBLEM");
    }

    #[test]
    fn check_submission_response_rejects_a_contest_less_event_as_not_in_match() {
        // Defensive: this hook is only ever dispatched for a contest where
        // the plugin is enabled, so `contest_id: None` should not occur in
        // practice -- but a gate that cannot positively confirm the four
        // conditions must not let the submission through.
        let host = Host::mock();
        let resp = check_submission_response(&host, &event(10, 103, None)).unwrap();
        assert_eq!(rejection_code(resp), "NOT_IN_MATCH");
    }
}
