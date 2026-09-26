use broccoli_server_sdk::prelude::*;

use crate::evaluate::EvalResult;

/// Persist the terminal submission update.
pub fn persist_and_track(
    host: &Host,
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
    eval: &EvalResult,
) -> Result<OnSubmissionOutput, SdkError> {
    let non_skipped: Vec<_> = eval
        .outcomes
        .iter()
        .filter(|o| !o.verdict.is_skipped_or_cancelled())
        .collect();

    let verdict = if non_skipped.is_empty() {
        // No test case produced a real outcome: every case was skipped or
        // cancelled (e.g. a host-side cancellation, or a worker restart before
        // any case was judged). The submission was never actually evaluated, so
        // it must NOT be finalized as Accepted/solved. Emit SystemError so the
        // dispatcher retries or surfaces it, never a silent AC on the scoreboard.
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
    let status = if is_ce {
        SubmissionStatus::CompilationError
    } else {
        SubmissionStatus::Judged
    };
    let db_verdict = if is_ce { None } else { Some(verdict.clone()) };

    let compile_output = if is_ce {
        eval.outcomes
            .iter()
            .find(|o| o.verdict == Verdict::CompileError)
            .and_then(|o| o.message.clone())
    } else {
        None
    };

    // Guard against an upstream `is_accepted` that is true despite no real
    // outcome: a submission with only skipped/cancelled cases scores 0.
    let score = if eval.is_accepted && !non_skipped.is_empty() {
        1.0
    } else {
        0.0
    };

    let affected = host.submission.update(&SubmissionUpdate {
        submission_id,
        judgement_id,
        judge_epoch,
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

    let _ = host.log.info(&format!(
        "ICPC: Submission {} judged: {:?}, accepted={}",
        submission_id, verdict, eval.is_accepted
    ));

    Ok(OnSubmissionOutput {
        success: true,
        error_message: None,
    })
}

#[cfg(test)]
fn eval_result(
    outcomes: Vec<(i32, Verdict)>,
    is_compile_error: bool,
    is_accepted: bool,
) -> crate::evaluate::EvalResult {
    use crate::evaluate::{EvalOutcome, EvalResult};
    EvalResult {
        outcomes: outcomes
            .into_iter()
            .map(|(tc_id, verdict)| EvalOutcome {
                test_case_id: tc_id,
                verdict,
                time_used: Some(100),
                memory_used: Some(1024),
                message: None,
                stdout: None,
                stderr: None,
            })
            .collect(),
        is_compile_error,
        is_accepted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBMISSION_ID: i32 = 1;
    const JUDGEMENT_ID: i32 = 1;
    const JUDGE_EPOCH: i32 = 1;

    #[test]
    fn accepted_sets_score_1() {
        let host = Host::mock();
        let eval = eval_result(vec![(1, Verdict::Accepted)], false, true);

        let out =
            persist_and_track(&host, SUBMISSION_ID, JUDGEMENT_ID, JUDGE_EPOCH, &eval).unwrap();

        assert!(out.success);
        let update = host.submission.last_update();
        assert_eq!(update.score, Some(1.0));
        assert_eq!(update.verdict, Some(Some(Verdict::Accepted)));
    }

    #[test]
    fn wrong_answer_sets_score_0() {
        let host = Host::mock();
        let eval = eval_result(vec![(1, Verdict::WrongAnswer)], false, false);

        let out =
            persist_and_track(&host, SUBMISSION_ID, JUDGEMENT_ID, JUDGE_EPOCH, &eval).unwrap();

        assert!(out.success);
        let update = host.submission.last_update();
        assert_eq!(update.score, Some(0.0));
    }

    #[test]
    fn compile_error_sets_compilation_error_without_verdict() {
        let host = Host::mock();
        let eval = eval_result(vec![(1, Verdict::CompileError)], true, false);

        let out =
            persist_and_track(&host, SUBMISSION_ID, JUDGEMENT_ID, JUDGE_EPOCH, &eval).unwrap();

        assert!(out.success);
        let update = host.submission.last_update();
        assert_eq!(update.status, Some(SubmissionStatus::CompilationError));
        assert_eq!(update.verdict, Some(None));
        assert_eq!(update.score, Some(0.0));
    }

    #[test]
    fn only_cancelled_or_skipped_is_system_error_not_accepted() {
        let host = Host::mock();
        // A submission whose every outcome was cancelled/skipped (host-side
        // cancellation, worker restart) was never actually judged. Even if the
        // upstream evaluator mislabels it accepted, it must not become a solve.
        let eval = eval_result(
            vec![(1, Verdict::Cancelled), (2, Verdict::Skipped)],
            false,
            true,
        );

        let out =
            persist_and_track(&host, SUBMISSION_ID, JUDGEMENT_ID, JUDGE_EPOCH, &eval).unwrap();

        assert!(out.success);
        let update = host.submission.last_update();
        assert_eq!(update.verdict, Some(Some(Verdict::SystemError)));
        assert_eq!(update.score, Some(0.0));
    }
}
