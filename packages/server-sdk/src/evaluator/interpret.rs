use crate::Checker;
use crate::error::SdkError;
use crate::types::*;

use super::outcome::{CompileOutcome, RunOutcome};

/// Canonical id of the fused `check` step. This is a contract between the
/// checker resolver (standard-checkers emits a step with this id and sets it as
/// the stage's `result_step_id`) and the batch-evaluator interpreter (looks the
/// step up by this id when turning the op result into a verdict). Both plugins
/// import this const so the two ends can never drift out of agreement.
pub const DEFAULT_CHECK_STEP_ID: &str = "check";

/// Interpret a FUSED operation result: the checker ran worker-side as a `check`
/// step in the same op, so its small result (exit code + message + sandbox
/// status) is read from `task_results` and turned into a verdict via the
/// checker plugin's `interpret`. Outcome precedence (TLE/MLE/RE/compile) is
/// that of the solution steps - a failed run wins and the checker is ignored.
///
/// The checker message is sourced from the check step's stderr - both
/// broccoli-compare and testlib write their verdict message there, while the
/// check step's stdout carries only the display preview. Worker File-redirects
/// are read back into both fields.
pub fn interpret_fused_result(
    checker: &Checker,
    test_case_id: i32,
    result: &OperationResult,
    checker_format: &str,
    check_step_id: &str,
) -> Result<TestCaseVerdict, SdkError> {
    // precheck first: it applies the correct precedence for a FAILED solution
    // step (host cancellation, compile error, and exec TLE/MLE/RE). A skipped
    // check step that was skipped *because* the solution failed must be handled
    // as that solution failure, not as an infrastructure error -- so this must
    // run before we inspect the check step below.
    if let Some(verdict) = precheck_verdict(test_case_id, result) {
        return Ok(verdict);
    }

    let time_used_ms = extract_time_used(result);
    let memory_used_kb = extract_memory_used(result);
    let exec_result = result
        .task_results
        .get(EXEC_STEP_ID)
        .expect("precheck guarantees a clean exec result");
    let exec_stdout = opt_nonempty(&exec_result.sandbox_result.stdout);
    let exec_stderr = opt_nonempty(&exec_result.sandbox_result.stderr);

    // `none`: no output comparison is performed, so the evaluator splices NO
    // check step (no worker-side comparator process - this is the hot path for
    // custom "run code", which has no expected output). precheck above already
    // cleared TLE/MLE/RE/CompileError, so a surviving result is a clean run ->
    // Accepted, with the solution's own stdout retained for display. There is no
    // check-step result to look up.
    if checker_format == "none" {
        return Ok(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::Accepted,
            score: 1.0,
            time_used_ms,
            memory_used_kb,
            message: None,
            stdout: exec_stdout,
            stderr: exec_stderr,
            problem_failure: None,
        });
    }

    // precheck confirmed a clean solution run, so the checker stage must have
    // produced an outcome; none at all is the platform's failure.
    let Some(outcome) = checker_outcome(result, check_step_id) else {
        return Ok(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms,
            memory_used_kb,
            message: Some(format!(
                "checker step '{check_step_id}' produced no result (infrastructure failure)"
            )),
            stdout: exec_stdout,
            stderr: exec_stderr,
            problem_failure: None,
        });
    };
    // Display preview: the run output preview (exec stdout, File mode) or the
    // comparator's stdout preview (Stream mode, where exec stdout is a FIFO and
    // thus empty). 64 KiB cap applied worker-side.
    let display_stdout = exec_stdout.or_else(|| {
        result
            .task_results
            .get(check_step_id)
            .and_then(|check| opt_nonempty(&check.sandbox_result.stdout))
    });
    // A checker step whose sandbox failed internally (isolate `XX`, or a
    // redirect file lost after a clean exit under load) says nothing about the
    // checker or the solution: the platform's fault, judged again.
    if outcome.sandbox.sandbox_status == SandboxStatus::InternalError {
        return Ok(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms,
            memory_used_kb,
            message: Some(format!(
                "checker step '{}' hit a sandbox internal error (infrastructure failure)",
                outcome.step_id
            )),
            stdout: display_stdout,
            stderr: exec_stderr,
            problem_failure: None,
        });
    }

    match checker.interpret(checker_format, &outcome) {
        Ok(v) => Ok(TestCaseVerdict {
            test_case_id,
            verdict: v.verdict,
            score: v.score,
            time_used_ms,
            memory_used_kb,
            message: v.message,
            stdout: display_stdout,
            stderr: exec_stderr,
            problem_failure: None,
        }),
        Err(e) => Ok(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms,
            memory_used_kb,
            message: Some(format!("Checker interpret failed: {e:?}")),
            stdout: display_stdout,
            stderr: exec_stderr,
            problem_failure: None,
        }),
    }
}

