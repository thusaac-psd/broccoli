//! Judging bracket submissions: binary AC/non-AC scoring through the shared
//! detached-evaluate driver, after which the submission's match is advanced.

use broccoli_server_sdk::Host;
use broccoli_server_sdk::error::SdkError;
use broccoli_server_sdk::evaluator::{
    CaseOutcome, ContestJudge, DetachedEval, JudgeProgress, JudgeStep,
};
use broccoli_server_sdk::types::*;
#[cfg(target_arch = "wasm32")]
use extism_pdk::{FnResult, plugin_fn};
use serde::{Deserialize, Serialize};

use crate::executor;
use crate::storage;

/// Resolve `(contest, user, problem)` -- as produced by a judged submission
/// -- to the match it belongs to, then tick it. A no-op if the
/// problem does not resolve to a round, or the user has no match in that
/// round (mirrors `gate.rs::resolve_rejection`'s same resolution, which is
/// deliberately not shared: that one REJECTS on failure to resolve, this
/// one simply has nothing to advance).
pub(crate) fn resolve_and_advance(
    host: &Host,
    contest_id: i32,
    user_id: i32,
    problem_id: i32,
) -> Result<(), SdkError> {
    let Some(setup) = storage::load_setup(host, contest_id)? else {
        return Ok(());
    };
    let Some(round) = setup.problem_round(problem_id) else {
        return Ok(());
    };

    let matches = storage::load_all_matches(host, contest_id)?;
    let Some(m) = storage::find_players_match(&matches, round, user_id) else {
        return Ok(());
    };
    executor::tick(
        host,
        contest_id,
        storage::match_id_for(setup.player_count(), m.round, m.pos),
    )
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

        let req = progress.submission;
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
        return Ok(OnSubmissionOutput::ok());
    }

    DetachedEval::start(
        host,
        req,
        &req.test_cases,
        CodelinkBracketJudge,
        "on_codelink_bracket_eval_result",
        1,
    )?;
    Ok(OnSubmissionOutput::ok())
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
        Err(SdkError::StaleEpoch) => OnSubmissionOutput::ok(),
        Err(e) => OnSubmissionOutput::failed(format!("{e:?}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MatchState, Phase, XiaojuState};
    use crate::test_support::{queue_now, seed, setup_two_rounds};

    fn match_in_progress() -> MatchState {
        MatchState {
            round: 1,
            player_a: 10,
            player_b: 20,
            group_a: vec![101, 102, 103],
            group_b: vec![201, 202, 203],
            order_a: Some(vec![103, 101, 102]),
            order_b: Some(vec![203, 201, 202]),
            phase: Phase::Playing,
            xiaoju: vec![XiaojuState {
                deadline_ms: 1_000_000,
                base_deadline_ms: 1_000_000,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_judged_submission_outside_the_bracket_advances_nothing() {
        let host = Host::mock();
        seed(&host, 7, &setup_two_rounds(), 0, &match_in_progress());
        resolve_and_advance(&host, 7, 10, 999).unwrap();
        assert!(host.db.queries().is_empty());
    }

    #[test]
    fn a_judged_submission_ticks_the_players_match() {
        let host = Host::mock();
        seed(&host, 7, &setup_two_rounds(), 0, &match_in_progress());
        queue_now(&host, 500_000);
        host.db.queue_query_result(serde_json::json!([{
            "id": 1, "user_id": 10, "problem_id": 103, "submitted_at_us": 100_000,
            "verdict": "Accepted", "status": "Judged",
        }]));
        resolve_and_advance(&host, 7, 10, 103).unwrap();
        assert_eq!(
            storage::load_match(&host, 7, 0).unwrap().unwrap().score_a,
            1
        );
    }
}
