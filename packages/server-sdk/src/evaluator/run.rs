use std::collections::HashSet;

use crate::Host;
use crate::error::SdkError;
use crate::types::*;

pub fn handle_code_run(host: &Host, input: &str) -> Result<String, SdkError> {
    let req: OnCodeRunInput = serde_json::from_str(input)
        .map_err(|e| SdkError::Serialization(format!("Failed to parse OnCodeRunInput: {e}")))?;
    let output = evaluate_run(host, &req)?;
    serde_json::to_string(&output)
        .map_err(|e| SdkError::Serialization(format!("Failed to serialize OnCodeRunOutput: {e}")))
}

pub fn evaluate_run(host: &Host, req: &OnCodeRunInput) -> Result<OnCodeRunOutput, SdkError> {
    let test_cases = &req.test_cases;

    if test_cases.is_empty() {
        host.code_run.update(&CodeRunUpdate {
            code_run_id: req.id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::Accepted)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            ..Default::default()
        })?;
        return Ok(OnCodeRunOutput {
            success: true,
            error_message: None,
        });
    }

    let batch_input = StartEvaluateBatchInput {
        problem_type: req.problem_type.clone(),
        test_cases: test_cases
            .iter()
            .map(|tc| StartEvaluateCaseInput {
                problem_id: req.problem_id,
                test_case_id: tc.id,
                solution_source: req
                    .files
                    .iter()
                    .map(|f| SourceFile {
                        filename: f.filename.clone(),
                        content: f.content.clone(),
                    })
                    .collect(),
                solution_language: req.language.clone(),
                time_limit_ms: req.time_limit_ms,
                memory_limit_kb: req.memory_limit_kb,
                contest_id: req.contest_id,
                input: tc.input.clone(),
                expected_output: tc.expected_output.clone(),
                is_custom: tc.is_custom,
                // Code-run flow does not currently support worker pinning.
                target_worker_id: None,
            })
            .collect(),
    };

    let mut verdicts: Vec<TcResult> = Vec::new();

    let mut eval_session = match host
        .eval
        .windowed(&batch_input)
        .concurrency(test_cases.len())
        .start()
    {
        Ok(session) => session,
        Err(e) => {
            let failed: Vec<TcResult> = test_cases
                .iter()
                .map(|tc| TcResult::system_error(tc.id, format!("BATCH_START_FAILED: {e:?}")))
                .collect();
            insert_code_run_tc_results(host, req.id, req.judge_epoch, &failed)?;
            verdicts.extend(failed);
            return finalize_code_run(host, req.id, req.judge_epoch, &verdicts);
        }
    };

    host.code_run.set_compiling(req.id, req.judge_epoch)?;

    let _ = host.log.info(&format!(
        "Run: evaluating {} test case(s)",
        test_cases.len()
    ));

    let mut collected = 0;
    let mut timed_out = false;
    let mut marked_running = false;
    let result_timeout_ms = default_evaluation_result_timeout_ms(req.time_limit_ms);

    while collected < test_cases.len() {
        match eval_session.next_result(result_timeout_ms) {
            Ok(Some(verdict)) => {
                let r = TcResult::from_verdict(&verdict);
                if r.verdict == Verdict::CompileError {
                    let first_new = verdicts.len();
                    verdicts.push(r);
                    let skipped = unrecorded(test_cases, &verdicts, |id| {
                        TcResult::skipped(id, "SKIPPED_SHORT_CIRCUIT".into())
                    });
                    verdicts.extend(skipped);
                    insert_code_run_tc_results(
                        host,
                        req.id,
                        req.judge_epoch,
                        &verdicts[first_new..],
                    )?;
                    let _ = eval_session.cancel_all();
                    break;
                }
                if !marked_running {
                    host.code_run.set_running(req.id, req.judge_epoch)?;
                    marked_running = true;
                }
                insert_code_run_tc_results(
                    host,
                    req.id,
                    req.judge_epoch,
                    std::slice::from_ref(&r),
                )?;
                verdicts.push(r);
                collected += 1;
            }
            Ok(None) => {
                let _ = host.log.info(&format!(
                    "Timeout waiting for result {}/{}",
                    collected + 1,
                    test_cases.len()
                ));
                timed_out = true;
                break;
            }
            Err(e) => {
                let _ = host.log.info(&format!("Error polling result: {e:?}"));
                timed_out = true;
                break;
            }
        }
    }

    if timed_out {
        let missing = unrecorded(test_cases, &verdicts, |id| {
            TcResult::system_error(id, "EVALUATION_TIMEOUT".into())
        });
        insert_code_run_tc_results(host, req.id, req.judge_epoch, &missing)?;
        verdicts.extend(missing);
        let _ = eval_session.cancel_all();
    }

    finalize_code_run(host, req.id, req.judge_epoch, &verdicts)
}