/// The solution's compile and run steps in a fused operation. Every other
/// step is the checker stage's.
const COMPILE_STEP_ID: &str = "compile";
const EXEC_STEP_ID: &str = "exec";

/// What the checker stage's checker interprets: its result step, when that
/// ran, else the stage step that failed before it so it never ran (a checker
/// that did not compile). `None` when the stage left no outcome at all.
fn checker_outcome(result: &OperationResult, check_step_id: &str) -> Option<CheckerRunOutcome> {
    let ran = |step: &TaskExecutionResult| {
        step.success || step.sandbox_result.sandbox_status != SandboxStatus::Unknown
    };
    if let Some(check) = result.task_results.get(check_step_id)
        && ran(check)
    {
        return Some(CheckerRunOutcome {
            step_id: check_step_id.to_string(),
            success: check.success,
            sandbox: ExecutionResult {
                stdout: String::new(),
                ..check.sandbox_result.clone()
            },
        });
    }
    result
        .task_results
        .iter()
        .filter(|(id, step)| {
            id.as_str() != check_step_id
                && ![COMPILE_STEP_ID, EXEC_STEP_ID].contains(&id.as_str())
                && !step.success
                && ran(step)
        })
        .min_by_key(|(id, _)| id.as_str())
        .map(|(id, step)| CheckerRunOutcome {
            step_id: id.clone(),
            success: step.success,
            sandbox: step.sandbox_result.clone(),
        })
}

