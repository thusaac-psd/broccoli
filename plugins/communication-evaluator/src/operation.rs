use broccoli_server_sdk::types::{
    BuildEvalOpsInput, Channel, Environment, IOConfig, IOTarget, OperationTask, OutputSpec,
    ResolveLanguageOutput, RunOptions, SessionFile, Step, StepCacheConfig, StepKind,
};

use crate::config::{CommConfig, CommunicationMode, ManagerSourceEntry, SandboxConfig};

/// Build the communication operation for one test case.
pub fn build_operation(
    req: &BuildEvalOpsInput,
    contestant_lang: &ResolveLanguageOutput,
    manager_lang: &ResolveLanguageOutput,
    manager_files: &[ManagerSourceEntry],
    comm_config: &CommConfig,
    sandbox_config: &SandboxConfig,
) -> Result<Vec<OperationTask>, String> {
    let n = comm_config.num_processes as usize;
    if n == 0 {
        return Err("num_processes must be >= 1".to_string());
    }

    let time_limit_ms = u32::try_from(req.time_limit_ms)
        .map_err(|_| format!("Invalid time_limit_ms: {}", req.time_limit_ms))?;
    let time_limit_s = time_limit_ms as f64 / 1000.0;
    let memory_limit_kb = u32::try_from(req.memory_limit_kb)
        .map_err(|_| format!("Invalid memory_limit_kb: {}", req.memory_limit_kb))?;

    let buffer_size = crate::config::FIFO_BUFFER_SIZE;
    let mut channels = Vec::new();
    for i in 0..n {
        // Declare each channel's producer/consumer STEP ids. The manager opens
        // every channel FIFO via a raw argv path (see `manager_argv` below), and
        // in `fifo_args` mode the contestant does too -- neither is an
        // `IOTarget::Pipe` on stdio, so the worker's keep-alive scan cannot see
        // them. Without these hints the argv-opened channels get no keep-alive and
        // a peer that fails to open its end (an early exit, or an EAGAIN-failed
        // exec under burst) strands the other side until its wall-time (the
        // manager's is `manager_time_limit_s * exec_wall_time_multiplier`, 150s by
        // default). The ids match the steps pushed below and hold in both modes;
        // in `redirect` they simply agree with the stdio-pipe detection.
        channels.push(Channel {
            name: format!("c{i}_to_m"),
            buffer_size: Some(buffer_size),
            producer_step: Some(format!("run_contestant_{i}")),
            consumer_step: Some("run_manager".to_string()),
        });
        channels.push(Channel {
            name: format!("m_to_c{i}"),
            buffer_size: Some(buffer_size),
            producer_step: Some("run_manager".to_string()),
            consumer_step: Some(format!("run_contestant_{i}")),
        });
    }

    let mut environments = Vec::new();

    let mut manager_files_in: Vec<(String, SessionFile)> = manager_files
        .iter()
        .map(|f| {
            (
                f.filename.clone(),
                SessionFile::Blob {
                    hash: f.hash.clone(),
                },
            )
        })
        .collect();
    manager_files_in.push(("input.txt".to_string(), req.test_input.to_session_file()));

    environments.push(Environment {
        id: "manager_env".to_string(),
        files_in: manager_files_in,
    });

    if req.solution_source.is_empty() {
        return Err("No contestant source file provided".into());
    }

    for i in 0..n {
        let mut files_in: Vec<(String, SessionFile)> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for af in &req.additional_file_refs {
            if seen.insert(af.filename.clone()) {
                files_in.push((
                    af.filename.clone(),
                    SessionFile::Blob {
                        hash: af.blob_hash.clone(),
                    },
                ));
            }
        }

        for source in &req.solution_source {
            if seen.insert(source.filename.clone()) {
                files_in.push((
                    source.filename.clone(),
                    SessionFile::Content {
                        content: source.content.clone(),
                    },
                ));
            }
        }

        environments.push(Environment {
            id: format!("contestant_{i}"),
            files_in,
        });
    }

    let mut steps = Vec::new();

    if let Some(compile) = &manager_lang.compile {
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

        steps.push(Step {
            id: "compile_manager".to_string(),
            kind: StepKind::Compile,
            env_ref: "manager_env".to_string(),
            argv: compile.command.clone(),
            conf: RunOptions {
                resource_limits: compile
                    .resource_limits
                    .clone()
                    .unwrap_or_else(|| sandbox_config.compile_limits()),
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
            mounts: Vec::new(),
            cache: Some(StepCacheConfig {
                key_inputs: compile.cache_inputs.clone(),
                outputs: cache_outputs,
            }),
        });
    }

    for i in 0..n {
        if let Some(compile) = &contestant_lang.compile {
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

            steps.push(Step {
                id: format!("compile_contestant_{i}"),
                kind: StepKind::Compile,
                env_ref: format!("contestant_{i}"),
                argv: compile.command.clone(),
                conf: RunOptions {
                    resource_limits: compile
                        .resource_limits
                        .clone()
                        .unwrap_or_else(|| sandbox_config.compile_limits()),
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
                mounts: Vec::new(),
                cache: Some(StepCacheConfig {
                    key_inputs: compile.cache_inputs.clone(),
                    outputs: cache_outputs,
                }),
            });
        }
    }

    let mut all_compile_deps: Vec<String> = Vec::new();
    if manager_lang.compile.is_some() {
        all_compile_deps.push("compile_manager".to_string());
    }
    if contestant_lang.compile.is_some() {
        for i in 0..n {
            all_compile_deps.push(format!("compile_contestant_{i}"));
        }
    }

    let mut manager_argv = manager_lang.run.command.clone();
    for i in 0..n {
        // argv order: write-to-contestant pipe first, read-from-contestant pipe second.
        // Manager code opens argv[1+2*i] for writing and argv[2+2*i] for reading.
        let write_pipe = format!("channels/m_to_c{i}");
        let read_pipe = format!("channels/c{i}_to_m");
        manager_argv.push(write_pipe);
        manager_argv.push(read_pipe);
    }

    steps.push(Step {
        id: "run_manager".to_string(),
        kind: StepKind::Testcase,
        env_ref: "manager_env".to_string(),
        argv: manager_argv,
        conf: RunOptions {
            resource_limits: sandbox_config.manager_limits(
                comm_config.manager_time_limit_s,
                comm_config.manager_memory_limit_kb,
                comm_config.num_processes,
            ),
            wait: true,
            env_rules: vec![],
            ..Default::default()
        },
        io: IOConfig {
            stdin: IOTarget::File {
                path: "input.txt".to_string(),
            },
            stdout: IOTarget::Inherit,
            stderr: IOTarget::Inherit,
        },
        collect: vec![],
        depends_on: all_compile_deps.clone(),
        mounts: Vec::new(),
        cache: None,
    });

    for i in 0..n {
        let (io, argv) = match comm_config.communication_mode {
            CommunicationMode::Redirect => {
                let io = IOConfig {
                    stdin: IOTarget::Pipe {
                        name: format!("m_to_c{i}"),
                    },
                    stdout: IOTarget::Pipe {
                        name: format!("c{i}_to_m"),
                    },
                    stderr: IOTarget::Inherit,
                };
                let mut argv = contestant_lang.run.command.clone();
                argv.push(i.to_string());
                (io, argv)
            }
            CommunicationMode::FifoArgs => {
                let io = IOConfig {
                    stdin: IOTarget::Null,
                    stdout: IOTarget::Null,
                    stderr: IOTarget::Inherit,
                };
                let mut argv = contestant_lang.run.command.clone();
                argv.push(format!("channels/m_to_c{i}"));
                argv.push(format!("channels/c{i}_to_m"));
                argv.push(i.to_string());
                (io, argv)
            }
        };

        steps.push(Step {
            id: format!("run_contestant_{i}"),
            kind: StepKind::Testcase,
            env_ref: format!("contestant_{i}"),
            argv,
            conf: RunOptions {
                resource_limits: sandbox_config.exec_limits(time_limit_s, memory_limit_kb),
                wait: true,
                env_rules: vec![],
                ..Default::default()
            },
            io,
            collect: vec![],
            depends_on: all_compile_deps.clone(),
            mounts: Vec::new(),
            cache: None,
        });
    }

    Ok(vec![OperationTask {
        environments,
        tasks: steps,
        channels,
        target_worker_id: req.target_worker_id.clone(),
        // Tag the op so the host's `record_ops` registers the op->batch mapping;
        // without both ids the result wait can't extend and judging reports
        // EVALUATION_TIMEOUT. `evaluate_batch_id` is stamped by the host in
        // resolve_inputs; `test_case_id` comes from the per-case request.
        evaluate_batch_id: req.evaluate_batch_id.clone(),
        test_case_id: Some(req.test_case_id),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;
    use broccoli_server_sdk::types::{CompileSpec, JudgeFile, RunSpec, SourceFile};

    fn make_req() -> BuildEvalOpsInput {
        BuildEvalOpsInput {
            problem_id: 1,
            test_case_id: 42,
            solution_source: vec![SourceFile {
                filename: "main.cpp".to_string(),
                content: "int main() {}".to_string(),
            }],
            solution_language: "cpp".to_string(),
            time_limit_ms: 2000,
            memory_limit_kb: 262144,
            contest_id: None,
            evaluate_batch_id: Some("test-batch".to_string()),
            test_input: JudgeFile::inline("5\n1 2 3 4 5\n"),
            expected_output: JudgeFile::Missing,
            checker_format: None,
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
                cache_inputs: vec!["main.cpp".to_string()],
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

    fn manager_lang() -> ResolveLanguageOutput {
        ResolveLanguageOutput {
            compile: Some(CompileSpec {
                command: vec![
                    "/usr/bin/g++".to_string(),
                    "-O2".to_string(),
                    "manager.cpp".to_string(),
                    "-o".to_string(),
                    "manager".to_string(),
                ],
                cache_inputs: vec!["manager.cpp".to_string()],
                outputs: vec![OutputSpec::File("manager".to_string())],
                resource_limits: None,
            }),
            run: RunSpec {
                command: vec!["./manager".to_string()],
                extra_files: vec![],
                min_process_limit: None,
            },
        }
    }

    fn mgr_files() -> Vec<ManagerSourceEntry> {
        vec![ManagerSourceEntry {
            filename: "manager.cpp".to_string(),
            hash: "abc123deadbeef".to_string(),
        }]
    }

    use crate::config::tests::{default_comm, default_sandbox};

    fn build(req: &BuildEvalOpsInput, comm: &CommConfig) -> Vec<OperationTask> {
        build_operation(
            req,
            &compiled_lang(),
            &manager_lang(),
            &mgr_files(),
            comm,
            &default_sandbox(),
        )
        .unwrap()
    }

    #[test]
    fn operation_steps_mark_compile_and_runtime_kinds() {
        let ops = build(&make_req(), &default_comm());
        let tasks = &ops[0].tasks;

        let compile_manager = tasks.iter().find(|s| s.id == "compile_manager").unwrap();
        assert_eq!(compile_manager.kind, StepKind::Compile);

        let compile_contestant = tasks
            .iter()
            .find(|s| s.id == "compile_contestant_0")
            .unwrap();
        assert_eq!(compile_contestant.kind, StepKind::Compile);

        let run_manager = tasks.iter().find(|s| s.id == "run_manager").unwrap();
        assert_eq!(run_manager.kind, StepKind::Testcase);

        let run_contestant = tasks.iter().find(|s| s.id == "run_contestant_0").unwrap();
        assert_eq!(run_contestant.kind, StepKind::Testcase);
    }

    #[test]
    fn manager_source_from_config_blob_hash() {
        let ops = build(&make_req(), &default_comm());
        let mgr_env = &ops[0].environments[0];

        let mgr_file = mgr_env
            .files_in
            .iter()
            .find(|(name, _)| name == "manager.cpp")
            .expect("manager source not in environment");
        match &mgr_file.1 {
            SessionFile::Blob { hash } => assert_eq!(hash, "abc123deadbeef"),
            _ => panic!("expected Blob for manager source"),
        }
    }

    #[test]
    fn multi_file_manager_mounts_all_files() {
        let files = vec![
            ManagerSourceEntry {
                filename: "manager.cpp".to_string(),
                hash: "hash_mgr".to_string(),
            },
            ManagerSourceEntry {
                filename: "manager.h".to_string(),
                hash: "hash_hdr".to_string(),
            },
        ];
        let ops = build_operation(
            &make_req(),
            &compiled_lang(),
            &manager_lang(),
            &files,
            &default_comm(),
            &default_sandbox(),
        )
        .unwrap();

        let mgr_env = &ops[0].environments[0];

        let cpp_file = mgr_env
            .files_in
            .iter()
            .find(|(name, _)| name == "manager.cpp")
            .expect("manager.cpp not in environment");
        match &cpp_file.1 {
            SessionFile::Blob { hash } => assert_eq!(hash, "hash_mgr"),
            _ => panic!("expected Blob for manager.cpp"),
        }

        let h_file = mgr_env
            .files_in
            .iter()
            .find(|(name, _)| name == "manager.h")
            .expect("manager.h not in environment");
        match &h_file.1 {
            SessionFile::Blob { hash } => assert_eq!(hash, "hash_hdr"),
            _ => panic!("expected Blob for manager.h"),
        }

        let compile_mgr = ops[0]
            .tasks
            .iter()
            .find(|s| s.id == "compile_manager")
            .unwrap();
        let cache = compile_mgr.cache.as_ref().unwrap();
        assert_eq!(cache.key_inputs, vec!["manager.cpp".to_string()]);
    }

    #[test]
    fn stubs_in_solution_source_appear_in_contestant_env() {
        let mut req = make_req();
        // Server has already merged stubs into solution_source
        req.solution_source.push(SourceFile {
            filename: "stub.cpp".to_string(),
            content: "void stub_fn() {}".to_string(),
        });

        let ops = build(&req, &default_comm());
        let c0_env = ops[0]
            .environments
            .iter()
            .find(|e| e.id == "contestant_0")
            .unwrap();
        assert!(
            c0_env.files_in.iter().any(|(name, _)| name == "stub.cpp"),
            "stub should be in contestant environment"
        );
    }
}
