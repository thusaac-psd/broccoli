//! How a sandbox step ended, classified once for every evaluator.
//!
//! An evaluator turns each of its steps into a verdict through
//! [`CompileOutcome`] or [`RunOutcome`], so whose fault an ending is (the
//! submission's, which stands, or the platform's, which is judged again)
//! follows one rule whatever the evaluator.

use crate::types::{ExecutionResult, SandboxStatus, TaskExecutionResult};

/// How a compile step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompileOutcome {
    /// It compiled, or its outputs came from the cache.
    Compiled,
    /// The compiler rejected the source and said why. The submission is at
    /// fault: a compile error.
    Rejected { diagnostics: String },
    /// The compile did not get to judge the source: the sandbox failed, the
    /// compiler was killed or timed out, it could not start (its output says
    /// it could not create a thread or fork), or it exited non-zero without a
    /// word. The platform is at fault and judges it again.
    Fault { reason: String },
}

impl CompileOutcome {
    /// Classify a compile step's result.
    pub fn of(step: &TaskExecutionResult) -> Self {
        Self::classify(step.success, &step.sandbox_result)
    }

    /// Classify a compile that ended as `sandbox`, `success` saying whether
    /// the worker counted the step as passed.
    pub fn classify(success: bool, sandbox: &ExecutionResult) -> Self {
        // A clobbered box can make the compiler exit non-zero with its
        // diagnostics file gone; the sandbox tags that, and it decides first.
        if sandbox.sandbox_status == SandboxStatus::InternalError {
            return Self::Fault {
                reason: nonempty(&sandbox.message)
                    .unwrap_or("the compilation sandbox reported an internal error")
                    .to_string(),
            };
        }
        match sandbox.exit_code {
            Some(0) => Self::Compiled,
            Some(code) => {
                // A compiler that rejects source always says why; one that
                // ran into the machine says so in words only a failure to
                // run produces.
                match nonempty(&sandbox.stderr).or_else(|| nonempty(&sandbox.stdout)) {
                    None => Self::Fault {
                        reason: format!(
                            "compilation exited with status {code} without diagnostics"
                        ),
                    },
                    Some(output) if compile_output_is_infra_fault(output) => Self::Fault {
                        reason: format!("compilation could not run: {output}"),
                    },
                    Some(output) => Self::Rejected {
                        diagnostics: output.to_string(),
                    },
                }
            }
            // No exit code: a cache hit, or a compile killed before it exited.
            None if success => Self::Compiled,
            None => Self::Fault {
                reason: match nonempty(&sandbox.stderr) {
                    Some(stderr) => format!("compilation did not finish: {stderr}"),
                    None => format!(
                        "compilation did not finish (sandbox status {})",
                        sandbox.status
                    ),
                },
            },
        }
    }
}

/// How a run of a program ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// It ran and exited 0.
    Exited,
    /// It never ran: a step it depends on failed.
    NotRun,
    /// The sandbox failed around it, so what it did is unknown. The
    /// platform is at fault and judges it again.
    SandboxFault,
    /// The kernel killed it for exceeding its memory limit.
    MemoryLimitExceeded,
    /// It exceeded its CPU or wall-time limit.
    TimedOut,
    /// A signal terminated it.
    Signaled(Option<i32>),
    /// It exited with this non-zero code.
    NonZeroExit(i32),
}

impl RunOutcome {
    /// Classify a run step's result.
    pub fn of(step: &TaskExecutionResult) -> Self {
        let sandbox = &step.sandbox_result;
        match sandbox.sandbox_status {
            SandboxStatus::InternalError => Self::SandboxFault,
            _ if sandbox.cg_oom_killed => Self::MemoryLimitExceeded,
            SandboxStatus::TimedOut => Self::TimedOut,
            SandboxStatus::Signaled => Self::Signaled(sandbox.signal),
            SandboxStatus::NonZeroExit => Self::NonZeroExit(sandbox.exit_code.unwrap_or(-1)),
            SandboxStatus::Ok => Self::Exited,
            SandboxStatus::Unknown if step.success => Self::Exited,
            SandboxStatus::Unknown => Self::NotRun,
        }
    }
}

