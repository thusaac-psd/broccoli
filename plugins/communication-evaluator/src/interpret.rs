use broccoli_server_sdk::evaluator::{CompileOutcome, RunOutcome};
use broccoli_server_sdk::types::{OperationResult, TestCaseVerdict, Verdict};

/// POSIX signal number for SIGPIPE: the program wrote to a pipe nobody reads
/// any more. A contestant killed by it wrote after the manager closed its end,
/// so the manager finished the interaction and its verdict decides. A manager
/// killed by it wrote after every contestant left, which the contestants
/// caused, so it is never the problem's failure.
const SIGPIPE: i32 = 13;

/// Interpret the communication operation result into a TestCaseVerdict.
pub fn interpret_result(
    test_case_id: i32,
    result: &OperationResult,
    num_processes: usize,
) -> TestCaseVerdict {
    if result.is_cancelled_by_host() {
        return TestCaseVerdict {
            test_case_id,
            verdict: Verdict::Cancelled,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: result.error.clone(),
            stdout: None,
            stderr: None,
            problem_failure: None,
        };
    }

    // Operation-level failure with no results
    if !result.success && result.task_results.is_empty() {
        return TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: result.error.clone().or(Some("Operation failed".into())),
            stdout: None,
            stderr: None,
            problem_failure: None,
        };
    }

    if let Some(compile_mgr) = result.task_results.get("compile_manager") {
        match CompileOutcome::of(compile_mgr) {
            CompileOutcome::Compiled => {}
            // The compiler rejected the problem's manager: every attempt
            // fails alike, so the failure stands.
            CompileOutcome::Rejected { diagnostics } => {
                let failure = truncate(&diagnostics, "Manager compilation failed");
                return TestCaseVerdict {
                    problem_failure: Some(failure.clone()),
                    ..without_run(test_case_id, Verdict::SystemError, failure)
                };
            }
            CompileOutcome::Fault { reason } => {
                return without_run(
                    test_case_id,
                    Verdict::SystemError,
                    format!("Manager: {}", truncate(&reason, "compilation failed")),
                );
            }
        }
    }

    for i in 0..num_processes {
        let Some(compile) = result.task_results.get(&format!("compile_contestant_{i}")) else {
            continue;
        };
        match CompileOutcome::of(compile) {
            CompileOutcome::Compiled => {}
            CompileOutcome::Rejected { diagnostics } => {
                return without_run(
                    test_case_id,
                    Verdict::CompileError,
                    truncate(&diagnostics, "Compilation failed"),
                );
            }
            CompileOutcome::Fault { reason } => {
                return without_run(
                    test_case_id,
                    Verdict::SystemError,
                    format!(
                        "Contestant {i}: {}",
                        truncate(&reason, "compilation failed")
                    ),
                );
            }
        }
    }

    let mut total_time_s: f64 = 0.0;
    let mut max_memory_kb: Option<u32> = None;
    let mut runs = Vec::with_capacity(num_processes);
    for i in 0..num_processes {
        let step_id = format!("run_contestant_{i}");
        let Some(run) = result.task_results.get(&step_id) else {
            return without_run(
                test_case_id,
                Verdict::SystemError,
                format!("Missing expected step '{step_id}' in operation result"),
            );
        };
        total_time_s += run.sandbox_result.time_used;
        if let Some(mem) = run.sandbox_result.memory_used {
            max_memory_kb = Some(max_memory_kb.map_or(mem, |m: u32| m.max(mem)));
        }
        runs.push(run);
    }

    let mut contestants_exited = true;
    for (i, run) in runs.into_iter().enumerate() {
        let sandbox = &run.sandbox_result;
        let (verdict, message) = match RunOutcome::of(run) {
            RunOutcome::Exited => continue,
            // SIGPIPE means the contestant wrote after the interactor closed
            // its end: the interactor finished the interaction and decided
            // the verdict, so it is the authority, not the incidental signal.
            RunOutcome::Signaled(Some(SIGPIPE)) => {
                contestants_exited = false;
                continue;
            }
            RunOutcome::NotRun => (
                Verdict::SystemError,
                format!("Contestant {i} run step was skipped"),
            ),
            RunOutcome::SandboxFault => (
                Verdict::SystemError,
                format!(
                    "Contestant {i}: {}",
                    truncate(&sandbox.message, "the sandbox reported an internal error")
                ),
            ),
            RunOutcome::MemoryLimitExceeded => (
                Verdict::MemoryLimitExceeded,
                format!(
                    "Memory limit exceeded (contestant {i}, {}KB)",
                    sandbox.memory_used.unwrap_or(0)
                ),
            ),
            RunOutcome::TimedOut => (
                Verdict::TimeLimitExceeded,
                format!("Time limit exceeded (contestant {i})"),
            ),
            RunOutcome::Signaled(_) => (
                Verdict::RuntimeError,
                format!("Signal received (contestant {i}): {}", sandbox.message),
            ),
            RunOutcome::NonZeroExit(code) => (
                Verdict::RuntimeError,
                format!("Runtime error (contestant {i}, exit code: {code})"),
            ),
        };
        return TestCaseVerdict {
            test_case_id,
            verdict,
            score: 0.0,
            time_used_ms: time_to_ms(total_time_s),
            memory_used_kb: max_memory_kb.map(|m| m as i64),
            message: Some(message),
            stdout: None,
            stderr: opt_nonempty(&sandbox.stderr),
            problem_failure: None,
        };
    }

    let run_mgr = match result.task_results.get("run_manager") {
        Some(r) => r,
        None => {
            return TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: time_to_ms(total_time_s),
                memory_used_kb: max_memory_kb.map(|m| m as i64),
                message: Some("Manager run step missing".into()),
                stdout: None,
                stderr: None,
                problem_failure: None,
            };
        }
    };

    let mgr_sandbox = &run_mgr.sandbox_result;
    let manager_failed =
        |verdict: Verdict, message: String, problem_failure: Option<String>| TestCaseVerdict {
            test_case_id,
            verdict,
            score: 0.0,
            time_used_ms: time_to_ms(total_time_s),
            memory_used_kb: max_memory_kb.map(|m| m as i64),
            message: Some(message),
            stdout: None,
            stderr: opt_nonempty(&mgr_sandbox.stderr),
            problem_failure,
        };
    let detail = || truncate(&mgr_sandbox.stderr, "manager error");
    // A manager that ran out of time or memory may have been waiting on or
    // buffering a contestant's output, and a sandbox fault or a step that
    // never ran says nothing about the manager: the platform judges these
    // again.
    let judged_again =
        |how: &str| manager_failed(Verdict::SystemError, format!("{how}: {}", detail()), None);
    match RunOutcome::of(run_mgr) {
        RunOutcome::Exited => {}
        // The manager wrote to a pipe no contestant reads any more: every
        // contestant exited (the loop above returned otherwise, or let a
        // contestant's own SIGPIPE through) before the interaction was over.
        // A contestant that left early fails the case; when the contestants
        // died of SIGPIPE too, each closed on the other and the platform
        // judges it again.
        RunOutcome::Signaled(Some(SIGPIPE)) => {
            return if contestants_exited {
                manager_failed(
                    Verdict::WrongAnswer,
                    "The contestant exited before the interaction was over".into(),
                    None,
                )
            } else {
                manager_failed(
                    Verdict::SystemError,
                    "The manager and the contestants closed their pipes on each other".into(),
                    None,
                )
            };
        }
        // The problem's manager failed on its own: every attempt fails alike,
        // so the failure stands.
        RunOutcome::NonZeroExit(code) => {
            let failure = format!("Manager exited with code {code}");
            return manager_failed(
                Verdict::SystemError,
                format!("{failure}: {}", detail()),
                Some(failure),
            );
        }
        RunOutcome::Signaled(signal) => {
            let failure = match signal {
                Some(n) => format!("Manager was terminated by signal {n}"),
                None => "Manager was terminated by a signal".to_string(),
            };
            return manager_failed(
                Verdict::SystemError,
                format!("{failure}: {}", detail()),
                Some(failure),
            );
        }
        RunOutcome::TimedOut => return judged_again("Manager timed out"),
        RunOutcome::MemoryLimitExceeded => {
            return judged_again("Manager exceeded its memory limit");
        }
        RunOutcome::SandboxFault => {
            return judged_again("Manager sandbox reported an internal error");
        }
        RunOutcome::NotRun => return judged_again("Manager run step was skipped"),
    }

    let score_str = mgr_sandbox.stdout.lines().next().unwrap_or("").trim();
    let score: f64 = match score_str.parse::<f64>() {
        Ok(s) if s.is_finite() => s,
        _ => {
            return TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: time_to_ms(total_time_s),
                memory_used_kb: max_memory_kb.map(|m| m as i64),
                message: Some(format!(
                    "Manager stdout is not a valid score: '{}'",
                    score_str
                )),
                stdout: opt_nonempty(&mgr_sandbox.stdout),
                stderr: opt_nonempty(&mgr_sandbox.stderr),
                problem_failure: None,
            };
        }
    };

    let message = opt_nonempty(mgr_sandbox.stderr.trim());

    // `score` is guaranteed finite here (the `is_finite()` guard above rejects
    // NaN/infinite manager output), so `clamp` is equivalent to the previous
    // `.min(1.0).max(0.0)` chain without clamp's NaN-propagation caveat.
    let capped_score = score.clamp(0.0, 1.0);
    let verdict = if capped_score >= 1.0 {
        Verdict::Accepted
    } else {
        Verdict::WrongAnswer
    };

    TestCaseVerdict {
        test_case_id,
        verdict,
        score: capped_score,
        time_used_ms: time_to_ms(total_time_s),
        memory_used_kb: max_memory_kb.map(|m| m as i64),
        message,
        stdout: opt_nonempty(&mgr_sandbox.stdout),
        stderr: opt_nonempty(&mgr_sandbox.stderr),
        problem_failure: None,
    }
}

