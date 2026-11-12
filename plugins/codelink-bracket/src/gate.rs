//! `[[server.hooks]] topic = "before_submission" scope = "global"` --
//! reject a submission unless it targets the submitter's OWN current 小局
//! problem (or the active 附加赛 problem) in a live match.
//!
//! A submission through a bracket contest is admitted only when all hold:
//!
//! - the submitter plays in a match of the problem's round whose game is
//!   open (`Phase::Playing`),
//! - the problem is the submitter's current 小局 problem (or the active
//!   附加赛 problem), as [`MatchState::current_problem`] defines it,
//! - the 小局 is open -- started, not yet decided, and inside its window.
//!
//! Visibility (`visibility.rs`) already hides a problem the viewer should
//! not see, but that is advisory only: a player can still POST an arbitrary
//! problem id directly, bypassing whatever the UI shows. This gate is the
//! enforcement point, independent of visibility.
//!
//! The hook is global, so it also sees submissions outside bracket contests,
//! and [`dispatch_submission_gate`] sorts them by where they come from:
//!
//! - through a bracket contest: the rules above;
//! - through any other contest (a qualifier sharing the problem, say): pass.
//!   That contest's own rules and the host decide, not this plugin;
//! - outside every contest: rejected while a bracket that has not ended
//!   holds the problem, so a player cannot dodge the rules above through
//!   practice.
//!
//! All of those restrictions are owned here, rather than inferred by the host.

use crate::model::{MatchState, Phase, RoundDef};
use crate::storage;
use broccoli_server_sdk::prelude::*;

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
    /// this round, but no game of it is open -- covers both "not started
    /// yet" (`Ordering`/`Preparing`), a break or a judging wait, and "no
    /// longer accepting submissions" (`Decided`/`NeedsAdjudication`). The spec's
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

/// Whether `viewer` may submit `problem_id` to the open game of `m`.
pub fn check(
    m: &MatchState,
    round_def: &RoundDef,
    viewer: i32,
    problem_id: i32,
) -> Result<(), Rejection> {
    if m.current_problem(round_def, viewer) != Some(problem_id) {
        return Err(Rejection::not_your_problem());
    }
    if m.xiaoju.last().is_some_and(|game| game.decided) {
        return Err(Rejection::xiaoju_closed());
    }
    Ok(())
}

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
    let Some(round) = setup.problem_round(event.problem_id) else {
        // Not part of this bracket at all this round -- certainly not
        // "yours" to submit to.
        return Ok(Err(Rejection::not_your_problem()));
    };

    let matches = storage::load_all_matches(host, contest_id)?;
    let Some(m) = storage::find_players_match(&matches, round, event.user_id) else {
        return Ok(Err(Rejection::not_in_match()));
    };
    let round_def = setup
        .round_for_match(m.round, m.pos)
        .ok_or_else(|| SdkError::Other("Missing match problem groups".into()))?;

    let state_key = storage::match_key(
        contest_id,
        storage::match_id_for(setup.player_count(), m.round, m.pos),
    );
    let snapshot = host
        .storage
        .snapshot(&state_key)?
        .ok_or_else(|| SdkError::Other("match disappeared during submission".into()))?;
    let m: MatchState = serde_json::from_str(&snapshot.value)?;
    if m.phase != Phase::Playing {
        return Ok(Err(Rejection::match_not_started()));
    }

    if let Err(rejection) = check(&m, &round_def, event.user_id, event.problem_id) {
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
    let clock = crate::clock::clock(host, contest_id)?;
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
        guard: SubmissionStorageGuard::from_snapshot(state_key.clone(), snapshot),
        feedback_key: state_key,
        metadata: serde_json::to_value(crate::feedback::AdmissionMetadata {
            scope_key: crate::feedback::game_scope(
                contest_id,
                storage::match_id_for(setup.player_count(), m.round, m.pos),
                game.index,
            ),
            delay_seconds,
            effective_at_us: None,
        })?,
        slot: feedback.slot_token,
        latest_arrival_ms: Some(deadline_ms),
    }))
}