/// Determine the verdict that applies BEFORE consulting the checker: host
/// cancellation, system errors, compile errors, and execution failures
/// (skipped / OOM->MLE / TO->TLE / SG,RE->RE). Returns `None` only when a clean
/// `exec` result is present, meaning the caller should consult the checker.
///
/// Used by `interpret_fused_result` (worker-side check step) to apply outcome
/// precedence before consulting the checker.
pub(crate) fn precheck_verdict(
    test_case_id: i32,
    result: &OperationResult,
) -> Option<TestCaseVerdict> {
    if result.is_cancelled_by_host() {
        return Some(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::Cancelled,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: result.error.clone(),
            stdout: None,
            stderr: None,
            problem_failure: None,
        });
    }

    if !result.success && result.task_results.is_empty() {
        return Some(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            // Never surface a null-message SystemError -- an operation that
            // produced no step results is opaque enough already.
            message: Some(result.error.clone().unwrap_or_else(|| {
                "Operation produced no step results (system fault; no error detail reported)".into()
            })),
            stdout: None,
            stderr: None,
            problem_failure: None,
        });
    }

    if let Some(compile) = result.task_results.get(COMPILE_STEP_ID) {
        match CompileOutcome::of(compile) {
            CompileOutcome::Compiled => {}
            CompileOutcome::Rejected { diagnostics } => {
                return Some(TestCaseVerdict {
                    message: truncate_stderr(&diagnostics, "Compilation failed"),
                    ..without_run(test_case_id, Verdict::CompileError)
                });
            }
            CompileOutcome::Fault { reason } => {
                return Some(TestCaseVerdict {
                    message: truncate_stderr(&reason, "Compilation failed"),
                    ..without_run(test_case_id, Verdict::SystemError)
                });
            }
        }
    }

    let Some(exec) = result.task_results.get(EXEC_STEP_ID) else {
        return Some(TestCaseVerdict {
            message: Some("No exec result found".into()),
            ..without_run(test_case_id, Verdict::SystemError)
        });
    };
    let sandbox = &exec.sandbox_result;
    let (verdict, message) = match RunOutcome::of(exec) {
        RunOutcome::Exited => return None,
        RunOutcome::NotRun => {
            return Some(TestCaseVerdict {
                message: Some("Execution step was skipped".into()),
                ..without_run(test_case_id, Verdict::SystemError)
            });
        }
        // The sandbox failed (isolate `XX`, or a redirect file that vanished
        // after a clean exit under load): what the program printed is lost,
        // and that is never the contestant's fault.
        RunOutcome::SandboxFault => (
            Verdict::SystemError,
            opt_nonempty(&sandbox.message)
                .unwrap_or_else(|| "Execution sandbox reported an internal error".into()),
        ),
        RunOutcome::MemoryLimitExceeded => (
            Verdict::MemoryLimitExceeded,
            format!(
                "Memory limit exceeded ({}KB)",
                sandbox.memory_used.unwrap_or(0)
            ),
        ),
        RunOutcome::TimedOut => (
            Verdict::TimeLimitExceeded,
            format!(
                "Time limit exceeded ({}ms)",
                extract_time_used(result).map_or("?".into(), |t| t.to_string())
            ),
        ),
        RunOutcome::Signaled(_) => (Verdict::RuntimeError, sandbox.message.clone()),
        RunOutcome::NonZeroExit(code) => (Verdict::RuntimeError, format!("Exit code: {code}")),
    };
    Some(TestCaseVerdict {
        test_case_id,
        verdict,
        score: 0.0,
        time_used_ms: extract_time_used(result),
        memory_used_kb: extract_memory_used(result),
        message: Some(message),
        stdout: opt_nonempty(&sandbox.stdout),
        stderr: opt_nonempty(&sandbox.stderr),
        problem_failure: None,
    })
}

/// A verdict for a case whose program never ran.
fn without_run(test_case_id: i32, verdict: Verdict) -> TestCaseVerdict {
    TestCaseVerdict {
        test_case_id,
        verdict,
        score: 0.0,
        time_used_ms: None,
        memory_used_kb: None,
        message: None,
        stdout: None,
        stderr: None,
        problem_failure: None,
    }
}

fn extract_time_used(result: &OperationResult) -> Option<i64> {
    result
        .task_results
        .get(EXEC_STEP_ID)
        .and_then(|exec_result| {
            let t = exec_result.sandbox_result.time_used;
            if t > 0.0 && t.is_finite() && t < (i64::MAX as f64 / 1000.0) {
                Some((t * 1000.0) as i64)
            } else {
                None
            }
        })
}

fn extract_memory_used(result: &OperationResult) -> Option<i64> {
    result
        .task_results
        .get(EXEC_STEP_ID)
        .and_then(|exec_result| exec_result.sandbox_result.memory_used.map(|m| m as i64))
}

