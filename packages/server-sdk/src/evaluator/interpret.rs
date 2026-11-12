use crate::Checker;
use crate::error::SdkError;
use crate::types::*;

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
        .get("exec")
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
        });
    }

    // At this point precheck confirmed a clean solution run, so the check step
    // MUST have produced a result. Treat a missing check step -- or one that is
    // present but never actually ran (not successful and with no sandbox status,
    // e.g. the platform comparator tool failed to launch) -- as an infrastructure
    // failure, never a contestant verdict.
    let check_result = match result.task_results.get(check_step_id) {
        Some(c) if c.success || !c.sandbox_result.status.is_empty() => c,
        _ => {
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
            });
        }
    };

    let sandbox = &check_result.sandbox_result;
    // A checker step whose own sandbox failed internally (isolate `XX`, or a
    // redirect file lost after a clean exit under load) is an infrastructure
    // fault, not a contestant verdict: surface it as a retryable SystemError
    // rather than feeding the missing comparator output to the checker.
    if sandbox.sandbox_status == SandboxStatus::InternalError {
        return Ok(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms,
            memory_used_kb,
            message: Some(
                "checker step hit a sandbox internal error (infrastructure failure)".into(),
            ),
            stdout: exec_stdout,
            stderr: exec_stderr,
        });
    }
    // Both checkers write their verdict message to stderr (broccoli-compare's
    // stdout carries the display preview, testlib's stderr carries quitf output).
    let message = sandbox.stderr.clone();
    // Display preview: the run output preview (exec stdout, File mode) or the
    // comparator's stdout preview (Stream mode, where exec stdout is a FIFO and
    // thus empty). 64 KiB cap applied worker-side.
    let display_stdout = exec_stdout.or_else(|| opt_nonempty(&sandbox.stdout));
    let outcome = CheckerRunOutcome {
        exit_code: sandbox.exit_code,
        stderr: message,
        sandbox_status: Some(sandbox.status.clone()),
    };

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
        }),
    }
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
        });
    }

    if let Some(compile_result) = result.task_results.get("compile") {
        // A clobbered box can make the compiler exit non-zero (e.g. its source
        // file vanished mid-run) with the declared stderr redirect also missing;
        // isolate tags that as InternalError. Check it BEFORE the exit-code
        // branch so an infrastructure fault becomes a retryable SystemError, not
        // a terminal CompileError that never self-heals. Mirrors the exec arm.
        if compile_result.sandbox_result.sandbox_status == SandboxStatus::InternalError {
            return Some(TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: None,
                memory_used_kb: None,
                message: Some(
                    opt_nonempty(&compile_result.sandbox_result.message)
                        .unwrap_or_else(|| "Compilation sandbox reported an internal error".into()),
                ),
                stdout: None,
                stderr: None,
            });
        }
        if let Some(exit_code) = compile_result.sandbox_result.exit_code {
            if exit_code != 0 {
                // A genuine compile error always carries the compiler's
                // diagnostics -- gcc, g++, javac and py_compile all report the
                // source problem on stderr (a few tools use stdout). A non-zero
                // exit is an infrastructure fault, NOT a diagnosable source error,
                // in two shapes, both of which must become a retryable SystemError
                // rather than a terminal CompileError that pins a permanent wrong
                // verdict on valid code under load:
                //   (a) NO output on either stream -- the compiler died before it
                //       could say anything;
                //   (b) output that is the signature of the toolchain failing to
                //       run at all (JVM thread-init EAGAIN, cc1 fork failure,
                //       native OOM) rather than a source diagnostic -- observed:
                //       a Java compile that cannot spawn its VM threads under
                //       transient per-uid RLIMIT_NPROC pressure prints an
                //       "unable to create native thread" / "Error occurred during
                //       initialization of VM" block and exits 1. See
                //       `compile_output_is_infra_fault`.
                // The InternalError guard above covers the clobbered-box case
                // (missing redirect files); this covers the completed non-zero
                // exit. (Compare the exec arm, which by contrast treats empty
                // present output as a legitimate WrongAnswer -- a program may print
                // nothing, but a compiler that actually ran never stays silent
                // about why it rejected source.)
                let diagnostics = opt_nonempty(&compile_result.sandbox_result.stderr)
                    .or_else(|| opt_nonempty(&compile_result.sandbox_result.stdout));
                let Some(diagnostics) = diagnostics else {
                    return Some(TestCaseVerdict {
                        test_case_id,
                        verdict: Verdict::SystemError,
                        score: 0.0,
                        time_used_ms: None,
                        memory_used_kb: None,
                        message: Some(format!(
                            "Compilation exited with status {exit_code} but produced no diagnostics (transient infrastructure fault)"
                        )),
                        stdout: None,
                        stderr: None,
                    });
                };
                if compile_output_is_infra_fault(&diagnostics) {
                    return Some(TestCaseVerdict {
                        test_case_id,
                        verdict: Verdict::SystemError,
                        score: 0.0,
                        time_used_ms: None,
                        memory_used_kb: None,
                        message: truncate_stderr(
                            &diagnostics,
                            "Compilation could not run (transient infrastructure fault)",
                        ),
                        stdout: None,
                        stderr: None,
                    });
                }
                return Some(TestCaseVerdict {
                    test_case_id,
                    verdict: Verdict::CompileError,
                    score: 0.0,
                    time_used_ms: None,
                    memory_used_kb: None,
                    message: truncate_stderr(&diagnostics, "Compilation failed"),
                    stdout: None,
                    stderr: None,
                });
            }
        } else if !compile_result.success {
            return Some(TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: None,
                memory_used_kb: None,
                message: truncate_stderr(
                    &compile_result.sandbox_result.stderr,
                    "Compilation step failed (sandbox error)",
                ),
                stdout: None,
                stderr: None,
            });
        }
    }

    if let Some(exec_result) = result.task_results.get("exec") {
        let sandbox = &exec_result.sandbox_result;
        let exec_stdout = opt_nonempty(&sandbox.stdout);
        let exec_stderr = opt_nonempty(&sandbox.stderr);

        if !exec_result.success && sandbox.status.is_empty() {
            return Some(TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: None,
                memory_used_kb: None,
                message: Some("Execution step was skipped".into()),
                stdout: None,
                stderr: None,
            });
        }

        if sandbox.cg_oom_killed {
            return Some(TestCaseVerdict {
                test_case_id,
                verdict: Verdict::MemoryLimitExceeded,
                score: 0.0,
                time_used_ms: extract_time_used(result),
                memory_used_kb: extract_memory_used(result),
                message: Some(format!(
                    "Memory limit exceeded ({}KB)",
                    sandbox.memory_used.unwrap_or(0)
                )),
                stdout: exec_stdout,
                stderr: exec_stderr,
            });
        }

        // Classify the sandbox termination once, via the normalized status.
        match sandbox.sandbox_status {
            SandboxStatus::TimedOut => {
                let time_ms = extract_time_used(result);
                return Some(TestCaseVerdict {
                    test_case_id,
                    verdict: Verdict::TimeLimitExceeded,
                    score: 0.0,
                    time_used_ms: time_ms,
                    memory_used_kb: extract_memory_used(result),
                    message: Some(format!(
                        "Time limit exceeded ({}ms)",
                        time_ms.map_or("?".into(), |t| t.to_string())
                    )),
                    stdout: exec_stdout,
                    stderr: exec_stderr,
                });
            }
            SandboxStatus::Signaled => {
                return Some(TestCaseVerdict {
                    test_case_id,
                    verdict: Verdict::RuntimeError,
                    score: 0.0,
                    time_used_ms: extract_time_used(result),
                    memory_used_kb: extract_memory_used(result),
                    message: Some(sandbox.message.clone()),
                    stdout: exec_stdout,
                    stderr: exec_stderr,
                });
            }
            SandboxStatus::NonZeroExit => {
                return Some(TestCaseVerdict {
                    test_case_id,
                    verdict: Verdict::RuntimeError,
                    score: 0.0,
                    time_used_ms: extract_time_used(result),
                    memory_used_kb: extract_memory_used(result),
                    message: Some(format!("Exit code: {}", sandbox.exit_code.unwrap_or(-1))),
                    stdout: exec_stdout,
                    stderr: exec_stderr,
                });
            }
            SandboxStatus::InternalError => {
                // The sandbox itself failed (isolate `XX`, or a redirect file that
                // vanished after a clean exit under concurrent load). This is a
                // system condition, never the contestant's code, so surface it as
                // a retryable SystemError instead of scoring the (lost) output as
                // a terminal WrongAnswer/TLE that never self-heals.
                return Some(TestCaseVerdict {
                    test_case_id,
                    verdict: Verdict::SystemError,
                    score: 0.0,
                    time_used_ms: extract_time_used(result),
                    memory_used_kb: extract_memory_used(result),
                    message: Some(
                        opt_nonempty(&sandbox.message).unwrap_or_else(|| {
                            "Execution sandbox reported an internal error".into()
                        }),
                    ),
                    stdout: exec_stdout,
                    stderr: exec_stderr,
                });
            }
            SandboxStatus::Ok | SandboxStatus::Unknown => {}
        }
    }

    // A clean exec result is required to consult the checker; its absence is a
    // system error.
    match result.task_results.get("exec") {
        Some(_) => None,
        None => Some(TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: Some("No exec result found".into()),
            stdout: None,
            stderr: None,
        }),
    }
}