fn check_submission_response(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<HookResponse, SdkError> {
    match resolve_rejection(host, event)? {
        Ok(admission) => Ok(HookResponse::admit(event, admission)?),
        Err(rejection) => Ok(HookResponse::reject(
            rejection.code,
            rejection.message,
            400,
            None,
        )),
    }
}

/// The global `before_submission` entry point. See the module doc comment
/// for how a submission is sorted by where it comes from.
pub fn dispatch_submission_gate(
    host: &Host,
    event: &BeforeSubmissionEvent,
) -> Result<HookResponse, SdkError> {
    let blocked = match event.contest_id {
        Some(_) if event.contest_type == "codelink-bracket" => {
            return check_submission_response(host, event);
        }
        // Another contest's submission is that contest's business, even when
        // a bracket also holds the problem.
        Some(_) => false,
        None => {
            event.contest_type == "codelink-bracket"
                || storage::unended_bracket_contests(host, &[event.problem_id])?
                    .contains_key(&event.problem_id)
        }
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

    fn round_def() -> RoundDef {
        RoundDef {
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            tiebreak: vec![319],
            ..Default::default()
        }
    }

    fn match_at_xiaoju(index: u8, decided: bool) -> MatchState {
        MatchState {
            player_a: 10,
            player_b: 20,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            phase: Phase::Playing,
            xiaoju: vec![crate::model::XiaojuState {
                index,
                decided,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn is_contest_submission_required(response: HookResponse) -> bool {
        matches!(response, HookResponse::Reject { code, .. } if code == "CONTEST_SUBMISSION_REQUIRED")
    }

    #[test]
    fn global_gate_passes_another_contest_without_a_lookup() {
        // A qualifier sharing a bracket problem judges its own submissions,
        // whether or not a bracket still holds the problem.
        let host = Host::mock();
        let mut request = event(10, 101, Some(9));
        request.contest_type = "other-format".into();
        assert!(matches!(
            dispatch_submission_gate(&host, &request).unwrap(),
            HookResponse::Pass
        ));
        assert!(
            host.db.queries().is_empty(),
            "another contest's submission must not look up brackets: {:?}",
            host.db.queries()
        );
    }

    #[test]
    fn global_gate_blocks_a_standalone_submission_only_while_a_bracket_holds_the_problem() {
        let host = Host::mock();
        let mut request = event(10, 101, None);
        request.contest_type = "other-format".into();

        host.db
            .queue_query_result(serde_json::json!([{ "problem_id": 101, "contest_id": 7 }]));
        assert!(is_contest_submission_required(
            dispatch_submission_gate(&host, &request).unwrap()
        ));
        let queries = host.db.queries();
        assert_eq!(queries.len(), 1);
        assert!(
            queries[0].sql.contains("c.end_time > clock_timestamp()"),
            "an ended bracket must not hold the problem: {}",
            queries[0].sql
        );

        host.db.queue_query_result(serde_json::json!([]));
        assert!(matches!(
            dispatch_submission_gate(&host, &request).unwrap(),
            HookResponse::Pass
        ));
    }

    #[test]
    fn global_gate_always_blocks_the_bracket_type_outside_a_contest() {
        let host = Host::mock();
        let mut request = event(10, 101, None);
        request.contest_type = "codelink-bracket".into();
        assert!(is_contest_submission_required(
            dispatch_submission_gate(&host, &request).unwrap()
        ));
        assert!(host.db.queries().is_empty());
    }

    #[test]
    fn rejects_a_submission_to_a_problem_that_is_not_the_current_xiaoju_problem() {
        let m = match_at_xiaoju(0, false);
        assert_eq!(
            check(&m, &round_def(), 10, 101).unwrap_err().code,
            "NOT_YOUR_PROBLEM"
        );
    }

    #[test]
    fn rejects_a_submission_to_the_opponents_problem() {
        // Visibility already denies reading it, but the gate must not rely
        // on that: a player could still POST the id directly.
        let m = match_at_xiaoju(0, false);
        assert_eq!(
            check(&m, &round_def(), 10, 201).unwrap_err().code,
            "NOT_YOUR_PROBLEM"
        );
    }

    #[test]
    fn rejects_a_submission_after_the_xiaoju_is_decided() {
        let m = match_at_xiaoju(0, true);
        assert_eq!(
            check(&m, &round_def(), 10, 103).unwrap_err().code,
            "XIAOJU_CLOSED"
        );
    }

    #[test]
    fn accepts_the_current_problem_from_the_right_player() {
        // Negative control for the three rejections above.
        let m = match_at_xiaoju(0, false);
        assert!(check(&m, &round_def(), 10, 103).is_ok());
    }

    #[test]
    fn both_players_may_submit_only_the_shared_problem_in_a_tiebreak() {
        let m = match_at_xiaoju(3, false);
        for player in [10, 20] {
            assert!(check(&m, &round_def(), player, 319).is_ok());
            assert!(check(&m, &round_def(), player, 103).is_err());
        }
    }

    // -- Host wiring: `check_submission_response` with real storage. --

    fn seeded_setup() -> crate::model::Setup {
        crate::model::Setup {
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
        phase: Phase,
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
                    group_a: vec![101, 102, 103],
                    group_b: vec![201, 202, 203],
                    order_a: Some(vec![103, 101, 102]),
                    order_b: Some(vec![203, 201, 202]),
                    xiaoju,
                    phase,
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
        seed_match(&host, 7, Phase::Playing, open_xiaoju(0, false));
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
            games[0].base_deadline_ms = 5_000;
            seed_match(&host, 7, Phase::Playing, games);
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
        seed_match(&host, 7, Phase::Playing, open_xiaoju(0, false));
        // 999 is not player_a (10) or player_b (20) of the seeded match.
        let resp = check_submission_response(&host, &event(999, 103, Some(7))).unwrap();
        assert_eq!(rejection_code(resp), "NOT_IN_MATCH");
    }

    #[test]
    fn check_submission_response_rejects_before_the_match_has_started() {
        let host = Host::mock();
        seed_match(&host, 7, Phase::Ordering, vec![]);
        let resp = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        assert_eq!(rejection_code(resp), "MATCH_NOT_STARTED");
    }

    #[test]
    fn check_submission_response_rejects_a_problem_not_in_the_bracket_at_all() {
        let host = Host::mock();
        seed_match(&host, 7, Phase::Playing, open_xiaoju(0, false));
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
        seed_match(&host, 7, Phase::Playing, open_xiaoju(0, false));
        host.db.queue_query_result(serde_json::json!([{
            "penalty_count": 0, "pending_submission_id": 42, "effective_at_us": 1_000_000
        }]));
        let response = check_submission_response(&host, &event(10, 103, Some(7))).unwrap();
        assert_eq!(rejection_code(response), "SUBMISSION_IN_FLIGHT");
    }

    #[test]
    fn admission_penalties_apply_only_to_regular_games() {
        for (state, index, problem, seconds, expected) in [
            (Phase::Playing, 0, 103, 7, 7),
            (Phase::Playing, 0, 103, 0, 0),
            (Phase::Playing, 3, 319, 20, 0),
            (Phase::Playing, 3, 319, 7, 0),
        ] {
            let host = Host::mock();
            seed_match(&host, 7, state, open_xiaoju(index, false));
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
            let admission = resolve_rejection(&host, &event(10, problem, Some(7)))
                .unwrap()
                .unwrap();
            assert_eq!(admission.metadata["delay_seconds"], expected);
        }
    }

    #[test]
    fn a_score_penalty_does_not_shorten_the_submission_window() {
        let host = Host::mock();
        seed_match(&host, 7, Phase::Playing, open_xiaoju(0, false));
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