fn opt_nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn truncate_stderr(stderr: &str, fallback: &str) -> Option<String> {
    if stderr.is_empty() {
        Some(fallback.into())
    } else if stderr.chars().count() <= 4096 {
        Some(stderr.to_string())
    } else {
        Some(format!(
            "{}... (truncated)",
            stderr.chars().take(4096).collect::<String>()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Host;
    use crate::types::{ExecutionResult, TaskExecutionResult};
    use std::collections::HashMap;

    fn task(success: bool, sandbox: ExecutionResult) -> TaskExecutionResult {
        TaskExecutionResult {
            task_id: "t".into(),
            success,
            sandbox_result: sandbox,
            collected_outputs: HashMap::new(),
        }
    }

    fn op(entries: Vec<(&str, TaskExecutionResult)>) -> OperationResult {
        let mut task_results = HashMap::new();
        for (id, t) in entries {
            task_results.insert(id.to_string(), t);
        }
        OperationResult {
            success: true,
            task_results,
            error: None,
        }
    }

    fn exec_ok() -> TaskExecutionResult {
        task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                status: "OK".into(),
                sandbox_status: SandboxStatus::Ok,
                ..Default::default()
            },
        )
    }

    fn compile_ok() -> TaskExecutionResult {
        task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                status: "OK".into(),
                sandbox_status: SandboxStatus::Ok,
                ..Default::default()
            },
        )
    }

    // ----- precheck_verdict: shared outcome precedence -----

    #[test]
    fn precheck_passes_through_clean_exec() {
        let result = op(vec![("compile", compile_ok()), ("exec", exec_ok())]);
        assert!(precheck_verdict(1, &result).is_none());
    }

    #[test]
    fn precheck_compile_error() {
        let compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "OK".into(),
                sandbox_status: SandboxStatus::Ok,
                stderr: "error: expected ';'".into(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile)])).unwrap();
        assert_eq!(v.verdict, Verdict::CompileError);
        assert!(v.message.unwrap().contains("expected"));
    }

    #[test]
    fn precheck_compile_internal_error_is_system_error() {
        // A box clobbered mid-compile makes the compiler exit non-zero (its source
        // vanished) with the declared stderr redirect also missing -> tagged
        // InternalError. The InternalError guard must fire BEFORE the exit-code
        // branch, so this becomes a retryable SystemError, not a terminal
        // CompileError that never self-heals.
        let compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                sandbox_status: SandboxStatus::InternalError,
                status: "XX".into(),
                stderr: String::new(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile)])).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn precheck_compile_nonzero_without_diagnostics_is_system_error() {
        // Under burst, a compile can complete with a non-zero exit yet emit NO
        // diagnostics on either stream -- observed when a Java compile fails to
        // spawn its VM threads under transient per-uid RLIMIT_NPROC pressure and
        // javac exits 1 writing nothing. This is NOT tagged InternalError (isolate
        // saw a clean non-zero exit, no missing files), so the InternalError guard
        // does not fire. A genuine source error ALWAYS carries the compiler's
        // diagnostics (gcc/g++/javac/py_compile all write to stderr), so silence
        // means the compiler never diagnosed anything -> retryable infra fault, not
        // a terminal CompileError that pins a permanent wrong verdict on valid code.
        let compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "RE".into(),
                sandbox_status: SandboxStatus::NonZeroExit,
                stderr: String::new(),
                stdout: String::new(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile)])).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn precheck_compile_nonzero_with_stdout_diagnostics_is_compile_error() {
        // A compiler that reports errors on STDOUT (not stderr) still failed for a
        // diagnosable source reason: any output present -> terminal CompileError.
        // Guards the silent-infra reclassification from swallowing real errors.
        let compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "RE".into(),
                sandbox_status: SandboxStatus::NonZeroExit,
                stderr: String::new(),
                stdout: "error: syntax".into(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile)])).unwrap();
        assert_eq!(v.verdict, Verdict::CompileError);
        assert!(v.message.unwrap().contains("syntax"));
    }

    #[test]
    fn precheck_compile_jvm_thread_eagain_is_system_error() {
        // Verbatim javac output captured under an N=96/CONC=48 Java burst: the
        // compile JVM cannot spawn its helper threads under transient per-uid
        // RLIMIT_NPROC pressure and exits 1 WITH output -- a VM-init fault, not a
        // source diagnostic. It must reclassify to a retryable SystemError, not the
        // terminal CompileError that pinned ~10% of valid submissions before the
        // signature check. This is the case the empty-diagnostics gate missed.
        let compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "RE".into(),
                sandbox_status: SandboxStatus::NonZeroExit,
                stderr: "[0.028s][warning][os,thread] Failed to start thread \"Unknown thread\" \
                         - pthread_create failed (EAGAIN) for attributes: stacksize: 1024k, \
                         guardsize: 0k, detached.\nError occurred during initialization of VM\n\
                         java.lang.OutOfMemoryError: unable to create native thread: possibly out \
                         of memory or process/resource limits reached"
                    .into(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile)])).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn precheck_compile_gcc_fork_failure_is_system_error() {
        // The C/C++ drivers show the same infra class when they cannot fork their
        // backend under pressure. Must also reclassify to SystemError.
        let compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "RE".into(),
                sandbox_status: SandboxStatus::NonZeroExit,
                stderr: "gcc: fatal error: cannot execute 'cc1': posix_spawn: \
                         Resource temporarily unavailable\ncompilation terminated."
                    .into(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile)])).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn precheck_time_limit_exceeded() {
        let exec = task(
            false,
            ExecutionResult {
                exit_code: None,
                status: "TO".into(),
                sandbox_status: SandboxStatus::TimedOut,
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile_ok()), ("exec", exec)])).unwrap();
        assert_eq!(v.verdict, Verdict::TimeLimitExceeded);
    }

    #[test]
    fn precheck_memory_limit_exceeded() {
        let exec = task(
            false,
            ExecutionResult {
                exit_code: None,
                status: "SG".into(),
                sandbox_status: SandboxStatus::Signaled,
                cg_oom_killed: true,
                memory_used: Some(999),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("exec", exec)])).unwrap();
        assert_eq!(v.verdict, Verdict::MemoryLimitExceeded);
    }

    #[test]
    fn precheck_runtime_error_on_signal() {
        let exec = task(
            false,
            ExecutionResult {
                exit_code: None,
                status: "SG".into(),
                sandbox_status: SandboxStatus::Signaled,
                message: "Caught fatal signal 11".into(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("exec", exec)])).unwrap();
        assert_eq!(v.verdict, Verdict::RuntimeError);
    }

    #[test]
    fn precheck_cancelled() {
        let v = precheck_verdict(1, &OperationResult::cancelled_by_host()).unwrap();
        assert_eq!(v.verdict, Verdict::Cancelled);
    }

    #[test]
    fn precheck_missing_exec_is_system_error() {
        let v = precheck_verdict(1, &op(vec![("compile", compile_ok())])).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
        assert!(v.message.unwrap().contains("No exec result"));
    }

    #[test]
    fn precheck_no_results_has_non_null_message() {
        // An operation that produced no step results (error unset) must still carry
        // a diagnostic; a null-message SystemError is opaque to admins/contestants.
        let result = OperationResult {
            success: false,
            task_results: HashMap::new(),
            error: None,
        };
        let v = precheck_verdict(1, &result).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
        let msg = v.message.expect("SystemError must carry a message");
        assert!(msg.contains("no step results"), "got: {msg}");
    }

    #[test]
    fn precheck_exec_internal_error_is_system_error() {
        // A redirect file that vanished after a clean exit (concurrency/infra) is
        // tagged InternalError by the sandbox; it must become a retryable
        // SystemError, never a terminal WrongAnswer scored off the lost output.
        let exec = task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                sandbox_status: SandboxStatus::InternalError,
                status: "XX".into(),
                stdout: String::new(),
                ..Default::default()
            },
        );
        let v = precheck_verdict(1, &op(vec![("compile", compile_ok()), ("exec", exec)])).unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn precheck_present_but_empty_output_is_not_infra() {
        // A file that is PRESENT but empty is legitimate empty program output, not
        // an infrastructure fault: precheck must pass it through to the checker
        // (which may score WrongAnswer) rather than looping forever on retries.
        let exec = task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                status: "OK".into(),
                sandbox_status: SandboxStatus::Ok,
                stdout: String::new(),
                ..Default::default()
            },
        );
        assert!(
            precheck_verdict(1, &op(vec![("compile", compile_ok()), ("exec", exec)])).is_none(),
            "empty-but-present output must not be reclassified as a system error"
        );
    }

    // ----- interpret_fused_result -----

    #[test]
    fn fused_exec_internal_error_is_system_error() {
        // Even with a check step present, an InternalError exec short-circuits to
        // SystemError before the checker is consulted (mock checker would Err with
        // a different message, so the message proves precheck won).
        let host = Host::mock();
        let exec = task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                sandbox_status: SandboxStatus::InternalError,
                status: "XX".into(),
                ..Default::default()
            },
        );
        let result = op(vec![
            ("compile", compile_ok()),
            ("exec", exec),
            ("check", exec_ok()),
        ]);
        let v = interpret_fused_result(&host.checker, 1, &result, "tokens", "check").unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
        assert!(!v.message.unwrap().contains("Checker interpret failed"));
    }

    #[test]
    fn fused_check_internal_error_is_system_error() {
        // The checker step's own sandbox failed internally -> infrastructure, not
        // a contestant verdict. The mock checker must not be consulted.
        let host = Host::mock();
        let check = task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                sandbox_status: SandboxStatus::InternalError,
                status: "XX".into(),
                ..Default::default()
            },
        );
        let result = op(vec![
            ("compile", compile_ok()),
            ("exec", exec_ok()),
            ("check", check),
        ]);
        let v = interpret_fused_result(&host.checker, 1, &result, "tokens", "check").unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
        assert!(v.message.unwrap().contains("infrastructure failure"));
    }

    #[test]
    fn fused_propagates_run_failure_without_calling_checker() {
        // A TLE must win regardless of the checker (mock checker would Err).
        let host = Host::mock();
        let exec = task(
            false,
            ExecutionResult {
                exit_code: None,
                status: "TO".into(),
                sandbox_status: SandboxStatus::TimedOut,
                ..Default::default()
            },
        );
        let result = op(vec![("compile", compile_ok()), ("exec", exec)]);
        let v = interpret_fused_result(&host.checker, 1, &result, "tokens", "check").unwrap();
        assert_eq!(v.verdict, Verdict::TimeLimitExceeded);
    }

    #[test]
    fn fused_missing_check_step_is_system_error() {
        let host = Host::mock();
        let result = op(vec![("compile", compile_ok()), ("exec", exec_ok())]);
        let v = interpret_fused_result(&host.checker, 1, &result, "tokens", "check").unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
        assert!(v.message.unwrap().contains("produced no result"));
    }

    #[test]
    fn fused_check_present_but_never_ran_is_system_error() {
        // Clean exec, but the check step exists yet never ran (launch failure:
        // not successful, empty sandbox status). This is infrastructure, not a
        // contestant verdict.
        let host = Host::mock();
        let dead_check = task(false, ExecutionResult::default());
        let result = op(vec![
            ("compile", compile_ok()),
            ("exec", exec_ok()),
            ("check", dead_check),
        ]);
        let v = interpret_fused_result(&host.checker, 1, &result, "tokens", "check").unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
    }

    #[test]
    fn fused_clean_run_consults_the_checker() {
        // Clean exec + a check step present -> interpret is consulted. The mock
        // checker returns Err, surfaced as SystemError "Checker interpret failed"
        // (the real verdict mapping is unit-tested in standard-checkers).
        let host = Host::mock();
        let check = task(
            true,
            ExecutionResult {
                exit_code: Some(1),
                status: "OK".into(),
                sandbox_status: SandboxStatus::Ok,
                stdout: "solution-output-preview".into(),
                stderr: "token 2 mismatch".into(),
                ..Default::default()
            },
        );
        let result = op(vec![
            ("compile", compile_ok()),
            ("exec", exec_ok()),
            ("check", check),
        ]);
        let v = interpret_fused_result(&host.checker, 1, &result, "tokens", "check").unwrap();
        assert_eq!(v.verdict, Verdict::SystemError);
        assert!(v.message.unwrap().contains("Checker interpret failed"));
        // Stream-mode preview fallback: exec stdout is empty (FIFO), so the
        // display preview comes from the check step's stdout.
        assert_eq!(v.stdout.as_deref(), Some("solution-output-preview"));
    }

    #[test]
    fn a_checker_that_did_not_compile_is_the_stage_outcome() {
        let failed_compile = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "RE".into(),
                sandbox_status: SandboxStatus::NonZeroExit,
                stderr: "checker.cpp:3: error: 'x' was not declared".into(),
                ..Default::default()
            },
        );
        let result = op(vec![
            ("compile", compile_ok()),
            ("exec", exec_ok()),
            ("compile_checker", failed_compile),
            ("check", task(false, ExecutionResult::default())),
        ]);
        let outcome = checker_outcome(&result, "check").expect("the failed compile decides");
        assert_eq!(outcome.step_id, "compile_checker");
        assert!(!outcome.success);
        assert_eq!(outcome.sandbox.exit_code, Some(1));
        assert!(outcome.sandbox.stderr.contains("not declared"));
    }

    #[test]
    fn a_check_that_ran_is_the_stage_outcome_without_the_output_preview() {
        let check = task(
            false,
            ExecutionResult {
                exit_code: Some(1),
                status: "RE".into(),
                sandbox_status: SandboxStatus::NonZeroExit,
                stdout: "solution-output-preview".into(),
                stderr: "wrong answer".into(),
                ..Default::default()
            },
        );
        let result = op(vec![
            ("compile", compile_ok()),
            ("exec", exec_ok()),
            ("compile_checker", task(true, ExecutionResult::default())),
            ("check", check),
        ]);
        let outcome = checker_outcome(&result, "check").unwrap();
        assert_eq!(outcome.step_id, "check");
        assert_eq!(outcome.sandbox.stderr, "wrong answer");
        assert_eq!(outcome.sandbox.stdout, "");
        let none_ran = op(vec![
            ("compile", compile_ok()),
            ("exec", exec_ok()),
            ("check", task(false, ExecutionResult::default())),
        ]);
        assert!(checker_outcome(&none_ran, "check").is_none());
    }

    // ----- interpret_fused_result: 'none' (no comparison, no check step) -----

    #[test]
    fn fused_none_accepts_clean_run_without_a_check_step() {
        // `none` schedules NO checker step (no worker process); a clean exec is
        // Accepted and the solution's own stdout is retained for display. The
        // mock checker would Err if consulted - proving none never consults it.
        let host = Host::mock();
        let exec = task(
            true,
            ExecutionResult {
                exit_code: Some(0),
                status: "OK".into(),
                sandbox_status: SandboxStatus::Ok,
                stdout: "hello world\n".into(),
                ..Default::default()
            },
        );
        let result = op(vec![("compile", compile_ok()), ("exec", exec)]);
        let v = interpret_fused_result(&host.checker, 7, &result, "none", "check").unwrap();
        assert_eq!(v.verdict, Verdict::Accepted);
        assert_eq!(v.score, 1.0);
        assert_eq!(v.test_case_id, 7);
        // Output retained for display (the custom "run code" path relies on this).
        assert_eq!(v.stdout.as_deref(), Some("hello world\n"));
    }

    #[test]
    fn fused_none_does_not_mask_a_run_failure() {
        // `none` must not turn a TLE/RE into Accepted - precheck still wins.
        let host = Host::mock();
        let exec = task(
            false,
            ExecutionResult {
                exit_code: None,
                status: "TO".into(),
                sandbox_status: SandboxStatus::TimedOut,
                ..Default::default()
            },
        );
        let result = op(vec![("compile", compile_ok()), ("exec", exec)]);
        let v = interpret_fused_result(&host.checker, 1, &result, "none", "check").unwrap();
        assert_eq!(v.verdict, Verdict::TimeLimitExceeded);
    }
}
