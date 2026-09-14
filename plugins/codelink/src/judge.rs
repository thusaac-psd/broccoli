use broccoli_server_sdk::evaluator::{
    CaseOutcome, ContestJudge, DetachedEval, JudgeProgress, JudgeStep,
};
use broccoli_server_sdk::prelude::*;
use serde::{Deserialize, Serialize};

pub fn judge(host: &Host, req: &OnSubmissionInput) -> Result<(), SdkError> {
    // An empty test set must not grant a scarce qualification slot.
    if req.test_cases.is_empty() {
        return update(
            host,
            SubmissionUpdate {
                submission_id: req.submission_id,
                judgement_id: req.judgement_id,
                judge_epoch: req.judge_epoch,
                status: Some(SubmissionStatus::Judged),
                verdict: Some(Some(Verdict::SystemError)),
                score: Some(0.0),
                error_code: Some(Some("CODELINK_NO_TEST_CASES".into())),
                error_message: Some(Some("Codelink problems require test cases".into())),
                ..Default::default()
            },
        );
    }

    DetachedEval::start(
        host,
        req,
        &req.test_cases,
        CodelinkJudge,
        "on_codelink_eval_result",
        1,
    )
}

/// Result delivery, epoch checks, retries, and test-case persistence are owned
/// by the SDK driver. This policy only supplies binary judging decisions.
#[derive(Serialize, Deserialize)]
struct CodelinkJudge;

impl ContestJudge for CodelinkJudge {
    fn score(&self, result: &TestCaseVerdict) -> CaseOutcome {
        CaseOutcome::from_verdict(
            result,
            if result.verdict.is_accepted() {
                1.0
            } else {
                0.0
            },
        )
    }

    fn next_step(&mut self, progress: &JudgeProgress<'_>) -> JudgeStep {
        if progress
            .last
            .is_some_and(|outcome| !outcome.verdict.is_accepted())
        {
            JudgeStep::short_circuit()
        } else {
            JudgeStep::Continue
        }
    }

    fn finalize(&self, host: &Host, progress: &JudgeProgress<'_>) -> Result<(), SdkError> {
        persist(host, progress)
    }
}

pub fn handle_eval_callback(
    host: &Host,
    input: DetachedEvaluateCallbackInput,
) -> DetachedEvaluateCallbackOutput {
    let snapshot = input.state.clone();
    match DetachedEval::<CodelinkJudge>::handle_callback(host, input) {
        Ok(output) => output,
        Err(SdkError::StaleEpoch) => DetachedEvaluateCallbackOutput::cancel(snapshot),
        Err(error) => {
            let _ = host
                .log
                .info(&format!("Codelink evaluation callback failed: {error}"));
            DetachedEval::<CodelinkJudge>::recover(host, &snapshot);
            DetachedEvaluateCallbackOutput::cancel(snapshot)
        }
    }
}

fn persist(host: &Host, progress: &JudgeProgress<'_>) -> Result<(), SdkError> {
    let req = progress.request;
    let all_accepted = progress.all_recorded()
        && !progress.outcomes.is_empty()
        && progress.outcomes.iter().all(|o| o.verdict.is_accepted());
    let outcomes: Vec<_> = progress
        .outcomes
        .iter()
        .filter(|o| !o.verdict.is_skipped_or_cancelled())
        .collect();
    let mut verdict = outcomes
        .iter()
        .map(|o| o.verdict.clone())
        .max_by_key(|v| v.severity())
        .unwrap_or(Verdict::SystemError);
    // A cancelled or skipped case cannot grant a scarce slot, even if some
    // other cases already passed. Standings consume the verdict, not the score.
    if verdict.is_accepted() && !all_accepted {
        verdict = Verdict::SystemError;
    }
    let is_ce = verdict == Verdict::CompileError;
    let compile_output = if is_ce {
        outcomes
            .iter()
            .find(|o| o.verdict == Verdict::CompileError)
            .and_then(|o| o.message.clone())
    } else {
        None
    };
    update(
        host,
        SubmissionUpdate {
            submission_id: req.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(if is_ce {
                SubmissionStatus::CompilationError
            } else {
                SubmissionStatus::Judged
            }),
            verdict: Some(if is_ce { None } else { Some(verdict) }),
            // Submission score describes judge correctness. The Codelink board
            // computes the competitive credit, which may be zero even for AC.
            score: Some(if all_accepted { 1.0 } else { 0.0 }),
            time_used: Some(outcomes.iter().filter_map(|o| o.time_used).max()),
            memory_used: Some(outcomes.iter().filter_map(|o| o.memory_used).max()),
            compile_output: Some(compile_output),
            error_code: Some(None),
            error_message: Some(None),
        },
    )
}

fn update(host: &Host, update: SubmissionUpdate) -> Result<(), SdkError> {
    if host.submission.update(&update)? == 0 {
        return Err(SdkError::StaleEpoch);
    }
    Ok(())
}