fn extract_time_used(result: &OperationResult) -> Option<i64> {
    result.task_results.get("exec").and_then(|exec_result| {
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
        .get("exec")
        .and_then(|exec_result| exec_result.sandbox_result.memory_used.map(|m| m as i64))
}

fn opt_nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Whether a **failed compile step's** captured output is the signature of an
/// infrastructure fault rather than a genuine, diagnosable source error.
///
/// A real source rejection from any supported compiler (gcc/g++/javac/py_compile)
/// names the offending construct: `error: expected ';'`, a `SyntaxError`
/// traceback, and so on. It never reports that the toolchain itself could not
/// start. When a compile step fails under host pressure the driver instead emits
/// an OS/runtime fault. Observed under a Java burst: the `javac` JVM cannot spawn
/// its helper threads and prints
///
/// ```text
/// [0.028s][warning][os,thread] Failed to start thread "Unknown thread" -
///     pthread_create failed (EAGAIN) for attributes: stacksize: 1024k ...
/// Error occurred during initialization of VM
/// java.lang.OutOfMemoryError: unable to create native thread: possibly out of
///     memory or process/resource limits reached
/// ```
///
/// then exits non-zero -- indistinguishable from a source `CompileError` by exit
/// code alone, but it is transient infrastructure (per-uid `RLIMIT_NPROC` /
/// thread exhaustion), not the contestant's fault. The C/C++ drivers show the
/// same class of fault when they cannot fork their backend (`cannot execute
/// 'cc1'`, `Resource temporarily unavailable`, `Cannot allocate memory`).
///
/// Classifying these as a terminal `CompileError` pins a permanent wrong verdict
/// on valid code; classifying them as a retryable `SystemError` lets the server's
/// SystemError-retry reaper requeue the judgement so it self-heals once pressure
/// clears. Every marker below only appears when the *compiler process itself*
/// failed to run -- none is producible by a compiler diagnosing submitted source
/// -- so this is safe against demoting a real `CompileError` to infra on the
/// compile path. Match is case-insensitive.
pub fn compile_output_is_infra_fault(output: &str) -> bool {
    const MARKERS: &[&str] = &[
        // A JVM (javac is itself a Java program) that cannot create its native
        // threads or otherwise initialize under thread/memory pressure.
        "unable to create native thread",
        "failed to start thread",
        "failed to start the native thread",
        "pthread_create failed",
        "error occurred during initialization of vm",
        "cannot create gc thread",
        "insufficient memory for the java runtime environment",
        // Native allocation / fork-exec failures shared by the C/C++ drivers and
        // the interpreters (strerror(ENOMEM)/strerror(EAGAIN) text, driver fork).
        "cannot allocate memory",
        "resource temporarily unavailable",
        "cannot fork",
        "out of system resources",
    ];
    let haystack = output.to_ascii_lowercase();
    MARKERS.iter().any(|marker| haystack.contains(marker))
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
    fn compile_output_infra_signature_does_not_match_real_diagnostics() {
        // Guard the reverse direction: a genuine source diagnostic must NOT trip
        // the infra signature, or real compile errors would loop forever.
        assert!(!compile_output_is_infra_fault(
            "error: expected ';' before '}' token"
        ));
        assert!(!compile_output_is_infra_fault(
            "Main.java:3: error: cannot find symbol\n  System.out.printn(x);"
        ));
        assert!(!compile_output_is_infra_fault(
            "  File \"sol.py\", line 2\n    print(\nSyntaxError: unexpected EOF while parsing"
        ));
        assert!(compile_output_is_infra_fault(
            "Error occurred during initialization of VM"
        ));
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