struct TcResult {
    test_case_id: i32,
    verdict: Verdict,
    score: f64,
    time_used: Option<i32>,
    memory_used: Option<i32>,
    message: Option<String>,
    stdout: Option<String>,
    stderr: Option<String>,
}

impl TcResult {
    fn from_verdict(v: &TestCaseVerdict) -> Self {
        let score = if v.score.is_finite() {
            v.score.clamp(0.0, 1.0)
        } else {
            0.0
        };
        Self {
            test_case_id: v.test_case_id,
            verdict: v.verdict.clone(),
            score,
            time_used: v.time_used_ms.map(|t| t.clamp(0, i32::MAX as i64) as i32),
            memory_used: v.memory_used_kb.map(|m| m.clamp(0, i32::MAX as i64) as i32),
            message: v.message.clone(),
            stdout: v.stdout.clone(),
            stderr: v.stderr.clone(),
        }
    }

    fn system_error(tc_id: i32, message: String) -> Self {
        Self {
            test_case_id: tc_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used: None,
            memory_used: None,
            message: Some(message),
            stdout: None,
            stderr: None,
        }
    }

    fn skipped(tc_id: i32, message: String) -> Self {
        Self {
            test_case_id: tc_id,
            verdict: Verdict::Skipped,
            score: 0.0,
            time_used: None,
            memory_used: None,
            message: Some(message),
            stdout: None,
            stderr: None,
        }
    }
}

/// A result made by `fill` for every case `verdicts` has none for.
fn unrecorded(
    test_cases: &[TestCaseRow],
    verdicts: &[TcResult],
    fill: impl Fn(i32) -> TcResult,
) -> Vec<TcResult> {
    let recorded: HashSet<i32> = verdicts.iter().map(|r| r.test_case_id).collect();
    test_cases
        .iter()
        .filter(|tc| !recorded.contains(&tc.id))
        .map(|tc| fill(tc.id))
        .collect()
}

/// Insert `results` in one host call.
fn insert_code_run_tc_results(
    host: &Host,
    code_run_id: i32,
    judge_epoch: i32,
    results: &[TcResult],
) -> Result<(), SdkError> {
    let rows: Vec<CodeRunResultRow> = results
        .iter()
        .map(|r| CodeRunResultRow {
            code_run_id,
            judge_epoch,
            run_index: r.test_case_id,
            verdict: r.verdict.clone(),
            score: r.score,
            time_used: r.time_used,
            memory_used: r.memory_used,
            message: r.message.clone(),
            stdout: r.stdout.clone(),
            stderr: r.stderr.clone(),
        })
        .collect();
    host.code_run.insert_results(&rows)
}

