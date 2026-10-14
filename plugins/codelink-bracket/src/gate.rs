//! `[[server.hooks]] topic = "before_submission" scope = "global"` --
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
//!
//! The global hook passes unrelated submissions and protects bracket problems
//! from submissions through practice or a different contest format. All of
//! those restrictions are owned here, rather than inferred by the host.

use crate::model::MatchPhase;

use crate::model::{MatchState, RoundDef};
use crate::storage;
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
        Self::new("XIAOJU_CLOSED", "This game is closed for submissions.")
    }

    /// Produced by the host wiring below, never by [`check`]: `problem_id`
    /// does not resolve to a round this viewer has a match in at all (either
    /// the problem is not part of this bracket, or it belongs to a round the
    /// viewer never played -- e.g. an eliminated player probing a later
    /// round's problem id directly).
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
fn resolve_rejection(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<Result<SubmissionAdmission, Rejection>, SdkError> {
    let Some(contest_id) = event.contest_id else {
        return Ok(Err(Rejection::not_in_match()));
    };
    let Some(setup) = storage::load_setup(host, contest_id)? else {
        return Ok(Err(Rejection::not_in_match()));
    };
    let Some(round_index) = setup.rounds.iter().position(|r| {
        r.group_a.contains(&event.problem_id)
            || r.group_b.contains(&event.problem_id)
            || r.tiebreak.contains(&event.problem_id)
    }) else {
        // Not part of this bracket at all this round -- certainly not
        // "yours" to submit to.
        return Ok(Err(Rejection::not_your_problem()));
    };
    let round_def = &setup.rounds[round_index];
    let round = (round_index + 1) as u8;

    let matches = storage::load_all_matches(host, contest_id)?;
    let Some(m) = storage::find_players_match(&matches, round, event.user_id) else {
        return Ok(Err(Rejection::not_in_match()));
    };

    let state_key = storage::match_key(contest_id, storage::match_id_for(m.round, m.pos));
    let expected_state = host
        .storage
        .get_one(&state_key)?
        .ok_or_else(|| SdkError::Other("match disappeared during submission".into()))?;
    let m: MatchState = serde_json::from_str(&expected_state)?;
    if m.state != MatchPhase::InProgress && m.state != MatchPhase::Tiebreak {
        return Ok(Err(Rejection::match_not_started()));
    }

    let ctx = gate_ctx_from_match(&m, round_def);
    if let Err(rejection) = check(&ctx, event.user_id, event.problem_id) {
        return Ok(Err(rejection));
    }
    let feedback = crate::feedback::player_feedback(
        host,
        contest_id,
        &m,
        event.user_id,
        event.problem_id,
        setup.penalty_seconds,
    )?;
    if feedback.pending_submission_id.is_some() {
        return Ok(Err(Rejection::new(
            "SUBMISSION_IN_FLIGHT",
            "Wait for your previous submission's result before submitting again.",
        )));
    }
    let game = m
        .xiaoju
        .last()
        .ok_or_else(|| SdkError::Other("no open game".into()))?;
    let delay_seconds = feedback.delay_seconds;
    let clock = crate::judge::clock(host, contest_id)?;
    if !game.contains_submission_at_us(clock.now.saturating_mul(1_000)) {
        return Ok(Err(Rejection::xiaoju_closed()));
    }
    let deadline_ms = clock.cutoff(game.deadline_ms);
    if clock.now >= deadline_ms {
        return Ok(Err(Rejection::new(
            "SUBMISSION_TOO_LATE",
            "The submission window for this game has closed.",
        )));
    }
    Ok(Ok(SubmissionAdmission {
        guard: SubmissionStorageGuard {
            key: state_key.clone(),
            expected_value: expected_state,
        },
        feedback_key: state_key,
        metadata: serde_json::to_value(crate::feedback::AdmissionMetadata {
            scope_key: crate::feedback::game_scope(
                contest_id,
                storage::match_id_for(m.round, m.pos),
                game.index,
            ),
            delay_seconds,
            effective_at_us: None,
        })?,
        slot: Some(SubmissionSlot {
            key: crate::feedback::player_slot(contest_id, event.user_id),
            generation: feedback.slot_generation,
        }),
        latest_arrival_ms: Some(deadline_ms),
    }))
}

fn check_submission_response(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<HookResponse, SdkError> {
    match resolve_rejection(host, event)? {
        Ok(admission) => {
            let mut payload = serde_json::to_value(event)?;
            payload["submission_admission"] = serde_json::to_value(admission)?;
            Ok(HookResponse::modified(payload))
        }
        Err(rejection) => Ok(HookResponse::reject(
            rejection.code,
            rejection.message,
            400,
            None,
        )),
    }
}

pub fn dispatch_submission_gate(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<HookResponse, SdkError> {
    if event.contest_id.is_some() && event.contest_type == "codelink-bracket" {
        return check_submission_response(host, event);
    }
    let blocked = if event.contest_type == "codelink-bracket" {
        true
    } else {
        let mut p = Params::new();
        let sql = format!("SELECT c.id FROM contest c JOIN contest_problem cp ON cp.contest_id = c.id
            WHERE cp.problem_id = {} AND c.contest_type = 'codelink-bracket'
            AND c.deleted_at IS NULL AND (c.end_time IS NULL OR c.end_time > clock_timestamp()) LIMIT 1", p.bind(event.problem_id));
        host.db
            .query_one_with_args::<serde_json::Value>(&sql, &p.into_args())?
            .is_some()
    };
    Ok(if blocked {
        HookResponse::reject(
            "CONTEST_SUBMISSION_REQUIRED",
            "Submit this problem through its bracket contest.",
            400,
            None,
        )
    } else {
        HookResponse::pass()
    })
}

#[cfg(target_arch = "wasm32")]
#[extism_pdk::plugin_fn]
pub fn check_submission(input: String) -> extism_pdk::FnResult<String> {
    let host = Host::new();
    let event: BeforeSubmissionEvent = serde_json::from_str(&input)?;
    let response = dispatch_submission_gate(&host, &event)?;
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
    fn global_gate_passes_unrelated_formats_and_blocks_alternate_submission_contexts() {
        let host = Host::mock();
        let mut request = event(10, 101, Some(9));
        request.contest_type = "other-format".into();
        assert!(matches!(
            dispatch_submission_gate(&host, &request).unwrap(),
            HookResponse::Pass
        ));
        for contest_id in [None, Some(9)] {
            request.contest_id = contest_id;
            host.db.queue_query_result(serde_json::json!([{"id":7}]));
            assert!(
                matches!(dispatch_submission_gate(&host,&request).unwrap(),HookResponse::Reject {code,..} if code == "CONTEST_SUBMISSION_REQUIRED")
            );
        }
        request.contest_type = "codelink-bracket".into();
        request.contest_id = None;
        assert!(
            matches!(dispatch_submission_gate(&host,&request).unwrap(),HookResponse::Reject {code,..} if code == "CONTEST_SUBMISSION_REQUIRED")
        );
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
                xiaoju_seconds: 1_800,
                tiebreak_seconds: 1_800,
                break_before_seconds: 0,
            }],
            escalation_grace_seconds: 120,
            penalty_seconds: crate::model::default_penalty_seconds(),
            ..Default::default()
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
            ..Default::default()
        }]
    }

    fn event(user_id: i32, problem_id: i32, contest_id: Option<i32>) -> BeforeSubmissionEvent {
        BeforeSubmissionEvent {
            user_id,
            problem_id,
            contest_id,
            language: "cpp".to_string(),
            contest_type: "codelink-bracket".into(),
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
        host.db.queue_query_result(serde_json::json!([]));
        host.db
            .queue_query_result(serde_json::json!([{"now_ms": 0.0, "end_ms": null}]));
        let resp = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        assert!(matches!(resp, HookResponse::Modified { .. }));
    }

    #[test]
    fn submission_gate_uses_the_final_deadline_even_before_the_timer_fires() {
        for (now, allowed) in [
            (999, false),
            (1_000, true),
            (5_000, true),
            (9_998, true),
            (9_999, false),
            (10_000, false),
        ] {
            let host = Host::mock();
            let mut games = open_xiaoju(0, false);
            games[0].opened_at_ms = 1_000;
            games[0].base_deadline_ms = Some(5_000);
            seed_match(&host, 7, MatchPhase::InProgress, games);
            host.db.queue_query_result(serde_json::json!([]));
            host.db
                .queue_query_result(serde_json::json!([{ "now_ms": now }]));
            let response = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
            if allowed {
                assert!(
                    matches!(response, HookResponse::Modified { .. }),
                    "at {now}"
                );
            } else {
                assert_eq!(rejection_code(response), "XIAOJU_CLOSED", "at {now}");
            }
        }
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

    #[test]
    fn unfinished_feedback_blocks_the_next_submission() {
        let host = Host::mock();
        seed_match(&host, 7, MatchPhase::InProgress, open_xiaoju(0, false));
        host.db.queue_query_result(serde_json::json!([{
            "penalty_count": 0, "pending_submission_id": 42, "effective_at_us": 1_000_000
        }]));
        let response = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        assert_eq!(rejection_code(response), "SUBMISSION_IN_FLIGHT");
    }

    #[test]
    fn admission_uses_the_configured_penalty_including_zero() {
        for seconds in [7, 0] {
            let host = Host::mock();
            seed_match(&host, 7, MatchPhase::InProgress, open_xiaoju(0, false));
            let mut setup = seeded_setup();
            setup.penalty_seconds = seconds;
            host.storage
                .set(&[(
                    storage::setup_key(7).as_str(),
                    serde_json::to_string(&setup).unwrap().as_str(),
                )])
                .unwrap();
            host.db.queue_query_result(serde_json::json!([{
                "penalty_count": 1, "pending_submission_id": null, "effective_at_us": null
            }]));
            host.db
                .queue_query_result(serde_json::json!([{"now_ms": 0.0, "end_ms": null}]));
            let admission = resolve_rejection(&host, &event(10, 103, Some(7)))
                .unwrap()
                .unwrap();
            assert_eq!(admission.metadata["delay_seconds"], seconds);
        }
    }

    #[test]
    fn a_score_penalty_does_not_shorten_the_submission_window() {
        let host = Host::mock();
        seed_match(&host, 7, MatchPhase::InProgress, open_xiaoju(0, false));
        host.db.queue_query_result(serde_json::json!([{
            "penalty_count": 1, "pending_submission_id": null, "effective_at_us": null
        }]));
        host.db
            .queue_query_result(serde_json::json!([{"now_ms": 0.0, "end_ms": null}]));
        let response = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        let HookResponse::Modified { event: payload } = response else {
            panic!("a timely submission must be admitted despite its score penalty");
        };
        assert_eq!(
            payload["submission_admission"]["metadata"]["delay_seconds"],
            20
        );
        assert_eq!(payload["submission_admission"]["latest_arrival_ms"], 9_999);
    }
}
