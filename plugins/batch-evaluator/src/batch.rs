use broccoli_server_sdk::types::{
    BuildEvalOpsInput, Channel, CheckerStage, Environment, EvaluationTimeoutBudget, IOConfig,
    IOTarget, OperationTask, OutputMode, OutputSpec, ResolveLanguageOutput, ResourceLimits,
    RunOptions, SessionFile, Step, StepCacheConfig, StepKind, seconds_from_ms,
};
use serde::Deserialize;
use std::collections::HashSet;

/// Admin-configurable sandbox resource limits.
/// All fields have sensible defaults so zero-config deployments work unchanged.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub compile_time_limit_s: f64,
    pub compile_wall_time_multiplier: f64,
    pub compile_extra_time_s: f64,
    pub compile_memory_limit_kb: u32,
    pub compile_stack_limit_kb: u32,
    pub compile_process_limit: u32,
    pub compile_open_files_limit: u32,
    pub compile_file_size_limit_kb: u32,
    pub exec_extra_time_s: f64,
    pub exec_stack_limit_kb: u32,
    pub exec_process_limit: u32,
    pub exec_open_files_limit: u32,
    pub exec_file_size_limit_kb: u32,
    pub exec_wall_time_multiplier: f64,
    pub result_timeout_ms: u64,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            compile_time_limit_s: 30.0,
            compile_wall_time_multiplier: 2.0,
            compile_extra_time_s: 0.0,
            compile_memory_limit_kb: 524_288, // 512 MB
            compile_stack_limit_kb: 0,
            compile_process_limit: 32,
            compile_open_files_limit: 256,
            compile_file_size_limit_kb: 524_288, // 512 MB
            exec_extra_time_s: 0.0,
            exec_stack_limit_kb: 0,
            exec_process_limit: 1,
            exec_open_files_limit: 64,
            exec_file_size_limit_kb: 65_536, // 64 MB
            exec_wall_time_multiplier: 3.0,
            result_timeout_ms: EvaluationTimeoutBudget::default_for_time_limit_ms(0)
                .minimum_timeout_ms,
        }
    }
}

impl SandboxConfig {
    /// Build ResourceLimits for the compilation step.
    pub fn compile_limits(&self) -> ResourceLimits {
        ResourceLimits {
            time_limit: Some(self.compile_time_limit_s),
            wall_time_limit: Some(self.compile_time_limit_s * self.compile_wall_time_multiplier),
            extra_time: if self.compile_extra_time_s > 0.0 {
                Some(self.compile_extra_time_s)
            } else {
                None
            },
            memory_limit: Some(self.compile_memory_limit_kb),
            stack_limit: limit_if_positive(self.compile_stack_limit_kb),
            process_limit: Some(self.compile_process_limit),
            open_files_limit: Some(self.compile_open_files_limit),
            file_size_limit: Some(self.compile_file_size_limit_kb),
        }
    }

    /// Build ResourceLimits for the execution step.
    pub fn exec_limits(&self, time_limit_s: f64, memory_limit_kb: u32) -> ResourceLimits {
        ResourceLimits {
            time_limit: Some(time_limit_s),
            wall_time_limit: Some(time_limit_s * self.exec_wall_time_multiplier),
            extra_time: if self.exec_extra_time_s > 0.0 {
                Some(self.exec_extra_time_s)
            } else {
                None
            },
            memory_limit: Some(memory_limit_kb),
            stack_limit: limit_if_positive(self.exec_stack_limit_kb),
            process_limit: Some(self.exec_process_limit),
            open_files_limit: Some(self.exec_open_files_limit),
            file_size_limit: Some(self.exec_file_size_limit_kb),
        }
    }

