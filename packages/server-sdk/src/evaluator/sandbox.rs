//! The sandbox limits an evaluator runs a submission under.
//!
//! Every evaluator declares the same plugin-scope `sandbox` config namespace
//! and builds its compile and run steps from it here, so the limits of a step
//! follow one rule whichever evaluator judges: a language's own compile limits
//! win over the namespace's, and a run never gets fewer processes than its
//! language needs to start ([`RunSpec::min_process_limit`]).

use serde::Deserialize;

use crate::Host;
use crate::error::SdkError;
use crate::types::{
    CompileSpec, EvaluationTimeoutBudget, IOConfig, IOTarget, OutputSpec, ResourceLimits,
    RunOptions, RunSpec, Step, StepCacheConfig, StepKind, seconds_from_ms,
};

/// The file a compile step writes the compiler's diagnostics to. The worker
/// reads it back into the step's `stderr`.
pub const COMPILE_STDERR_FILE: &str = "compile_stderr.txt";

/// The plugin-scope `sandbox` config namespace. Its defaults and bounds live
/// in the schema of the evaluator's `plugin.toml`, which the host fills in, so
/// every field is present. Each evaluator that reads it declares every field.
#[derive(Debug, Clone, Deserialize)]
pub struct SandboxConfig {
    pub compile_time_limit_s: f64,
    pub compile_wall_time_multiplier: f64,
    pub compile_extra_time_s: f64,
    pub compile_memory_limit_kb: u32,
    /// 0 leaves the stack unlimited.
    pub compile_stack_limit_kb: u32,
    pub compile_process_limit: u32,
    pub compile_open_files_limit: u32,
    pub compile_file_size_limit_kb: u32,
    pub exec_extra_time_s: f64,
    /// 0 leaves the stack unlimited.
    pub exec_stack_limit_kb: u32,
    pub exec_process_limit: u32,
    pub exec_open_files_limit: u32,
    pub exec_file_size_limit_kb: u32,
    pub exec_wall_time_multiplier: f64,
    pub result_timeout_ms: u64,
}

impl SandboxConfig {
    /// The namespace's name in every evaluator's manifest.
    pub const NAMESPACE: &'static str = "sandbox";

    /// The deployment's sandbox limits.
    pub fn load(host: &Host) -> Result<Self, SdkError> {
        host.config.get_global(Self::NAMESPACE)?.parse()
    }

    /// The limits a compile of `compile` runs under: the language's own when
    /// it sets them, else the namespace's.
    pub fn compile_limits(&self, compile: &CompileSpec) -> ResourceLimits {
        compile
            .resource_limits
            .clone()
            .unwrap_or_else(|| ResourceLimits {
                time_limit: Some(self.compile_time_limit_s),
                wall_time_limit: Some(
                    self.compile_time_limit_s * self.compile_wall_time_multiplier,
                ),
                extra_time: positive(self.compile_extra_time_s),
                memory_limit: Some(self.compile_memory_limit_kb),
                stack_limit: nonzero(self.compile_stack_limit_kb),
                process_limit: Some(self.compile_process_limit),
                open_files_limit: Some(self.compile_open_files_limit),
                file_size_limit: Some(self.compile_file_size_limit_kb),
            })
    }

    /// The limits a run of `run` gets for a test with these time and memory
    /// limits. The process cap is the namespace's, raised to the floor the
    /// runtime needs to start (the JVM's helper threads, Go's scheduler);
    /// time, memory and stack stay the admin's.
    pub fn run_limits(
        &self,
        run: &RunSpec,
        time_limit_s: f64,
        memory_limit_kb: u32,
    ) -> ResourceLimits {
        ResourceLimits {
            time_limit: Some(time_limit_s),
            wall_time_limit: Some(time_limit_s * self.exec_wall_time_multiplier),
            extra_time: positive(self.exec_extra_time_s),
            memory_limit: Some(memory_limit_kb),
            stack_limit: nonzero(self.exec_stack_limit_kb),
            process_limit: Some(
                self.exec_process_limit
                    .max(run.min_process_limit.unwrap_or(0)),
            ),
            open_files_limit: Some(self.exec_open_files_limit),
            file_size_limit: Some(self.exec_file_size_limit_kb),
        }
    }

    /// The step `id` that compiles `compile` in environment `env_ref`: the
    /// compiler's diagnostics go to [`COMPILE_STDERR_FILE`], and its outputs
    /// are cached by the language's cache inputs.
    pub fn compile_step(
        &self,
        id: impl Into<String>,
        env_ref: impl Into<String>,
        compile: &CompileSpec,
    ) -> Step {
        let outputs: Vec<String> = compile
            .outputs
            .iter()
            .map(|o| match o {
                OutputSpec::File(f) | OutputSpec::Glob(f) => f.clone(),
            })
            .collect();
        let mut collect = outputs.clone();
        collect.push(COMPILE_STDERR_FILE.to_string());
        Step {
            id: id.into(),
            kind: StepKind::Compile,
            env_ref: env_ref.into(),
            argv: compile.command.clone(),
            conf: RunOptions {
                resource_limits: self.compile_limits(compile),
                wait: true,
                env_rules: vec![],
                ..Default::default()
            },
            io: IOConfig {
                stdin: IOTarget::Null,
                stdout: IOTarget::Null,
                stderr: IOTarget::File {
                    path: COMPILE_STDERR_FILE.to_string(),
                },
            },
            collect,
            depends_on: vec![],
            mounts: vec![],
            cache: Some(StepCacheConfig {
                key_inputs: compile.cache_inputs.clone(),
                outputs,
            }),
        }
    }