fn finalize_code_run(
    host: &Host,
    code_run_id: i32,
    judge_epoch: i32,
    results: &[TcResult],
) -> Result<OnCodeRunOutput, SdkError> {
    let non_skipped: Vec<_> = results
        .iter()
        .filter(|r| !r.verdict.is_skipped_or_cancelled())
        .collect();

    let verdict = non_skipped
        .iter()
        .map(|r| r.verdict.clone())
        .max_by_key(|v| v.severity())
        .unwrap_or(Verdict::Accepted);

    let max_time = non_skipped.iter().filter_map(|r| r.time_used).max();
    let max_memory = non_skipped.iter().filter_map(|r| r.memory_used).max();
    let total_score: f64 = non_skipped.iter().map(|r| r.score).sum();

    let is_ce = verdict == Verdict::CompileError;
    let status = if is_ce {
        SubmissionStatus::CompilationError
    } else {
        SubmissionStatus::Judged
    };
    let db_verdict = if is_ce { None } else { Some(verdict.clone()) };

    let compile_output = if is_ce {
        results
            .iter()
            .find(|r| r.verdict == Verdict::CompileError)
            .and_then(|r| r.message.clone())
    } else {
        None
    };

    host.code_run.update(&CodeRunUpdate {
        code_run_id,
        judge_epoch,
        status: Some(status),
        verdict: Some(db_verdict),
        score: Some(total_score),
        time_used: Some(max_time),
        memory_used: Some(max_memory),
        compile_output: Some(compile_output),
        error_code: None,
        error_message: None,
    })?;

    let _ = host.log.info(&format!(
        "Code run {} complete: {:?}, score {:.2}",
        code_run_id, verdict, total_score
    ));

    Ok(OnCodeRunOutput {
        success: true,
        error_message: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code_run_input(test_cases: Vec<TestCaseRow>) -> OnCodeRunInput {
        OnCodeRunInput {
            id: 42,
            judge_epoch: 0,
            user_id: 7,
            problem_id: 11,
            contest_id: Some(3),
            files: vec![SourceFile {
                filename: "main.cpp".into(),
                content: "int main(){}".into(),
            }],
            language: "cpp".into(),
            time_limit_ms: 1000,
            memory_limit_kb: 262_144,
            problem_type: "standard".into(),
            test_cases,
        }
    }

    fn test_case(id: i32) -> TestCaseRow {
        TestCaseRow {
            id,
            score: 1.0,
            is_sample: false,
            position: id - 1,
            description: None,
            label: Some(id.to_string()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        }
    }

    #[test]
    fn compile_error_fills_remaining_code_run_cases_with_skipped() {
        let host = Host::mock();
        host.eval.queue_result(TestCaseVerdict::compile_error(1));
        let req = code_run_input(vec![test_case(1), test_case(2), test_case(3)]);

        let output = evaluate_run(&host, &req).unwrap();

        assert!(output.success);
        let results = host.code_run.results();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].run_index, 1);
        assert_eq!(results[0].verdict, Verdict::CompileError);
        assert_eq!(results[1].run_index, 2);
        assert_eq!(results[1].verdict, Verdict::Skipped);
        assert_eq!(results[1].message.as_deref(), Some("SKIPPED_SHORT_CIRCUIT"));
        assert_eq!(results[2].run_index, 3);
        assert_eq!(results[2].verdict, Verdict::Skipped);
        assert_eq!(results[2].message.as_deref(), Some("SKIPPED_SHORT_CIRCUIT"));
        assert!(host.eval.was_cancelled());

        let updates = host.code_run.updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::CompilationError));
        assert_eq!(updates[1].verdict, Some(None));
        assert!(
            updates
                .iter()
                .all(|update| update.status != Some(SubmissionStatus::Running))
        );
    }

    #[test]
    fn a_compile_error_records_every_code_run_case_in_one_insert() {
        let host = Host::mock();
        host.eval.queue_result(TestCaseVerdict::compile_error(1));
        let req = code_run_input(vec![test_case(1), test_case(2), test_case(3)]);

        evaluate_run(&host, &req).unwrap();

        assert_eq!(host.code_run.results().len(), 3);
        assert_eq!(host.code_run.insert_call_count(), 1);
    }

    #[test]
    fn non_compile_result_moves_code_run_from_compiling_to_running() {
        let host = Host::mock();
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(2));
        let req = code_run_input(vec![test_case(1), test_case(2)]);

        let output = evaluate_run(&host, &req).unwrap();

        assert!(output.success);
        let updates = host.code_run.updates();
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::Running));
        assert_eq!(updates[2].status, Some(SubmissionStatus::Judged));
    }
}
