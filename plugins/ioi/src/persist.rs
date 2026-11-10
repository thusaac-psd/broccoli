use broccoli_server_sdk::prelude::*;

use crate::config::round_score;
use crate::evaluate_batch::EvalOutcome;

/// Persist the terminal submission update after evaluation.
pub fn persist_results(
    host: &Host,
    submission_id: i32,
    judgement_id: i32,
    judge_epoch: i32,
    outcomes: &[EvalOutcome],
    submission_score: f64,
) -> Result<OnSubmissionOutput, SdkError> {
    let non_skipped: Vec<_> = outcomes
        .iter()
        .filter(|o| !o.verdict.is_skipped_or_cancelled())
        .collect();

    let verdict = if non_skipped.is_empty() {
        // No test case produced a real outcome: every case was skipped or
        // cancelled (a host-side cancellation, or a worker restart before any
        // case was judged). The submission was never actually evaluated, so it
        // must NOT be finalized as a silent Accepted on the scoreboard. Emit
        // SystemError so the dispatcher retries or surfaces it. Mirrors the ICPC
        // persist guard (see `icpc::persist::persist_and_track`) so both contest
        // types finalize a never-evaluated submission identically. Unreachable
        // through the detached driver today (empty scoring sets are handled
        // upstream in `judge_with_context_detached`, and the driver fills any
        // unrecorded case with SystemError, never Skipped), but this is the
        // persist contract boundary and must not depend on those guarantees.
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
        outcomes
            .iter()
            .find(|o| o.verdict == Verdict::CompileError)
            .and_then(|o| o.message.clone())
    } else {
        None
    };

    // A never-evaluated submission (empty `non_skipped`, finalized as
    // SystemError above) awards no points, regardless of the score the caller
    // computed — matches the ICPC guard that ties the all-skipped verdict to a
    // zero score.
    let score = if non_skipped.is_empty() {
        0.0
    } else {
        round_score(submission_score)
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
        "Submission {} judged: {:?}, score {}",
        submission_id, verdict, score
    ));

    Ok(OnSubmissionOutput::ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUBMISSION_ID: i32 = 1;
    const JUDGEMENT_ID: i32 = 1;
    const JUDGE_EPOCH: i32 = 1;

    fn outcome(test_case_id: i32, verdict: Verdict) -> EvalOutcome {
        EvalOutcome {
            test_case_id,
            verdict,
            raw_score: 0.0,
            time_used: Some(100),
            memory_used: Some(1024),
            message: None,
            stdout: None,
            stderr: None,
        }
    }

    #[test]
    fn only_skipped_or_cancelled_is_system_error_not_accepted() {
        let host = Host::mock();
        // Every outcome cancelled/skipped => the submission was never actually
        // judged. It must not finalize as a silent Accepted solve, and the score
        // the caller passed is discarded (a never-evaluated submission scores 0).
        // Sibling of the ICPC persist guard
        // `only_cancelled_or_skipped_is_system_error_not_accepted`.
        let outcomes = vec![outcome(1, Verdict::Cancelled), outcome(2, Verdict::Skipped)];

        let out = persist_results(
            &host,
            SUBMISSION_ID,
            JUDGEMENT_ID,
            JUDGE_EPOCH,
            &outcomes,
            42.0,
        )
        .unwrap();

        assert!(out.success);
        let update = host.submission.last_update();
        assert_eq!(update.status, Some(SubmissionStatus::Judged));
        assert_eq!(update.verdict, Some(Some(Verdict::SystemError)));
        assert_eq!(update.score, Some(0.0));
    }

    #[test]
    fn real_outcomes_aggregate_by_severity_and_keep_score() {
        let host = Host::mock();
        // A mix with at least one real outcome aggregates by severity (WA
        // outranks AC) and passes the caller's score through untouched.
        let outcomes = vec![
            outcome(1, Verdict::Accepted),
            outcome(2, Verdict::WrongAnswer),
            outcome(3, Verdict::Skipped),
        ];

        let out = persist_results(
            &host,
            SUBMISSION_ID,
            JUDGEMENT_ID,
            JUDGE_EPOCH,
            &outcomes,
            57.0,
        )
        .unwrap();

        assert!(out.success);
        let update = host.submission.last_update();
        assert_eq!(update.status, Some(SubmissionStatus::Judged));
        assert_eq!(update.verdict, Some(Some(Verdict::WrongAnswer)));
        assert_eq!(update.score, Some(round_score(57.0)));
    }
}