fn nonempty(s: &str) -> Option<&str> {
    (!s.trim().is_empty()).then_some(s)
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
fn compile_output_is_infra_fault(output: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn step(success: bool, sandbox: ExecutionResult) -> TaskExecutionResult {
        TaskExecutionResult {
            task_id: "step".into(),
            success,
            sandbox_result: sandbox,
            collected_outputs: Default::default(),
        }
    }

    fn exited(code: i32, stderr: &str) -> ExecutionResult {
        ExecutionResult {
            exit_code: Some(code),
            status: if code == 0 { "OK" } else { "RE" }.into(),
            sandbox_status: if code == 0 {
                SandboxStatus::Ok
            } else {
                SandboxStatus::NonZeroExit
            },
            stderr: stderr.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_compiler_that_names_the_error_rejects_the_source() {
        let outcome =
            CompileOutcome::of(&step(false, exited(1, "main.cpp:1: error: expected ';'")));
        assert_eq!(
            outcome,
            CompileOutcome::Rejected {
                diagnostics: "main.cpp:1: error: expected ';'".into()
            }
        );
        let on_stdout = ExecutionResult {
            stdout: "Main.java:1: error".into(),
            ..exited(1, "")
        };
        assert!(matches!(
            CompileOutcome::of(&step(false, on_stdout)),
            CompileOutcome::Rejected { .. }
        ));
    }

    #[test]
    fn a_compile_the_platform_broke_is_a_fault() {
        let silent = exited(1, "");
        let jvm = exited(
            1,
            "Error occurred during initialization of VM\njava.lang.OutOfMemoryError: unable to create native thread",
        );
        let fork = exited(
            1,
            "g++: fatal error: cannot execute 'cc1': Resource temporarily unavailable",
        );
        let internal = ExecutionResult {
            sandbox_status: SandboxStatus::InternalError,
            ..exited(1, "error: x.cpp vanished")
        };
        let killed = ExecutionResult {
            exit_code: None,
            status: "TO".into(),
            sandbox_status: SandboxStatus::TimedOut,
            ..Default::default()
        };
        for sandbox in [silent, jvm, fork, internal, killed] {
            assert!(
                matches!(
                    CompileOutcome::of(&step(false, sandbox.clone())),
                    CompileOutcome::Fault { .. }
                ),
                "{sandbox:?}"
            );
        }
    }

    #[test]
    fn a_cached_compile_compiled() {
        assert_eq!(
            CompileOutcome::of(&step(true, ExecutionResult::default())),
            CompileOutcome::Compiled
        );
        assert_eq!(
            CompileOutcome::of(&step(true, exited(0, "warning: x"))),
            CompileOutcome::Compiled
        );
    }

    #[test]
    fn real_diagnostics_never_read_as_a_platform_fault() {
        for diagnostics in [
            "main.cpp:3:5: error: 'foo' was not declared in this scope",
            "Main.java:1: error: class, interface, enum, or record expected",
            "  File \"main.py\", line 1\nSyntaxError: invalid syntax",
            "main.cpp: In function 'int main()': error: cannot convert 'std::string'",
        ] {
            assert!(!compile_output_is_infra_fault(diagnostics), "{diagnostics}");
        }
    }

    #[test]
    fn a_run_is_classified_by_its_normalized_status() {
        let signaled = ExecutionResult {
            exit_code: None,
            signal: Some(11),
            status: "SG".into(),
            sandbox_status: SandboxStatus::Signaled,
            ..Default::default()
        };
        let oom = ExecutionResult {
            cg_oom_killed: true,
            ..signaled.clone()
        };
        let internal = ExecutionResult {
            sandbox_status: SandboxStatus::InternalError,
            ..exited(0, "")
        };
        let timed_out = ExecutionResult {
            status: "TO".into(),
            sandbox_status: SandboxStatus::TimedOut,
            ..Default::default()
        };
        assert_eq!(
            RunOutcome::of(&step(true, exited(0, ""))),
            RunOutcome::Exited
        );
        assert_eq!(
            RunOutcome::of(&step(false, exited(3, ""))),
            RunOutcome::NonZeroExit(3)
        );
        assert_eq!(
            RunOutcome::of(&step(false, signaled)),
            RunOutcome::Signaled(Some(11))
        );
        assert_eq!(
            RunOutcome::of(&step(false, oom)),
            RunOutcome::MemoryLimitExceeded
        );
        assert_eq!(
            RunOutcome::of(&step(false, timed_out)),
            RunOutcome::TimedOut
        );
        assert_eq!(
            RunOutcome::of(&step(false, internal)),
            RunOutcome::SandboxFault
        );
        assert_eq!(
            RunOutcome::of(&step(false, ExecutionResult::default())),
            RunOutcome::NotRun
        );
    }
}