    pub fn result_timeout_ms_for(&self, time_limit_ms: i32, compile_units: u32) -> u64 {
        EvaluationTimeoutBudget {
            compile_units,
            compile_time_limit_s: self.compile_time_limit_s,
            compile_wall_time_multiplier: self.compile_wall_time_multiplier,
            compile_extra_time_s: self.compile_extra_time_s,
            exec_time_limit_s: seconds_from_ms(time_limit_ms),
            exec_wall_time_multiplier: self.exec_wall_time_multiplier,
            exec_extra_time_s: self.exec_extra_time_s,
            minimum_timeout_ms: self.result_timeout_ms.max(
                EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms)
                    .minimum_timeout_ms,
            ),
            maximum_timeout_ms: self.result_timeout_ms.max(
                EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms)
                    .maximum_timeout_ms,
            ),
            ..EvaluationTimeoutBudget::default_for_time_limit_ms(time_limit_ms)
        }
        .timeout_ms()
    }
}

fn limit_if_positive(value: u32) -> Option<u32> {
    if value > 0 { Some(value) } else { None }
}

/// Build a sandbox OperationTask from enriched evaluator input.
///
/// Returns `Vec<OperationTask>` ready for `host.operations.windowed(...).collect(...)`.
pub fn build_operation(
    req: &BuildEvalOpsInput,
    lang: &ResolveLanguageOutput,
    config: &SandboxConfig,
    checker_stage: Option<&CheckerStage>,
) -> Result<Vec<OperationTask>, String> {
    if req.solution_source.is_empty() {
        return Err("No source file provided".into());
    }

    let mut files_in = Vec::new();
    let mut seen_filenames = HashSet::new();

    for af in &req.additional_file_refs {
        if seen_filenames.insert(af.filename.clone()) {
            files_in.push((
                af.filename.clone(),
                SessionFile::Blob {
                    hash: af.blob_hash.clone(),
                },
            ));
        }
    }

    for source in &req.solution_source {
        if seen_filenames.insert(source.filename.clone()) {
            files_in.push((
                source.filename.clone(),
                SessionFile::Content {
                    content: source.content.clone(),
                },
            ));
        }
    }

    files_in.push(("input.txt".to_string(), req.test_input.to_session_file()));

    let env = Environment {
        id: "sandbox".to_string(),
        files_in,
    };

    let time_limit_ms = u32::try_from(req.time_limit_ms)
        .map_err(|_| format!("Invalid time_limit_ms: {}", req.time_limit_ms))?;
    let time_limit_s = time_limit_ms as f64 / 1000.0;
    let memory_limit_kb = u32::try_from(req.memory_limit_kb)
        .map_err(|_| format!("Invalid memory_limit_kb: {}", req.memory_limit_kb))?;

    let mut steps = Vec::new();

    // Compile step (only for compiled languages)
    if let Some(compile) = &lang.compile {
        let cache_outputs: Vec<String> = compile
            .outputs
            .iter()
            .map(|o| match o {
                OutputSpec::File(f) => f.clone(),
                OutputSpec::Glob(g) => g.clone(),
            })
            .collect();

        let mut collect = cache_outputs.clone();
        collect.push("compile_stderr.txt".to_string());

        let compile_step = Step {
            id: "compile".to_string(),
            kind: StepKind::Compile,
            env_ref: "sandbox".to_string(),
            argv: compile.command.clone(),
            conf: RunOptions {
                resource_limits: compile
                    .resource_limits
                    .clone()
                    .unwrap_or_else(|| config.compile_limits()),
                wait: true,
                env_rules: vec![],
                ..Default::default()
            },
            io: IOConfig {
                stdin: IOTarget::Null,
                stdout: IOTarget::Null,
                stderr: IOTarget::File {
                    path: "compile_stderr.txt".to_string(),
                },
            },
            collect,
            depends_on: vec![],
            cache: Some(StepCacheConfig {
                key_inputs: compile.cache_inputs.clone(),
                outputs: cache_outputs,
            }),
            mounts: vec![],
        };
        steps.push(compile_step);
    }

    // Exec step
    let exec_depends = if lang.compile.is_some() {
        vec!["compile".to_string()]
    } else {
        vec![]
    };

    // Raise the exec process limit to the runtime's floor when it needs more than
    // the (tight, single-process) admin default - e.g. the JVM, which aborts at
    // init if it cannot spawn its GC/JIT/VM helper threads. Admin authority over
    // memory/time/stack is untouched; only the process cap moves, and only upward.
    let mut exec_limits = config.exec_limits(time_limit_s, memory_limit_kb);
    if let Some(floor) = lang.run.min_process_limit {
        let effective = exec_limits.process_limit.unwrap_or(0).max(floor);
        exec_limits.process_limit = Some(effective);
    }

    let exec_step = Step {
        id: "exec".to_string(),
        kind: StepKind::Testcase,
        env_ref: "sandbox".to_string(),
        argv: lang.run.command.clone(),
        conf: RunOptions {
            resource_limits: exec_limits,
            wait: true,
            env_rules: vec![],
            ..Default::default()
        },
        io: IOConfig {
            stdin: IOTarget::File {
                path: "input.txt".to_string(),
            },
            stdout: IOTarget::File {
                path: "output.txt".to_string(),
            },
            stderr: IOTarget::File {
                path: "stderr.txt".to_string(),
            },
        },
        collect: vec!["output.txt".to_string(), "stderr.txt".to_string()],
        depends_on: exec_depends,
        cache: None,
        mounts: vec![],
    };
    steps.push(exec_step);

    let mut environments = vec![env];
    let mut channels: Vec<Channel> = vec![];

    // Checker fusion: splice the resolved checker stage into this op so the
    // solution output is checked worker-side (never streamed to the coordinator).
    if let Some(stage) = checker_stage {
        splice_checker_stage(&mut steps, &mut environments, &mut channels, stage);
    }

    let op = OperationTask {
        environments,
        tasks: steps,
        channels,
        priority: None,
        target_worker_id: req.target_worker_id.clone(),
        evaluate_batch_id: None,
        test_case_id: None,
    };

    Ok(vec![op])
}