/// A verdict for a case whose contestants never ran.
fn without_run(test_case_id: i32, verdict: Verdict, message: String) -> TestCaseVerdict {
    TestCaseVerdict {
        test_case_id,
        verdict,
        score: 0.0,
        time_used_ms: None,
        memory_used_kb: None,
        message: Some(message),
        stdout: None,
        stderr: None,
        problem_failure: None,
    }
}

fn time_to_ms(secs: f64) -> Option<i64> {
    if secs >= 0.0 && secs.is_finite() && secs < (i64::MAX as f64 / 1000.0) {
        Some((secs * 1000.0) as i64)
    } else {
        None
    }
}

fn opt_nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn truncate(s: &str, fallback: &str) -> String {
    if s.is_empty() {
        return fallback.to_string();
    }
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(4096).collect();
    if chars.next().is_some() {
        format!("{head}... (truncated)")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use broccoli_server_sdk::types::{ExecutionResult, SandboxStatus, TaskExecutionResult};
    use std::collections::HashMap;

    const MEM_LIMIT: u32 = 262_144; // 256 MB

    fn ok_sandbox(exit_code: i32, time: f64, memory: u32) -> ExecutionResult {
        ExecutionResult {
            exit_code: Some(exit_code),
            time_used: time,
            wall_time_used: time * 2.0,
            memory_used: Some(memory),
            status: if exit_code == 0 {
                "OK".to_string()
            } else {
                "RE".to_string()
            },
            sandbox_status: if exit_code == 0 {
                SandboxStatus::Ok
            } else {
                SandboxStatus::NonZeroExit
            },
            ..Default::default()
        }
    }

    fn task_result(
        id: &str,
        success: bool,
        sandbox: ExecutionResult,
    ) -> (String, TaskExecutionResult) {
        (
            id.to_string(),
            TaskExecutionResult {
                task_id: id.to_string(),
                success,
                sandbox_result: sandbox,
                collected_outputs: HashMap::new(),
            },
        )
    }

    fn mgr_result(score: &str, message: &str) -> ExecutionResult {
        ExecutionResult {
            exit_code: Some(0),
            time_used: 0.5,
            wall_time_used: 1.0,
            memory_used: Some(4096),
            status: "OK".to_string(),
            sandbox_status: SandboxStatus::Ok,
            stdout: score.to_string(),
            stderr: message.to_string(),
            ..Default::default()
        }
    }

    fn operation(tasks: Vec<(String, TaskExecutionResult)>) -> OperationResult {
        OperationResult {
            success: true,
            task_results: HashMap::from_iter(tasks),
            error: None,
        }
    }

    /// An operation whose manager compile ended as `sandbox`.
    fn manager_build(success: bool, sandbox: ExecutionResult) -> OperationResult {
        operation(vec![task_result("compile_manager", success, sandbox)])
    }

    /// An operation whose contestant exited 0 and whose manager ran as
    /// `sandbox`.
    fn manager_run(success: bool, sandbox: ExecutionResult) -> OperationResult {
        operation(vec![
            task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
            task_result("compile_contestant_0", true, ok_sandbox(0, 1.0, 8192)),
            task_result("run_contestant_0", true, ok_sandbox(0, 0.5, 4096)),
            task_result("run_manager", success, sandbox),
        ])
    }

    fn ended(
        sandbox_status: SandboxStatus,
        exit_code: Option<i32>,
        stderr: &str,
    ) -> ExecutionResult {
        ExecutionResult {
            exit_code,
            status: "XX".to_string(),
            sandbox_status,
            stderr: stderr.to_string(),
            ..Default::default()
        }
    }

    /// The problem is at fault (`problem_failure`, which stands) only for a
    /// failure every attempt repeats and no contestant can cause: the
    /// manager's compiler rejecting its source, or the manager exiting
    /// non-zero or dying of its own signal. A compile or run the platform
    /// broke (a sandbox fault, a killed or timed-out compiler, a step that
    /// never ran) is judged again.
    #[test]
    fn only_the_managers_own_failure_is_the_problems_fault() {
        let rejected = manager_build(
            false,
            ended(
                SandboxStatus::NonZeroExit,
                Some(1),
                "manager.cpp:3: error: expected ';'",
            ),
        );
        let verdict = interpret_result(42, &rejected, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
        assert!(
            verdict
                .problem_failure
                .as_deref()
                .is_some_and(|f| f.contains("expected ';'")),
            "{verdict:?}"
        );

        for (name, success, sandbox) in [
            (
                "silent non-zero compile",
                false,
                ended(SandboxStatus::NonZeroExit, Some(1), ""),
            ),
            (
                "compile sandbox fault",
                false,
                ended(SandboxStatus::InternalError, Some(1), "box clobbered"),
            ),
            (
                "compiler killed",
                false,
                ended(SandboxStatus::Signaled, None, ""),
            ),
            (
                "compiler timed out",
                false,
                ended(SandboxStatus::TimedOut, None, ""),
            ),
            (
                "compiler could not fork",
                false,
                ended(
                    SandboxStatus::NonZeroExit,
                    Some(1),
                    "g++: fatal error: cannot fork: Resource temporarily unavailable",
                ),
            ),
        ] {
            let verdict = interpret_result(42, &manager_build(success, sandbox), 1);
            assert_eq!(verdict.verdict, Verdict::SystemError, "{name}");
            assert_eq!(verdict.problem_failure, None, "{name}: {verdict:?}");
        }

        let exited = interpret_result(
            42,
            &manager_run(
                false,
                ended(SandboxStatus::NonZeroExit, Some(3), "assertion failed"),
            ),
            1,
        );
        assert_eq!(exited.verdict, Verdict::SystemError);
        assert_eq!(
            exited.problem_failure.as_deref(),
            Some("Manager exited with code 3")
        );

        let crashed = interpret_result(42, &manager_run(false, signal_sandbox(11)), 1);
        assert_eq!(crashed.verdict, Verdict::SystemError);
        assert_eq!(
            crashed.problem_failure.as_deref(),
            Some("Manager was terminated by signal 11")
        );

        for (name, success, sandbox) in [
            (
                "run sandbox fault",
                false,
                ended(SandboxStatus::InternalError, None, "cgroup gone"),
            ),
            (
                "run timed out",
                false,
                ended(SandboxStatus::TimedOut, None, ""),
            ),
            (
                "run never ran",
                false,
                ended(SandboxStatus::Unknown, None, ""),
            ),
        ] {
            let verdict = interpret_result(42, &manager_run(success, sandbox), 1);
            assert_eq!(verdict.verdict, Verdict::SystemError, "{name}");
            assert_eq!(verdict.problem_failure, None, "{name}: {verdict:?}");
        }

        let lost = OperationResult {
            success: false,
            task_results: HashMap::new(),
            error: Some("worker gone".into()),
        };
        let verdict = interpret_result(42, &lost, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
        assert_eq!(verdict.problem_failure, None, "the platform's failure");
    }

    /// A manager killed by SIGPIPE wrote to a contestant that had already
    /// left: every contestant exited before the interaction was over, which
    /// is the submission's failure, not the problem's.
    #[test]
    fn a_manager_writing_to_a_contestant_that_left_is_the_contestants_failure() {
        let verdict = interpret_result(42, &manager_run(false, signal_sandbox(SIGPIPE)), 1);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer, "{verdict:?}");
        assert_eq!(verdict.score, 0.0);
        assert_eq!(verdict.problem_failure, None);
    }

    /// When the contestant died of SIGPIPE too, each closed on the other and
    /// nobody's fault is known: the platform judges it again.
    #[test]
    fn a_manager_and_contestant_both_dying_of_sigpipe_is_judged_again() {
        let verdict = interpret_result(
            42,
            &operation(vec![
                task_result("compile_contestant_0", true, ok_sandbox(0, 1.0, 8192)),
                task_result("run_contestant_0", false, signal_sandbox(SIGPIPE)),
                task_result("run_manager", false, signal_sandbox(SIGPIPE)),
            ]),
            1,
        );
        assert_eq!(verdict.verdict, Verdict::SystemError, "{verdict:?}");
        assert_eq!(verdict.problem_failure, None);
    }

    #[test]
    fn accepted_when_score_is_1() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", true, ok_sandbox(0, 1.0, 8192)),
                task_result("run_manager", true, mgr_result("1.0\n", "Correct\n")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.5, 4096)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::Accepted);
        assert_eq!(verdict.score, 1.0);
        assert_eq!(verdict.message, Some("Correct".into()));
    }

    #[test]
    fn partial_score_when_between_0_and_1() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.75\n", "Partial\n")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer);
        assert_eq!(verdict.score, 0.75);
    }

    #[test]
    fn wrong_answer_when_score_is_0() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0\n", "Wrong\n")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.2, 1024)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer);
        assert_eq!(verdict.score, 0.0);
    }

    #[test]
    fn contestant_tle() {
        let mut tle_sandbox = ok_sandbox(0, 2.0, 4096);
        tle_sandbox.killed = true;
        tle_sandbox.status = "TO".to_string();
        tle_sandbox.sandbox_status = SandboxStatus::TimedOut;
        tle_sandbox.exit_code = None;

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0", "")),
                task_result("run_contestant_0", false, tle_sandbox),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::TimeLimitExceeded);
    }

    #[test]
    fn contestant_killed_by_the_oom_killer_exceeded_memory() {
        let mut mle_sandbox = ok_sandbox(0, 0.5, MEM_LIMIT);
        mle_sandbox.killed = true;
        mle_sandbox.cg_oom_killed = true;
        mle_sandbox.exit_code = None;
        mle_sandbox.status = "SG".to_string();
        mle_sandbox.sandbox_status = SandboxStatus::Signaled;

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0", "")),
                task_result("run_contestant_0", false, mle_sandbox),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::MemoryLimitExceeded);
    }

    /// A worker that lost a contestant's run (isolate `XX`, a step it could
    /// not collect) says nothing about the solution: judged again, never a
    /// runtime error.
    #[test]
    fn contestant_sandbox_internal_error_is_system_error() {
        let lost = ExecutionResult {
            exit_code: Some(1),
            status: "XX".to_string(),
            sandbox_status: SandboxStatus::InternalError,
            message: "box clobbered".to_string(),
            ..Default::default()
        };
        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("1.0", "")),
                task_result("run_contestant_0", false, lost),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn contestant_runtime_error() {
        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0", "")),
                task_result("run_contestant_0", false, ok_sandbox(11, 0.1, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::RuntimeError);
    }

    fn signal_sandbox(signal: i32) -> ExecutionResult {
        ExecutionResult {
            exit_code: None,
            signal: Some(signal),
            status: "SG".to_string(),
            sandbox_status: SandboxStatus::Signaled,
            time_used: 0.1,
            memory_used: Some(2048),
            ..Default::default()
        }
    }

    #[test]
    fn contestant_sigpipe_defers_to_manager_accepted() {
        // A correct solution that writes once more after the interactor has
        // decided + closed the pipe gets SIGPIPE. The manager's clean AC must
        // win - reporting RuntimeError here is the classic interactive bug.
        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("1.0\n", "Correct\n")),
                task_result("run_contestant_0", false, signal_sandbox(SIGPIPE)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::Accepted);
        assert_eq!(verdict.score, 1.0);
    }

    #[test]
    fn contestant_sigpipe_defers_to_manager_wrong_answer() {
        // Interactor decided WA and closed the pipe first; the solution's next
        // write -> SIGPIPE. Verdict must be the manager's WA, not RuntimeError.
        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0\n", "Wrong\n")),
                task_result("run_contestant_0", false, signal_sandbox(SIGPIPE)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer);
    }

    #[test]
    fn contestant_non_sigpipe_signal_is_runtime_error() {
        // A genuine crash (SIGSEGV = 11) is still RuntimeError, even when the
        // manager would have produced a score.
        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("1.0\n", "")),
                task_result("run_contestant_0", false, signal_sandbox(11)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::RuntimeError);
    }

    #[test]
    fn contestant_compile_error() {
        let mut ce = ok_sandbox(1, 1.0, 8192);
        ce.stderr = "error: expected ';'".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, ce),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::CompileError);
        assert!(verdict.message.unwrap().contains("expected ';'"));
    }

    #[test]
    fn contestant_compile_nonzero_without_diagnostics_is_system_error() {
        // A contestant Java compile that fails to spawn its VM threads under
        // transient per-uid RLIMIT_NPROC pressure exits non-zero writing nothing.
        // A real compile error always carries diagnostics, so silence is the
        // platform's fault and judged again.
        let mut silent = ok_sandbox(1, 1.0, 8192);
        silent.stderr = String::new();
        silent.stdout = String::new();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, silent),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn contestant_compile_nonzero_with_diagnostics_is_compile_error() {
        // The real-error path must survive the silent-infra reclassification: any
        // diagnostics present -> terminal CompileError.
        let mut ce = ok_sandbox(1, 1.0, 8192);
        ce.stderr = "error: cannot find symbol".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, ce),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::CompileError);
        assert!(verdict.message.unwrap().contains("cannot find symbol"));
    }

    #[test]
    fn contestant_compile_jvm_thread_eagain_is_system_error() {
        // A Java compile that cannot start its VM threads says so instead of
        // diagnosing the source: the platform's fault.
        let mut infra = ok_sandbox(1, 1.0, 8192);
        infra.stderr = "Failed to start thread \"Unknown thread\" - pthread_create failed \
                        (EAGAIN)\nError occurred during initialization of VM\n\
                        java.lang.OutOfMemoryError: unable to create native thread"
            .to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, infra),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn contestant_compile_aborted_without_exit_is_system_error() {
        // A compile killed before it could exit (signal / wall-timeout / OOM) has
        // exit_code == None. Even with partial output it is an infra fault, not a
        // source error -> SystemError, matching server-sdk's exit_code None arm.
        // This is the case the minimal `!success` gate got wrong; the faithful
        // exit_code mirror fixes it.
        let mut aborted = ok_sandbox(1, 1.0, 8192);
        aborted.exit_code = None;
        aborted.status = "TO".to_string();
        aborted.sandbox_status = SandboxStatus::TimedOut;
        aborted.stderr = "partial diagnostics before the kill".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, aborted),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn contestant_compile_internal_error_is_system_error() {
        // An isolate-tagged internal error (clobbered box, vanished redirect file)
        // outranks any exit_code and diagnostics -> SystemError. Precedence must
        // match server-sdk: InternalError is checked before exit_code.
        let mut internal = ok_sandbox(1, 1.0, 8192);
        internal.status = "XX".to_string();
        internal.sandbox_status = SandboxStatus::InternalError;
        internal.stderr = "error: looks like a real diagnostic".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, internal),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn manager_compile_failure_is_system_error() {
        let mut ce = ok_sandbox(1, 1.0, 8192);
        ce.stderr = "manager.cpp: error".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([task_result("compile_manager", false, ce)]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn manager_nonzero_exit_is_system_error() {
        let mut mgr = mgr_result("0.5", "internal error");
        mgr.exit_code = Some(1);
        mgr.status = "RE".to_string();
        mgr.sandbox_status = SandboxStatus::NonZeroExit;

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", false, mgr),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn invalid_manager_score_is_system_error() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("not_a_number", "")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
        assert!(verdict.message.unwrap().contains("not a valid score"));
    }

    #[test]
    fn nan_score_is_system_error() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("NaN", "")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
        assert!(verdict.message.unwrap().contains("not a valid score"));
    }

    #[test]
    fn inf_score_is_system_error() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("inf", "")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn n2_aggregates_time_and_memory() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("1.0\n", "")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
                task_result("run_contestant_1", true, ok_sandbox(0, 0.7, 4096)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 2);
        assert_eq!(verdict.verdict, Verdict::Accepted);
        // Time = sum: 0.3 + 0.7 = 1.0s = 1000ms
        assert_eq!(verdict.time_used_ms, Some(1000));
        // Memory = max: 4096
        assert_eq!(verdict.memory_used_kb, Some(4096));
    }

    #[test]
    fn score_capped_at_1() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("1.5\n", "")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.1, 1024)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::Accepted);
        assert_eq!(verdict.score, 1.0);
    }

    #[test]
    fn negative_score_clamped_to_0() {
        let result = OperationResult {
            success: true,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("-0.5\n", "")),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.1, 1024)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer);
        assert_eq!(verdict.score, 0.0);
    }
}
