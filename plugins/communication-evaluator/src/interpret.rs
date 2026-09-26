use broccoli_server_sdk::evaluator::compile_output_is_infra_fault;
use broccoli_server_sdk::types::{OperationResult, SandboxStatus, TestCaseVerdict, Verdict};

/// POSIX signal number for SIGPIPE. A contestant terminated by SIGPIPE wrote to
/// a pipe whose reader (the interactor/manager) had already closed it - i.e. the
/// interactor finished the interaction first. That is not a contestant fault, so
/// the verdict defers to the interactor instead of reporting RuntimeError.
const SIGPIPE: i32 = 13;

/// Interpret the communication operation result into a TestCaseVerdict.
pub fn interpret_result(
    test_case_id: i32,
    result: &OperationResult,
    num_processes: usize,
    req_memory_limit_kb: u32,
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
        };
    }

    if let Some(compile_mgr) = result.task_results.get("compile_manager")
        && !compile_mgr.success
    {
        return TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms: None,
            memory_used_kb: None,
            message: Some(truncate(
                &compile_mgr.sandbox_result.stderr,
                "Manager compilation failed",
            )),
            stdout: None,
            stderr: None,
        };
    }

    for i in 0..num_processes {
        let step_id = format!("compile_contestant_{i}");
        if let Some(compile_c) = result.task_results.get(&step_id)
            && !compile_c.success
        {
            // Classify a failed contestant compile with the SAME precedence as
            // broccoli_server_sdk::evaluator precheck_verdict's compile arm, so
            // interactive and batch problems agree on infra-vs-source faults:
            //   (1) an isolate-tagged internal error (clobbered box, vanished
            //       redirect file) -> SystemError,
            //   (2) a non-zero exit WITH diagnostics -> genuine, terminal
            //       CompileError (gcc/g++/javac/py_compile always say why they
            //       reject source),
            //   (3) a non-zero exit with NO diagnostics on either stream -> a
            //       transient EAGAIN during JVM thread-init under per-uid
            //       RLIMIT_NPROC pressure makes javac exit non-zero writing
            //       nothing; infra, not source -> retryable SystemError,
            //   (4) a compile aborted before it could exit (signal / timeout /
            //       OOM, exit_code None) -> SystemError.
            // Only (2) is terminal; the rest self-heal via the server's
            // SystemError-retry reaper instead of pinning a permanent wrong
            // CompileError on valid code under burst.
            let sandbox = &compile_c.sandbox_result;
            if sandbox.status_kind() == SandboxStatus::InternalError {
                return TestCaseVerdict {
                    test_case_id,
                    verdict: Verdict::SystemError,
                    score: 0.0,
                    time_used_ms: None,
                    memory_used_kb: None,
                    message: Some(opt_nonempty(&sandbox.message).unwrap_or_else(|| {
                        format!("Contestant {i} compilation sandbox reported an internal error")
                    })),
                    stdout: None,
                    stderr: None,
                };
            }
            match sandbox.exit_code {
                // Compiled cleanly despite the success flag; keep checking.
                Some(0) => {}
                Some(_) => {
                    return match opt_nonempty(&sandbox.stderr)
                        .or_else(|| opt_nonempty(&sandbox.stdout))
                    {
                        // Output that is the signature of the toolchain failing
                        // to run (JVM thread-init EAGAIN, cc1 fork failure,
                        // native OOM) is infra, not a source diagnostic -> the
                        // same retryable SystemError as the no-output case.
                        // Single-sourced with server-sdk so the two classifiers
                        // cannot drift on which strings count as infra.
                        Some(diagnostics) if compile_output_is_infra_fault(&diagnostics) => {
                            TestCaseVerdict {
                                test_case_id,
                                verdict: Verdict::SystemError,
                                score: 0.0,
                                time_used_ms: None,
                                memory_used_kb: None,
                                message: Some(truncate(
                                    &diagnostics,
                                    "Contestant compilation could not run (transient infrastructure fault)",
                                )),
                                stdout: None,
                                stderr: None,
                            }
                        }
                        Some(diagnostics) => TestCaseVerdict {
                            test_case_id,
                            verdict: Verdict::CompileError,
                            score: 0.0,
                            time_used_ms: None,
                            memory_used_kb: None,
                            message: Some(truncate(&diagnostics, "Compilation failed")),
                            stdout: None,
                            stderr: None,
                        },
                        None => TestCaseVerdict {
                            test_case_id,
                            verdict: Verdict::SystemError,
                            score: 0.0,
                            time_used_ms: None,
                            memory_used_kb: None,
                            message: Some(format!(
                                "Contestant {i} compilation failed with no diagnostics (transient infrastructure fault)"
                            )),
                            stdout: None,
                            stderr: None,
                        },
                    };
                }
                None => {
                    return TestCaseVerdict {
                        test_case_id,
                        verdict: Verdict::SystemError,
                        score: 0.0,
                        time_used_ms: None,
                        memory_used_kb: None,
                        message: Some(truncate(
                            &sandbox.stderr,
                            "Compilation step failed (sandbox error)",
                        )),
                        stdout: None,
                        stderr: None,
                    };
                }
            }
        }
    }

    for i in 0..num_processes {
        let step_id = format!("run_contestant_{i}");
        if !result.task_results.contains_key(&step_id) {
            return TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: None,
                memory_used_kb: None,
                message: Some(format!(
                    "Missing expected step '{step_id}' in operation result"
                )),
                stdout: None,
                stderr: None,
            };
        }
    }

    let mut total_time_s: f64 = 0.0;
    let mut max_memory_kb: Option<u32> = None;

    for i in 0..num_processes {
        let step_id = format!("run_contestant_{i}");
        if let Some(run_c) = result.task_results.get(&step_id) {
            let sandbox = &run_c.sandbox_result;

            total_time_s += sandbox.time_used;
            if let Some(mem) = sandbox.memory_used {
                max_memory_kb = Some(max_memory_kb.map_or(mem, |m: u32| m.max(mem)));
            }

            if !run_c.success || sandbox.exit_code != Some(0) {
                let mem_exceeded = sandbox
                    .memory_used
                    .is_some_and(|m| req_memory_limit_kb > 0 && m >= req_memory_limit_kb);
                if sandbox.cg_oom_killed || (sandbox.killed && mem_exceeded) {
                    return TestCaseVerdict {
                        test_case_id,
                        verdict: Verdict::MemoryLimitExceeded,
                        score: 0.0,
                        time_used_ms: time_to_ms(total_time_s),
                        memory_used_kb: max_memory_kb.map(|m| m as i64),
                        message: Some(format!(
                            "Memory limit exceeded (contestant {i}, {}KB)",
                            sandbox.memory_used.unwrap_or(0)
                        )),
                        stdout: None,
                        stderr: opt_nonempty(&sandbox.stderr),
                    };
                }

                match sandbox.status.as_str() {
                    "TO" => {
                        return TestCaseVerdict {
                            test_case_id,
                            verdict: Verdict::TimeLimitExceeded,
                            score: 0.0,
                            time_used_ms: time_to_ms(total_time_s),
                            memory_used_kb: max_memory_kb.map(|m| m as i64),
                            message: Some(format!("Time limit exceeded (contestant {i})")),
                            stdout: None,
                            stderr: opt_nonempty(&sandbox.stderr),
                        };
                    }
                    "SG" => {
                        // SIGPIPE (13) means the contestant wrote after the interactor had
                        // already closed its end of the pipe - i.e. the interactor finished the
                        // interaction (it decided the verdict, then exited and closed the FIFO)
                        // and is the authority. Do NOT penalize the solution for that incidental
                        // signal; fall through to the manager's verdict below. Any other signal
                        // (SIGSEGV, SIGABRT, ...) is a genuine crash -> RuntimeError.
                        if sandbox.signal != Some(SIGPIPE) {
                            return TestCaseVerdict {
                                test_case_id,
                                verdict: Verdict::RuntimeError,
                                score: 0.0,
                                time_used_ms: time_to_ms(total_time_s),
                                memory_used_kb: max_memory_kb.map(|m| m as i64),
                                message: Some(format!(
                                    "Signal received (contestant {i}): {}",
                                    sandbox.message
                                )),
                                stdout: None,
                                stderr: opt_nonempty(&sandbox.stderr),
                            };
                        }
                        // SIGPIPE: interactor closed the pipe; defer to the manager's verdict.
                    }
                    _ => {
                        // RE or unknown failure
                        return TestCaseVerdict {
                            test_case_id,
                            verdict: Verdict::RuntimeError,
                            score: 0.0,
                            time_used_ms: time_to_ms(total_time_s),
                            memory_used_kb: max_memory_kb.map(|m| m as i64),
                            message: Some(format!(
                                "Runtime error (contestant {i}, exit code: {})",
                                sandbox.exit_code.unwrap_or(-1)
                            )),
                            stdout: None,
                            stderr: opt_nonempty(&sandbox.stderr),
                        };
                    }
                }
            }
        } else {
            return TestCaseVerdict {
                test_case_id,
                verdict: Verdict::SystemError,
                score: 0.0,
                time_used_ms: None,
                memory_used_kb: None,
                message: Some(format!("Contestant {i} run step was skipped")),
                stdout: None,
                stderr: None,
            };
        }
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
            };
        }
    };

    let mgr_sandbox = &run_mgr.sandbox_result;

    if mgr_sandbox.exit_code != Some(0) {
        return TestCaseVerdict {
            test_case_id,
            verdict: Verdict::SystemError,
            score: 0.0,
            time_used_ms: time_to_ms(total_time_s),
            memory_used_kb: max_memory_kb.map(|m| m as i64),
            message: Some(format!(
                "Manager exited with code {} — {}",
                mgr_sandbox.exit_code.unwrap_or(-1),
                truncate(&mgr_sandbox.stderr, "manager error")
            )),
            stdout: None,
            stderr: opt_nonempty(&mgr_sandbox.stderr),
        };
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
    use broccoli_server_sdk::types::{ExecutionResult, TaskExecutionResult};
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
            stdout: score.to_string(),
            stderr: message.to_string(),
            ..Default::default()
        }
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer);
        assert_eq!(verdict.score, 0.0);
    }

    #[test]
    fn contestant_tle() {
        let mut tle_sandbox = ok_sandbox(0, 2.0, 4096);
        tle_sandbox.killed = true;
        tle_sandbox.status = "TO".to_string();
        tle_sandbox.exit_code = None;

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0", "")),
                task_result("run_contestant_0", false, tle_sandbox),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::TimeLimitExceeded);
    }

    #[test]
    fn contestant_mle_via_memory_exceeded_without_oom_kill() {
        // Killed + memory >= limit but cg_oom_killed is false
        let mut mle_sandbox = ok_sandbox(0, 0.5, MEM_LIMIT);
        mle_sandbox.killed = true;
        mle_sandbox.exit_code = None;
        mle_sandbox.status = "SG".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", true, mgr_result("0.0", "")),
                task_result("run_contestant_0", false, mle_sandbox),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::MemoryLimitExceeded);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::RuntimeError);
    }

    fn signal_sandbox(signal: i32) -> ExecutionResult {
        ExecutionResult {
            exit_code: None,
            signal: Some(signal),
            status: "SG".to_string(),
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::CompileError);
        assert!(verdict.message.unwrap().contains("expected ';'"));
    }

    #[test]
    fn contestant_compile_nonzero_without_diagnostics_is_system_error() {
        // A contestant Java compile that fails to spawn its VM threads under
        // transient per-uid RLIMIT_NPROC pressure exits non-zero writing nothing.
        // A real compile error always carries diagnostics, so silence => retryable
        // infra fault, not a terminal CompileError pinned on valid code under
        // burst. Mirrors broccoli_server_sdk precheck_verdict's compile arm.
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::CompileError);
        assert!(verdict.message.unwrap().contains("cannot find symbol"));
    }

    #[test]
    fn contestant_compile_jvm_thread_eagain_is_system_error() {
        // A contestant Java compile that exits non-zero WITH a VM-init/thread
        // EAGAIN block (not a source diagnostic) is infra -> SystemError, matching
        // server-sdk via the shared compile_output_is_infra_fault signature.
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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
        aborted.stderr = "partial diagnostics before the kill".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, aborted),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn contestant_compile_internal_error_is_system_error() {
        // An isolate-tagged internal error (clobbered box, vanished redirect file)
        // outranks any exit_code and diagnostics -> SystemError. Precedence must
        // match server-sdk: InternalError is checked before exit_code.
        let mut internal = ok_sandbox(1, 1.0, 8192);
        internal.status = "XX".to_string();
        internal.stderr = "error: looks like a real diagnostic".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("compile_manager", true, ok_sandbox(0, 1.0, 8192)),
                task_result("compile_contestant_0", false, internal),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::SystemError);
    }

    #[test]
    fn manager_nonzero_exit_is_system_error() {
        let mut mgr = mgr_result("0.5", "internal error");
        mgr.exit_code = Some(1);
        mgr.status = "RE".to_string();

        let result = OperationResult {
            success: false,
            task_results: HashMap::from([
                task_result("run_manager", false, mgr),
                task_result("run_contestant_0", true, ok_sandbox(0, 0.3, 2048)),
            ]),
            error: None,
        };

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 2, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
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

        let verdict = interpret_result(42, &result, 1, MEM_LIMIT);
        assert_eq!(verdict.verdict, Verdict::WrongAnswer);
        assert_eq!(verdict.score, 0.0);
    }
}