/// Extra result-wait budget (ms) a spliced checker stage adds: the sum of each
/// checker step's worst-case wall time. Conservative - added regardless of mode.
/// A Stream checker overlaps `exec`, so for it this only ever OVER-budgets (which
/// merely delays hung-op detection, never a spurious timeout); a File checker
/// runs sequentially after `exec`, where the budget is genuinely needed (e.g. a
/// cold testlib checker compile). Steps with no time limit contribute 0.
pub fn checker_stage_timeout_ms(stage: &CheckerStage) -> u64 {
    const WALL_MULTIPLIER: f64 = 3.0;
    stage
        .steps
        .iter()
        .map(|s| {
            let limits = &s.conf.resource_limits;
            let wall_s = limits
                .wall_time_limit
                .or_else(|| limits.time_limit.map(|t| t * WALL_MULTIPLIER))
                .unwrap_or(0.0);
            (wall_s * 1000.0).ceil().max(0.0) as u64
        })
        .sum()
}

/// Merge a resolved `CheckerStage` into the run op. Generic over checker
/// identity - wires solution-output handoff purely from the declared
/// `output_mode`:
/// - Stream: exec stdout -> FIFO; the check step (which already reads that pipe
///   on stdin) runs CONCURRENTLY with exec, so it depends on what exec depends
///   on (the compile step), not on exec itself.
/// - File: exec stdout -> a worker-local file the check step mounts; the check
///   step depends on `exec` (needs the completed file; also satisfies the
///   StepOutput mount's from_step ∈ depends_on requirement).
///
/// In both modes the full output is dropped from `exec.collect` (kept
/// worker-local / piped) so the coordinator never sees it.
fn splice_checker_stage(
    steps: &mut Vec<Step>,
    environments: &mut Vec<Environment>,
    channels: &mut Vec<Channel>,
    stage: &CheckerStage,
) {
    // The exec step is the last one pushed before the stage is spliced.
    let exec = steps
        .last_mut()
        .expect("exec step is always present before splicing");
    let exec_depends = exec.depends_on.clone();

    match &stage.output_mode {
        OutputMode::Stream { channel } => {
            exec.io.stdout = IOTarget::Pipe {
                name: channel.clone(),
            };
            channels.push(Channel {
                name: channel.clone(),
                ..Channel::default()
            });
        }
        OutputMode::File { name } => {
            exec.io.stdout = IOTarget::File { path: name.clone() };
        }
    }
    exec.collect.retain(|f| f != "output.txt");

    environments.push(stage.checker_env.clone());

    for step in &stage.steps {
        let mut spliced = step.clone();
        if spliced.id == stage.result_step_id {
            match &stage.output_mode {
                // Concurrent with exec -> share exec's upstream deps (compile).
                OutputMode::Stream { .. } => {
                    for dep in &exec_depends {
                        if !spliced.depends_on.contains(dep) {
                            spliced.depends_on.push(dep.clone());
                        }
                    }
                }
                // Sequential after exec -> needs the completed output file.
                OutputMode::File { .. } => {
                    let exec_id = "exec".to_string();
                    if !spliced.depends_on.contains(&exec_id) {
                        spliced.depends_on.push(exec_id);
                    }
                }
            }
        }
        steps.push(spliced);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use broccoli_server_sdk::types::{
        CompileSpec, FileRef, JudgeFile, MountSource, MountSpec, RunSpec, SourceFile, StepKind,
    };

    fn make_req() -> BuildEvalOpsInput {
        BuildEvalOpsInput {
            problem_id: 1,
            test_case_id: 42,
            solution_source: vec![SourceFile {
                filename: "main.cpp".to_string(),
                content: "int main() {}".to_string(),
            }],
            solution_language: "cpp".to_string(),
            time_limit_ms: 1000,
            memory_limit_kb: 262144,
            contest_id: None,
            evaluate_batch_id: None,
            test_input: JudgeFile::inline("hello\n"),
            expected_output: JudgeFile::inline("world\n"),
            checker_format: Some("exact".to_string()),
            checker_config: None,
            additional_file_refs: vec![],
            target_worker_id: None,
        }
    }

    fn compiled_lang() -> ResolveLanguageOutput {
        ResolveLanguageOutput {
            compile: Some(CompileSpec {
                command: vec![
                    "/usr/bin/g++".to_string(),
                    "-O2".to_string(),
                    "solution.cpp".to_string(),
                    "-o".to_string(),
                    "solution".to_string(),
                ],
                cache_inputs: vec!["main.cpp".to_string(), "solution.cpp".to_string()],
                outputs: vec![OutputSpec::File("solution".to_string())],
                resource_limits: None,
            }),
            run: RunSpec {
                command: vec!["./solution".to_string()],
                extra_files: vec![],
                min_process_limit: None,
            },
        }
    }

    fn interpreted_lang() -> ResolveLanguageOutput {
        ResolveLanguageOutput {
            compile: None,
            run: RunSpec {
                command: vec!["/usr/bin/python3".to_string(), "solution.py".to_string()],
                extra_files: vec!["solution.py".to_string()],
                min_process_limit: None,
            },
        }
    }

    fn default_config() -> SandboxConfig {
        SandboxConfig::default()
    }

    #[test]
    fn compiled_language_produces_compile_and_exec_steps() {
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();

        assert_eq!(ops.len(), 1);
        let tasks = &ops[0].tasks;
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "compile");
        assert_eq!(tasks[0].kind, StepKind::Compile);
        assert_eq!(tasks[1].id, "exec");
        assert_eq!(tasks[1].kind, StepKind::Testcase);
        assert_eq!(tasks[1].depends_on, vec!["compile"]);
    }

    #[test]
    fn interpreted_language_produces_only_exec_step() {
        let ops =
            build_operation(&make_req(), &interpreted_lang(), &default_config(), None).unwrap();

        assert_eq!(ops.len(), 1);
        let tasks = &ops[0].tasks;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "exec");
        assert_eq!(tasks[0].kind, StepKind::Testcase);
        assert!(tasks[0].depends_on.is_empty());
    }

    #[test]
    fn test_input_wired_to_environment_files() {
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();

        let env = &ops[0].environments[0];
        let input_file = env
            .files_in
            .iter()
            .find(|(name, _)| name == "input.txt")
            .expect("input.txt not found in environment");
        match &input_file.1 {
            SessionFile::Content { content } => {
                assert_eq!(content, "hello\n");
            }
            _ => panic!("expected inline content for input.txt"),
        }
    }

    #[test]
    fn test_input_blob_ref_wired_to_environment_files() {
        let mut req = make_req();
        req.test_input = JudgeFile::blob(FileRef {
            filename: "input.txt".to_string(),
            content_type: Some("text/plain".to_string()),
            blob_hash: "abc123".to_string(),
            read_token: None,
        });

        let ops = build_operation(&req, &compiled_lang(), &default_config(), None).unwrap();

        let env = &ops[0].environments[0];
        let input_file = env
            .files_in
            .iter()
            .find(|(name, _)| name == "input.txt")
            .expect("input.txt not found in environment");
        match &input_file.1 {
            SessionFile::Blob { hash } => {
                assert_eq!(hash, "abc123");
            }
            _ => panic!("expected blob ref for input.txt"),
        }
    }

    #[test]
    fn source_file_placed_with_correct_filename() {
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();

        let env = &ops[0].environments[0];
        let source_file = env
            .files_in
            .iter()
            .find(|(name, _)| name == "main.cpp")
            .expect("source file not found");
        match &source_file.1 {
            SessionFile::Content { content } => {
                assert_eq!(content, "int main() {}");
            }
            _ => panic!("expected inline content for source file"),
        }
    }

    #[test]
    fn multi_file_submissions_keep_all_files_in_the_environment() {
        let mut req = make_req();
        req.solution_source.push(SourceFile {
            filename: "helper.hpp".to_string(),
            content: "// helper".to_string(),
        });

        let ops = build_operation(&req, &compiled_lang(), &default_config(), None).unwrap();
        let env = &ops[0].environments[0];

        assert!(env.files_in.iter().any(|(name, _)| name == "main.cpp"));
        assert!(env.files_in.iter().any(|(name, _)| name == "helper.hpp"));

        let compile = &ops[0].tasks[0];
        let cache = compile
            .cache
            .as_ref()
            .expect("compile step missing cache config");
        assert_eq!(
            cache.key_inputs,
            vec!["main.cpp".to_string(), "solution.cpp".to_string(),]
        );
    }

    #[test]
    fn time_limit_converted_from_ms_to_seconds() {
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.time_limit, Some(1.0));
    }

    #[test]
    fn memory_limit_passed_through() {
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.memory_limit, Some(262144));
    }

    #[test]
    fn no_source_file_returns_error() {
        let mut req = make_req();
        req.solution_source.clear();
        let result = build_operation(&req, &compiled_lang(), &default_config(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No source file"));
    }

    #[test]
    fn exec_step_collects_stdout_and_stderr() {
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();

        let exec = &ops[0].tasks[1];
        assert!(exec.collect.contains(&"output.txt".to_string()));
        assert!(exec.collect.contains(&"stderr.txt".to_string()));
    }

    #[test]
    fn negative_memory_limit_returns_error() {
        let mut req = make_req();
        req.memory_limit_kb = -1;
        let result = build_operation(&req, &compiled_lang(), &default_config(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid memory_limit_kb"));
    }

    #[test]
    fn negative_time_limit_returns_error() {
        let mut req = make_req();
        req.time_limit_ms = -1;
        let result = build_operation(&req, &compiled_lang(), &default_config(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid time_limit_ms"));
    }

    #[test]
    fn custom_config_overrides_defaults() {
        let config = SandboxConfig {
            exec_process_limit: 4,
            exec_wall_time_multiplier: 5.0,
            compile_time_limit_s: 60.0,
            ..SandboxConfig::default()
        };
        let ops = build_operation(&make_req(), &compiled_lang(), &config, None).unwrap();

        let compile = &ops[0].tasks[0];
        assert_eq!(compile.conf.resource_limits.time_limit, Some(60.0));

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.process_limit, Some(4));
        // 1000ms = 1.0s, wall_time = 1.0 * 5.0 = 5.0s
        assert_eq!(exec.conf.resource_limits.wall_time_limit, Some(5.0));
    }

    /// A runtime floor (`RunSpec.min_process_limit`) raises the exec process cap
    /// above the tight single-process default. This is the JVM path: the default
    /// `exec_process_limit` of 1 aborts the VM at init, so the language resolver
    /// hands up a floor and the op builder must honor it.
    #[test]
    fn runtime_process_floor_raises_tight_exec_limit() {
        let mut lang = compiled_lang();
        lang.run.min_process_limit = Some(64);
        // Default config leaves exec_process_limit at its tight value of 1.
        let ops = build_operation(&make_req(), &lang, &default_config(), None).unwrap();

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.process_limit, Some(64));
    }

    /// The floor only ever moves the cap upward. When an admin has already
    /// configured a higher exec process limit than the runtime's floor, the
    /// configured value wins - the floor is a `max`, not an override.
    #[test]
    fn runtime_process_floor_never_lowers_configured_limit() {
        let mut lang = compiled_lang();
        lang.run.min_process_limit = Some(64);
        let config = SandboxConfig {
            exec_process_limit: 128,
            ..SandboxConfig::default()
        };
        let ops = build_operation(&make_req(), &lang, &config, None).unwrap();

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.process_limit, Some(128));
    }

    #[test]
    fn compile_limits_use_configured_stack_limit() {
        let config = SandboxConfig {
            compile_stack_limit_kb: 262_144,
            ..SandboxConfig::default()
        };
        let ops = build_operation(&make_req(), &compiled_lang(), &config, None).unwrap();

        let compile = &ops[0].tasks[0];
        assert_eq!(compile.conf.resource_limits.stack_limit, Some(262_144));
    }

    #[test]
    fn exec_limits_use_configured_stack_limit() {
        let config = SandboxConfig {
            exec_stack_limit_kb: 262_144,
            ..SandboxConfig::default()
        };
        let ops = build_operation(&make_req(), &compiled_lang(), &config, None).unwrap();

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.stack_limit, Some(262_144));
    }

    #[test]
    fn compile_limits_use_configured_wall_time_multiplier_and_extra_time() {
        let config = SandboxConfig {
            compile_time_limit_s: 40.0,
            compile_wall_time_multiplier: 3.5,
            compile_extra_time_s: 1.5,
            ..SandboxConfig::default()
        };
        let ops = build_operation(&make_req(), &compiled_lang(), &config, None).unwrap();

        let compile = &ops[0].tasks[0];
        assert_eq!(compile.conf.resource_limits.time_limit, Some(40.0));
        assert_eq!(compile.conf.resource_limits.wall_time_limit, Some(140.0));
        assert_eq!(compile.conf.resource_limits.extra_time, Some(1.5));
    }

    #[test]
    fn exec_limits_use_configured_extra_time() {
        let config = SandboxConfig {
            exec_extra_time_s: 2.5,
            ..SandboxConfig::default()
        };
        let ops = build_operation(&make_req(), &compiled_lang(), &config, None).unwrap();

        let exec = &ops[0].tasks[1];
        assert_eq!(exec.conf.resource_limits.extra_time, Some(2.5));
    }

    #[test]
    fn result_timeout_uses_configured_value_as_floor() {
        let config = SandboxConfig {
            result_timeout_ms: 1_200_000,
            ..SandboxConfig::default()
        };

        assert_eq!(config.result_timeout_ms_for(1000, 1), 1_200_000);
    }

    // ----- Checker fusion (Phase 6.1) -----

    fn stream_stage() -> CheckerStage {
        CheckerStage {
            checker_env: Environment {
                id: "checker".to_string(),
                files_in: vec![(
                    "answer.txt".to_string(),
                    SessionFile::Content {
                        content: "world\n".to_string(),
                    },
                )],
            },
            steps: vec![Step {
                id: "check".to_string(),
                kind: StepKind::Checker,
                env_ref: "checker".to_string(),
                argv: vec!["/tools/broccoli-compare".to_string()],
                conf: RunOptions::default(),
                io: IOConfig {
                    stdin: IOTarget::Pipe {
                        name: "sol_out".to_string(),
                    },
                    stdout: IOTarget::File {
                        path: "check_msg.txt".to_string(),
                    },
                    stderr: IOTarget::File {
                        path: "check_err.txt".to_string(),
                    },
                },
                collect: vec!["check_msg.txt".to_string(), "preview.txt".to_string()],
                depends_on: vec![],
                cache: None,
                mounts: vec![],
            }],
            output_mode: OutputMode::Stream {
                channel: "sol_out".to_string(),
            },
            result_step_id: "check".to_string(),
        }
    }

    fn file_stage() -> CheckerStage {
        CheckerStage {
            checker_env: Environment {
                id: "checker".to_string(),
                files_in: vec![
                    (
                        "answer.txt".to_string(),
                        SessionFile::Content {
                            content: "world\n".to_string(),
                        },
                    ),
                    (
                        "input.txt".to_string(),
                        SessionFile::Content {
                            content: "hello\n".to_string(),
                        },
                    ),
                ],
            },
            steps: vec![Step {
                id: "check".to_string(),
                kind: StepKind::Checker,
                env_ref: "checker".to_string(),
                argv: vec!["./checker".to_string()],
                conf: RunOptions::default(),
                io: IOConfig {
                    stdin: IOTarget::Null,
                    stdout: IOTarget::File {
                        path: "checker_out.txt".to_string(),
                    },
                    stderr: IOTarget::File {
                        path: "checker_err.txt".to_string(),
                    },
                },
                collect: vec!["checker_out.txt".to_string()],
                depends_on: vec![],
                cache: None,
                mounts: vec![MountSpec {
                    inside_path: "output.txt".to_string(),
                    source: MountSource::StepOutput {
                        from_step: "exec".to_string(),
                        file: "output.txt".to_string(),
                    },
                }],
            }],
            output_mode: OutputMode::File {
                name: "output.txt".to_string(),
            },
            result_step_id: "check".to_string(),
        }
    }

    fn find_step<'a>(ops: &'a [OperationTask], id: &str) -> &'a Step {
        ops[0]
            .tasks
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("step '{id}' not found"))
    }

    #[test]
    fn fused_stream_pipes_exec_output_and_adds_channel() {
        let stage = stream_stage();
        let ops = build_operation(
            &make_req(),
            &compiled_lang(),
            &default_config(),
            Some(&stage),
        )
        .unwrap();

        // exec stdout becomes a FIFO; the full output is NOT collected.
        let exec = find_step(&ops, "exec");
        match &exec.io.stdout {
            IOTarget::Pipe { name } => assert_eq!(name, "sol_out"),
            other => panic!("expected exec stdout Pipe, got {other:?}"),
        }
        assert!(!exec.collect.contains(&"output.txt".to_string()));
        assert!(exec.collect.contains(&"stderr.txt".to_string()));

        // The channel is declared on the op.
        assert!(ops[0].channels.iter().any(|c| c.name == "sol_out"));

        // The checker env + check step are present.
        assert!(ops[0].environments.iter().any(|e| e.id == "checker"));
        let check = find_step(&ops, "check");
        match &check.io.stdin {
            IOTarget::Pipe { name } => assert_eq!(name, "sol_out"),
            other => panic!("expected check stdin Pipe, got {other:?}"),
        }
        // Concurrent with exec -> depends on compile (NOT exec).
        assert!(check.depends_on.contains(&"compile".to_string()));
        assert!(!check.depends_on.contains(&"exec".to_string()));
    }

    #[test]
    fn fused_file_writes_output_file_and_check_depends_on_exec() {
        let stage = file_stage();
        let ops = build_operation(
            &make_req(),
            &compiled_lang(),
            &default_config(),
            Some(&stage),
        )
        .unwrap();

        let exec = find_step(&ops, "exec");
        match &exec.io.stdout {
            IOTarget::File { path } => assert_eq!(path, "output.txt"),
            other => panic!("expected exec stdout File, got {other:?}"),
        }
        // Output stays worker-local for the mount -> not collected.
        assert!(!exec.collect.contains(&"output.txt".to_string()));
        // No channels for File mode.
        assert!(ops[0].channels.is_empty());

        let check = find_step(&ops, "check");
        // Needs the completed output file (also satisfies the StepOutput mount).
        assert!(check.depends_on.contains(&"exec".to_string()));
        assert_eq!(check.mounts.len(), 1);
    }

    fn step_with_limits(id: &str, time_limit: Option<f64>, wall: Option<f64>) -> Step {
        Step {
            id: id.to_string(),
            kind: StepKind::Checker,
            env_ref: "checker".to_string(),
            argv: vec!["x".to_string()],
            conf: RunOptions {
                resource_limits: ResourceLimits {
                    time_limit,
                    wall_time_limit: wall,
                    ..Default::default()
                },
                ..Default::default()
            },
            io: IOConfig::default(),
            collect: vec![],
            depends_on: vec![],
            cache: None,
            mounts: vec![],
        }
    }

    fn stage_with_steps(steps: Vec<Step>) -> CheckerStage {
        CheckerStage {
            checker_env: Environment {
                id: "checker".to_string(),
                files_in: vec![],
            },
            steps,
            output_mode: OutputMode::File {
                name: "output.txt".to_string(),
            },
            result_step_id: "check".to_string(),
        }
    }

    #[test]
    fn checker_stage_timeout_sums_step_wall_budgets() {
        // compile_checker (10s cpu, no wall) + check (5s cpu, no wall):
        // each cpu limit is scaled by the 3x wall multiplier -> (30 + 15)s.
        let stage = stage_with_steps(vec![
            step_with_limits("compile_checker", Some(10.0), None),
            step_with_limits("check", Some(5.0), None),
        ]);
        assert_eq!(checker_stage_timeout_ms(&stage), 45_000);
    }

    #[test]
    fn checker_stage_timeout_prefers_explicit_wall_limit() {
        let stage = stage_with_steps(vec![step_with_limits("check", Some(5.0), Some(8.0))]);
        assert_eq!(checker_stage_timeout_ms(&stage), 8_000);
    }

    #[test]
    fn checker_stage_timeout_zero_when_no_limits() {
        // Built-in check step uses RunOptions::default() (no time limit).
        let stage = stage_with_steps(vec![step_with_limits("check", None, None)]);
        assert_eq!(checker_stage_timeout_ms(&stage), 0);
    }

    #[test]
    fn fused_op_never_puts_answer_in_solution_env() {
        let ops = build_operation(
            &make_req(),
            &compiled_lang(),
            &default_config(),
            Some(&stream_stage()),
        )
        .unwrap();
        let solution_env = ops[0]
            .environments
            .iter()
            .find(|e| e.id == "sandbox")
            .expect("solution env present");
        assert!(
            !solution_env.files_in.iter().any(|(n, _)| n == "answer.txt"),
            "answer must never appear in the solution env"
        );
    }

    #[test]
    fn no_stage_keeps_legacy_collected_output() {
        // None checker stage (e.g. the `none` format) -> unchanged behavior.
        let ops = build_operation(&make_req(), &compiled_lang(), &default_config(), None).unwrap();
        let exec = find_step(&ops, "exec");
        match &exec.io.stdout {
            IOTarget::File { path } => assert_eq!(path, "output.txt"),
            other => panic!("expected exec stdout File, got {other:?}"),
        }
        assert!(exec.collect.contains(&"output.txt".to_string()));
        assert!(ops[0].channels.is_empty());
        assert_eq!(ops[0].environments.len(), 1);
    }

    #[test]
    fn result_timeout_scales_with_worst_case_wall_budget() {
        let config = SandboxConfig {
            compile_time_limit_s: 120.0,
            compile_wall_time_multiplier: 3.0,
            exec_wall_time_multiplier: 5.0,
            ..SandboxConfig::default()
        };

        assert!(config.result_timeout_ms_for(300_000, 1) > config.result_timeout_ms);
    }
}