    /// How long a case may take before its result is given up on:
    /// `compile_units` compiles and one run under these limits, never less
    /// than the namespace's `result_timeout_ms`. An evaluator that runs more
    /// (an interactor) adds its share to the budget before taking its
    /// [`EvaluationTimeoutBudget::timeout_ms`].
    pub fn timeout_budget(
        &self,
        time_limit_ms: i32,
        compile_units: u32,
    ) -> EvaluationTimeoutBudget {
        let defaults = EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms);
        EvaluationTimeoutBudget {
            compile_units,
            compile_time_limit_s: self.compile_time_limit_s,
            compile_wall_time_multiplier: self.compile_wall_time_multiplier,
            compile_extra_time_s: self.compile_extra_time_s,
            exec_time_limit_s: seconds_from_ms(time_limit_ms),
            exec_wall_time_multiplier: self.exec_wall_time_multiplier,
            exec_extra_time_s: self.exec_extra_time_s,
            minimum_timeout_ms: self.result_timeout_ms.max(defaults.minimum_timeout_ms),
            maximum_timeout_ms: self.result_timeout_ms.max(defaults.maximum_timeout_ms),
            ..defaults
        }
    }
}

fn positive(value: f64) -> Option<f64> {
    (value > 0.0).then_some(value)
}

fn nonzero(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SandboxConfig {
        SandboxConfig {
            compile_time_limit_s: 30.0,
            compile_wall_time_multiplier: 2.0,
            compile_extra_time_s: 0.0,
            compile_memory_limit_kb: 524_288,
            compile_stack_limit_kb: 0,
            compile_process_limit: 64,
            compile_open_files_limit: 64,
            compile_file_size_limit_kb: 65_536,
            exec_extra_time_s: 0.0,
            exec_stack_limit_kb: 0,
            exec_process_limit: 1,
            exec_open_files_limit: 64,
            exec_file_size_limit_kb: 65_536,
            exec_wall_time_multiplier: 3.0,
            result_timeout_ms: 0,
        }
    }

    fn run(min_process_limit: Option<u32>) -> RunSpec {
        RunSpec {
            command: vec!["./solution".into()],
            extra_files: vec![],
            min_process_limit,
        }
    }

    fn compile(resource_limits: Option<ResourceLimits>) -> CompileSpec {
        CompileSpec {
            command: vec!["g++".into(), "main.cpp".into()],
            cache_inputs: vec!["main.cpp".into()],
            outputs: vec![OutputSpec::File("solution".into())],
            resource_limits,
        }
    }

    #[test]
    fn a_run_gets_the_process_floor_its_language_needs() {
        let limits = config().run_limits(&run(Some(64)), 1.0, 262_144);
        assert_eq!(limits.process_limit, Some(64));
    }

    #[test]
    fn a_process_cap_above_the_floor_stands() {
        let config = SandboxConfig {
            exec_process_limit: 128,
            ..config()
        };
        assert_eq!(
            config
                .run_limits(&run(Some(64)), 1.0, 262_144)
                .process_limit,
            Some(128)
        );
        assert_eq!(
            config.run_limits(&run(None), 1.0, 262_144).process_limit,
            Some(128)
        );
    }

    #[test]
    fn a_run_takes_the_test_limits_and_the_admins_stack_and_wall_time() {
        let config = SandboxConfig {
            exec_stack_limit_kb: 262_144,
            exec_extra_time_s: 0.5,
            ..config()
        };
        let limits = config.run_limits(&run(None), 2.0, 131_072);
        assert_eq!(limits.time_limit, Some(2.0));
        assert_eq!(limits.wall_time_limit, Some(6.0));
        assert_eq!(limits.extra_time, Some(0.5));
        assert_eq!(limits.memory_limit, Some(131_072));
        assert_eq!(limits.stack_limit, Some(262_144));
        assert_eq!(config.compile_limits(&compile(None)).stack_limit, None);
    }

    #[test]
    fn a_languages_own_compile_limits_win() {
        let own = ResourceLimits {
            time_limit: Some(99.0),
            ..Default::default()
        };
        assert_eq!(
            config().compile_limits(&compile(Some(own))).time_limit,
            Some(99.0)
        );
        let limits = config().compile_limits(&compile(None));
        assert_eq!(limits.time_limit, Some(30.0));
        assert_eq!(limits.wall_time_limit, Some(60.0));
        assert_eq!(limits.process_limit, Some(64));
    }

    #[test]
    fn a_compile_step_reports_diagnostics_and_caches_its_outputs() {
        let step = config().compile_step("compile", "sandbox", &compile(None));
        assert_eq!(step.kind, StepKind::Compile);
        assert_eq!(step.argv, ["g++", "main.cpp"]);
        assert_eq!(step.collect, ["solution", COMPILE_STDERR_FILE]);
        let cache = step.cache.expect("compiles are cached");
        assert_eq!(cache.key_inputs, ["main.cpp"]);
        assert_eq!(cache.outputs, ["solution"]);
    }

    #[test]
    fn the_result_timeout_never_falls_below_the_configured_floor() {
        let config = SandboxConfig {
            result_timeout_ms: 1_500_000,
            ..config()
        };
        assert_eq!(config.timeout_budget(1000, 1).timeout_ms(), 1_500_000);
    }
}
