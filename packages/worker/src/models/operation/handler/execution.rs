use super::super::cache_leader::LeaderRole;
use super::super::file_cacher::FileCacher;
use super::super::models::*;
use super::super::sandbox::{
    DirectoryOptions, DirectoryRule, ExecutionResult, RunOptions, SandboxManager, SandboxStatus,
};
use super::super::task_cache::TaskCacheStore;
use super::box_id::{allocate_box_id, next_channel_seq};
use super::metrics::{sandbox_exit_kind, sandbox_status_label, step_kind};
use super::paths::{
    create_fifo, platform_tool_directory_rule, resolve_step_output_src, stage_step_output_file,
    validate_pipe_name,
};
use super::teardown::Leftovers;
use super::{EnvironmentList, OperationHandler, StepMetricRecord};
use anyhow::{Context, Result, anyhow};
use futures::future::{FutureExt, join_all};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{Instrument, debug, error, info, instrument, warn};

impl OperationHandler {
    pub fn new(
        sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
        file_cacher: Box<dyn FileCacher>,
        task_cache: Box<dyn TaskCacheStore>,
        toolchain_fingerprint: crate::toolchain_fingerprint::ToolchainFingerprint,
        metrics: common::metrics::Metrics,
    ) -> Self {
        Self::with_compile_cache(
            sandbox_manager,
            Arc::from(file_cacher),
            super::CompileCache::local(Arc::from(task_cache), toolchain_fingerprint),
            metrics,
        )
    }

    pub fn with_compile_cache(
        sandbox_manager: Box<dyn SandboxManager + Send + Sync>,
        file_cacher: Arc<dyn FileCacher>,
        cache: super::CompileCache,
        metrics: common::metrics::Metrics,
    ) -> Self {
        Self {
            sandbox_manager: Arc::from(sandbox_manager),
            file_cacher,
            cache,
            metrics,
            tools_dir: None,
            teardown: Default::default(),
        }
    }

    /// The file cacher this handler materializes blobs through. Anything else
    /// writing the same cache directory must use it too, so the directory has
    /// one size cap.
    pub fn file_cacher(&self) -> Arc<dyn FileCacher> {
        self.file_cacher.clone()
    }

    /// Set the worker-local platform tools directory (builder style so existing
    /// constructor call sites are unaffected).
    pub fn with_tools_dir(mut self, tools_dir: Option<PathBuf>) -> Self {
        self.tools_dir = tools_dir;
        self
    }

    #[instrument(skip(self, operation))]
    pub async fn execute(&self, operation: &OperationTask) -> Result<OperationResult> {
        info!(
            "Starting operation execution with {} environments and {} tasks",
            operation.environments.len(),
            operation.tasks.len()
        );

        // Sandboxes still being removed after the previous operation must be
        // gone before this one creates its own, so no step runs beside a
        // removal and no box id is reused while its box is still dirty.
        self.teardown.settle().await;

        // Run every stage under a panic guard so the sandbox + channels
        // removal below ALWAYS runs, and runs once. Each stage records what it
        // created in `environments` / `channels_dir` as it goes, so a stage
        // that fails or panics midway leaves nothing behind, and its own
        // error is the one returned.
        let mut environments = HashMap::new();
        let mut channels_dir = None;
        let outcome = AssertUnwindSafe(async {
            self.create_environments(operation, &mut environments)
                .await?;
            self.run_operation(operation, &environments, &mut channels_dir)
                .await
        })
        .catch_unwind()
        .await;

        let leftovers = Leftovers {
            environments,
            channels_dir,
        };
        // The removal runs in the background, off the result's path; the
        // next operation waits for it (see `teardown`). A panic is re-raised
        // only after its removal.
        if outcome.is_ok() {
            self.teardown.defer(self.sandbox_manager.clone(), leftovers);
        } else {
            leftovers.remove(&*self.sandbox_manager).await;
        }

        let (task_results, global_success) = match outcome {
            Ok(result) => result?,
            Err(panic) => {
                error!("A step future panicked; sandboxes removed, re-raising the panic");
                std::panic::resume_unwind(panic);
            }
        };

        info!(
            success = global_success,
            tasks_count = task_results.len(),
            "Operation execution completed"
        );

        Ok(OperationResult {
            success: global_success,
            task_results,
            error: None,
        })
    }

    /// Create one sandbox per environment, all at once, and load its files,
    /// recording each sandbox in `environments` once it exists so the caller
    /// removes it whatever happens next. Returns the first failure.
    async fn create_environments(
        &self,
        operation: &OperationTask,
        environments: &mut HashMap<String, EnvironmentList>,
    ) -> Result<()> {
        let created = join_all(operation.environments.iter().map(|env_config| async move {
            let box_id = allocate_box_id();
            debug!(env_id = %env_config.id, box_id = %box_id, "Initializing environment");
            let working_dir = self.create_sandbox(box_id.as_str()).await;
            let loaded = match &working_dir {
                Ok(dir) => self.load_environment_files(dir, &env_config.files_in).await,
                Err(_) => Ok(()),
            };
            (env_config, box_id, working_dir, loaded)
        }))
        .await;

        let mut first_error = None;
        for (env_config, box_id, working_dir, loaded) in created {
            match working_dir {
                Ok(working_dir) => {
                    environments.insert(
                        env_config.id.clone(),
                        EnvironmentList::new(env_config.id.clone(), box_id, working_dir),
                    );
                    if let Err(e) = loaded {
                        first_error.get_or_insert(e.context("Failed to load environment files"));
                    }
                }
                Err(e) => {
                    first_error.get_or_insert(e.context("Failed to create sandbox"));
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// Everything after the environments exist: channels, keep-alives and the
    /// execution layers. `channels_dir` is set as soon as the shared channels
    /// directory exists, so the caller removes it whatever happens next.
    /// Returns the per-step results and whether every step succeeded.
    async fn run_operation(
        &self,
        operation: &OperationTask,
        environments: &HashMap<String, EnvironmentList>,
        channels_dir: &mut Option<PathBuf>,
    ) -> Result<(HashMap<String, TaskExecutionResult>, bool)> {
        let channel_names: HashSet<String> =
            operation.channels.iter().map(|c| c.name.clone()).collect();
        for name in &channel_names {
            validate_pipe_name(name)?;
        }
        if !channel_names.is_empty() {
            let dir = std::env::temp_dir().join(format!(
                "broccoli-channels-{}-{}-{}",
                std::process::id(),
                next_channel_seq(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            ));
            tokio::fs::create_dir_all(&dir)
                .await
                .context("Failed to create shared channels directory")?;
            *channels_dir = Some(dir.clone());
            create_channel_fifos(&dir, &operation.channels).await?;
        }
        let shared_channels_dir = channels_dir.clone();

        let execution_layers = self.get_execution_order(operation)?;
        debug!(layers = ?execution_layers, "Task execution layers determined");

        // Map each step id to its environment's working dir so a dependent step
        // can mount a prior step's captured output file directly
        // (MountSource::StepOutput), no blob round-trip.
        let step_working_dirs: HashMap<String, PathBuf> = operation
            .tasks
            .iter()
            .filter_map(|t| {
                environments
                    .get(&t.env_ref)
                    .map(|env| (t.id.clone(), env.working_dir.clone()))
            })
            .collect();

        let mut task_results = HashMap::new();
        let mut global_success = true;

        // Channel keep-alive write fds. Each channel is a named FIFO the
        // producer step writes and the consumer step reads via a concurrent
        // `open(O_RDONLY)`, which blocks in the kernel until some writer opens
        // the O_WRONLY end. If the producer never opens it (its exec fork/spawn
        // fails) or resolves before the consumer's isolate reaches its stdin
        // open, the consumer would block forever. So an O_RDWR fd is held on
        // each producer's FIFO (opening O_RDWR never blocks) and handed to
        // `release_channel_keepalive` when the producer resolves, which
        // delivers EOF once the consumer can receive it. The per-consumer
        // `ConsumerDone` ends the release as soon as that consumer finishes.
        let mut channel_keepalives: HashMap<String, Vec<KeepAlive>> = HashMap::new();
        let mut consumer_flags: HashMap<String, Arc<ConsumerDone>> = HashMap::new();
        if let Some(ref dir) = shared_channels_dir {
            let (producer_of, consumer_of) =
                resolve_channel_roles(&operation.tasks, &operation.channels, &channel_names);
            for channel in &operation.channels {
                // Only manage a keep-alive for channels with an identifiable
                // producer step. An orphan channel (consumed, never produced) keeps
                // its pre-existing behavior; there is no safe moment to close a
                // writer we can't tie to a producer's completion.
                if let Some(producer_id) = producer_of.get(&channel.name)
                    && let Some(file) = open_channel_keepalive(&dir.join(&channel.name))
                {
                    let consumer_done = consumer_of.get(&channel.name).map(|consumer_id| {
                        consumer_flags
                            .entry(consumer_id.clone())
                            .or_default()
                            .clone()
                    });
                    channel_keepalives
                        .entry(producer_id.clone())
                        .or_default()
                        .push(KeepAlive {
                            fifo_path: dir.join(&channel.name),
                            file,
                            consumer_done,
                        });
                }
            }
        }

        // Box-local (non-channel) pipes need the SAME keep-alive rendezvous the
        // channel block above gives declared channels. A `Pipe` whose name is not
        // a declared channel is a per-environment FIFO at
        // `<env working_dir>/pipes/<name>` (see `prepare_io_target`). When a
        // producer and its consumer land in the same execution layer they run
        // concurrently, so the identical race applies: the producer can open,
        // write, and close its write end before the consumer opens the read end --
        // the kernel then discards the buffered bytes and the consumer sees a
        // premature EOF (empty read); or a fork-inherited dup of a transient
        // O_RDWR fd (O_CLOEXEC closes at execve, not fork) keeps a phantom writer
        // alive so a line-oriented consumer blocks forever. Holding an O_RDWR
        // keep-alive from before the layer until the consumer's done-flag closes
        // both tails, reusing the channel machinery (`channel_keepalives`,
        // `consumer_flags`, `release_channel_keepalive`) unchanged. Production
        // never wires two concurrent local-pipe steps; this is a correctness
        // backstop the mock-sandbox tests exercise directly.
        {
            let (producer_of, consumer_of) =
                resolve_local_pipe_roles(&operation.tasks, &channel_names);
            for ((env_ref, pipe_name), producer_id) in &producer_of {
                let Some(working_dir) = step_working_dirs.get(producer_id) else {
                    continue;
                };
                let pipes_dir = working_dir.join("pipes");
                let fifo_path = pipes_dir.join(pipe_name);
                // Pre-create the FIFO so the keep-alive can open before the producer
                // runs. `prepare_io_target` creates the same path lazily and reuses
                // an existing FIFO, so racing it is harmless; a genuine non-FIFO
                // conflict is left for `prepare_io_target` to surface.
                if !path_is_fifo(&fifo_path) {
                    if let Err(e) = tokio::fs::create_dir_all(&pipes_dir).await {
                        warn!(
                            path = %pipes_dir.display(),
                            error = %e,
                            "Failed to create local pipes dir for keep-alive; consumer relies on the worker hard timeout"
                        );
                        continue;
                    }
                    let _ = create_fifo(&fifo_path);
                    if !path_is_fifo(&fifo_path) {
                        continue;
                    }
                }
                if let Some(file) = open_channel_keepalive(&fifo_path) {
                    let consumer_done =
                        consumer_of
                            .get(&(env_ref.clone(), pipe_name.clone()))
                            .map(|consumer_id| {
                                consumer_flags
                                    .entry(consumer_id.clone())
                                    .or_default()
                                    .clone()
                            });
                    channel_keepalives
                        .entry(producer_id.clone())
                        .or_default()
                        .push(KeepAlive {
                            fifo_path,
                            file,
                            consumer_done,
                        });
                }
            }
        }

        for layer in execution_layers {
            let mut futures = Vec::new();
            for task_id in &layer {
                let task = operation
                    .tasks
                    .iter()
                    .find(|t| t.id == *task_id)
                    .ok_or_else(|| {
                        anyhow!(
                            "Task '{}' not found — dependency graph inconsistency",
                            task_id
                        )
                    })?;

                let deps_ok = task.depends_on.iter().all(|dep_id| {
                    task_results
                        .get(dep_id)
                        .map(|r: &TaskExecutionResult| r.success)
                        .unwrap_or(false)
                });

                // Hand this step the keep-alive fds for any channels it
                // produces. They are released -- delivering EOF to the
                // concurrent consumer -- only once the consumer has opened the
                // FIFO (see `release_channel_keepalive`), not merely when this
                // producer resolves. `own_done` is this step's consumer flag (if
                // it reads a channel); we raise it before releasing so a paired
                // producer's release routine can stop promptly.
                let produced_keepalives = channel_keepalives.remove(task_id);
                let own_done = consumer_flags.get(task_id).cloned();
                let step_future = self.execute_step_with_deps(
                    task,
                    environments,
                    &step_working_dirs,
                    deps_ok,
                    shared_channels_dir.as_deref(),
                    &channel_names,
                );
                futures.push(async move {
                    let result = step_future.await;
                    if let Some(flag) = &own_done {
                        flag.set();
                    }
                    if let Some(keepalives) = produced_keepalives {
                        release_channel_keepalives(keepalives).await;
                    }
                    result
                });
            }

            let results = join_all(futures).await;
            for result in results {
                if !result.success {
                    global_success = false;
                }
                task_results.insert(result.task_id.clone(), result);
            }
        }
        Ok((task_results, global_success))
    }

    fn build_dependency_graph(&self, operation: &OperationTask) -> HashMap<String, Vec<String>> {
        let mut graph = HashMap::new();

        for task in &operation.tasks {
            graph.insert(task.id.clone(), task.depends_on.clone());
        }

        graph
    }

    fn get_execution_order(&self, operation: &OperationTask) -> Result<Vec<Vec<String>>> {
        let graph = self.build_dependency_graph(operation);
        let mut in_degree: HashMap<String, usize> = HashMap::new();
        let mut adj_list: HashMap<String, Vec<String>> = HashMap::new();

        for task in &operation.tasks {
            in_degree.insert(task.id.clone(), 0);
            adj_list.insert(task.id.clone(), Vec::new());
        }

        for (task_id, deps) in &graph {
            let mut degree = 0;
            for dep in deps {
                if !operation.tasks.iter().any(|t| &t.id == dep) {
                    return Err(anyhow!("Dependency task not found: {}", dep));
                }
                degree += 1;
                if let Some(adj) = adj_list.get_mut(dep) {
                    adj.push(task_id.clone());
                }
            }
            in_degree.insert(task_id.clone(), degree);
        }

        let mut current_layer: Vec<_> = in_degree
            .iter()
            .filter(|(_, degree)| **degree == 0)
            .map(|(id, _)| id.clone())
            .collect();

        let mut layers = Vec::new();
        let mut total = 0;

        while !current_layer.is_empty() {
            let mut next_layer = Vec::new();

            for task_id in &current_layer {
                if let Some(neighbors) = adj_list.get(task_id) {
                    for neighbor in neighbors {
                        if let Some(degree) = in_degree.get_mut(neighbor) {
                            *degree -= 1;
                            if *degree == 0 {
                                next_layer.push(neighbor.clone());
                            }
                        }
                    }
                }
            }

            total += current_layer.len();
            layers.push(current_layer);
            current_layer = next_layer;
        }

        if total != operation.tasks.len() {
            return Err(anyhow!(
                "Circular dependency detected or task missing in dependency graph"
            ));
        }

        Ok(layers)
    }

    async fn execute_step_with_deps(
        &self,
        step: &Step,
        environments: &HashMap<String, EnvironmentList>,
        step_working_dirs: &HashMap<String, PathBuf>,
        deps_ok: bool,
        shared_channels_dir: Option<&Path>,
        channel_names: &HashSet<String>,
    ) -> TaskExecutionResult {
        let start = std::time::Instant::now();

        if !deps_ok {
            warn!(task_id = %step.id, "Skipping task due to dependency failure");
            self.record_step_metrics(StepMetricRecord {
                start,
                step_kind: step_kind(step),
                outcome: "skipped",
                sandbox_status: "dependency_failed".to_string(),
                exit_kind: "none",
                killed: false,
                cg_oom_killed: false,
            });
            return TaskExecutionResult {
                task_id: step.id.clone(),
                success: false,
                sandbox_result: ExecutionResult::default(),
                collected_outputs: HashMap::new(),
            };
        }

        // Cached cache_key (computed at most once per execute_step_with_deps call) so
        // try_cache_hit, leader-acquire and store_in_cache all see the same key.
        let mut cache_key_cache: Option<String> = None;

        if let Some(cache_spec) = &step.cache
            && let Some(cached) = self
                .try_cache_hit(step, environments, cache_spec, &mut cache_key_cache)
                .await
        {
            self.record_step_metrics(StepMetricRecord {
                start,
                step_kind: step_kind(step),
                outcome: "cache_hit",
                sandbox_status: "cache_hit".to_string(),
                exit_kind: "none",
                killed: false,
                cg_oom_killed: false,
            });
            return cached;
        }

        // Leader-election around the cache miss: if another worker is already
        // computing this same key, become a follower and poll task_cache until
        // they store the result (or our max_wait elapses).
        //
        // The `_lease` binding keeps the leader's Redis lock alive (with
        // heartbeat) through `execute_step` + `store_in_cache`. Drop happens
        // at the end of this function and fires a CAS-release.
        let _lease;
        if let Some(cache_spec) = &step.cache {
            // None here means we couldn't compute a key (e.g. env missing or
            // input file read error); fall through with an empty key so we
            // skip leader-election but still attempt execution.
            let cache_key = self
                .ensure_cache_key(step, environments, cache_spec, &mut cache_key_cache)
                .await
                .unwrap_or_default();

            if !cache_key.is_empty() {
                match self.cache.leader.acquire(&cache_key).await {
                    Ok(LeaderRole::Leader(lease)) => {
                        _lease = Some(lease);
                        debug!(step_id = %step.id, cache_key = %cache_key, "cache-leader: leading");
                    }
                    Ok(LeaderRole::Follower) => {
                        _lease = None;
                        debug!(step_id = %step.id, cache_key = %cache_key, "cache-leader: following — polling task_cache");
                        if let Some(restored) = self
                            .follower_poll_loop(step, environments, &cache_key)
                            .await
                        {
                            self.record_step_metrics(StepMetricRecord {
                                start,
                                step_kind: step_kind(step),
                                outcome: "cache_hit",
                                sandbox_status: "cache_follower_hit".to_string(),
                                exit_kind: "none",
                                killed: false,
                                cg_oom_killed: false,
                            });
                            return restored;
                        }
                        warn!(step_id = %step.id, cache_key = %cache_key, "cache-leader: follower timed out, falling back to execution");
                    }
                    Err(e) => {
                        _lease = None;
                        warn!(step_id = %step.id, cache_key = %cache_key, error = %e, "cache-leader: acquire failed, executing normally");
                    }
                }
            } else {
                _lease = None;
            }
        } else {
            _lease = None;
        }

        let result = match self
            .execute_step(
                step,
                environments,
                step_working_dirs,
                shared_channels_dir,
                channel_names,
            )
            .instrument(tracing::info_span!(
                "operation_step",
                step_id = %step.id,
                step_kind = %step_kind(step),
                env_ref = %step.env_ref,
            ))
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(task_id = %step.id, error = %e, "Task execution error");
                self.record_step_metrics(StepMetricRecord {
                    start,
                    step_kind: step_kind(step),
                    outcome: "failure",
                    sandbox_status: "execution_error".to_string(),
                    exit_kind: "none",
                    killed: false,
                    cg_oom_killed: false,
                });
                return TaskExecutionResult {
                    task_id: step.id.clone(),
                    success: false,
                    // Reaching here means execute_step returned Err BEFORE the
                    // sandbox ran the step: a malformed operation (invalid pipe
                    // name, unsafe redirect path) or an environment setup failure.
                    // Such errors are deterministic -- they fail identically on
                    // retry -- so they stay a terminal default (status "UNKNOWN").
                    // The self-healing InternalError is reserved for genuine sandbox
                    // infrastructure faults, tagged inside execute_step at the
                    // sandbox_manager.execute() call site.
                    sandbox_result: ExecutionResult::default(),
                    collected_outputs: HashMap::new(),
                };
            }
        };

        if result.success
            && let Some(cache_spec) = &step.cache
        {
            self.store_in_cache(
                step,
                environments,
                cache_spec,
                &result.collected_outputs,
                &mut cache_key_cache,
            )
            .await;
        }
        drop(_lease);

        self.record_sandbox_metrics(&result.sandbox_result, result.success);
        self.record_step_metrics(StepMetricRecord {
            start,
            step_kind: step_kind(step),
            outcome: if result.success { "success" } else { "failure" },
            sandbox_status: sandbox_status_label(&result.sandbox_result),
            exit_kind: sandbox_exit_kind(&result.sandbox_result),
            killed: result.sandbox_result.killed,
            cg_oom_killed: result.sandbox_result.cg_oom_killed,
        });
        result
    }

    #[instrument(skip(self, step, environments, shared_channels_dir, channel_names))]
    async fn execute_step(
        &self,
        step: &Step,
        environments: &HashMap<String, EnvironmentList>,
        step_working_dirs: &HashMap<String, PathBuf>,
        shared_channels_dir: Option<&Path>,
        channel_names: &HashSet<String>,
    ) -> Result<TaskExecutionResult> {
        debug!(step_id = %step.id, "Executing step");

        let env = environments
            .get(&step.env_ref)
            .ok_or_else(|| anyhow!("Environment not found: {}", step.env_ref))?;

        let (stdin_path, stdout_path, stderr_path) = self
            .prepare_io(
                &env.working_dir,
                &step.io,
                shared_channels_dir,
                channel_names,
            )
            .await?;

        let mut directory_rules = step.conf.directory_rules.clone();
        if let Some(channels_dir) = shared_channels_dir {
            directory_rules.push(DirectoryRule {
                inside_path: PathBuf::from("/channels"),
                outside_path: Some(channels_dir.to_path_buf()),
                options: DirectoryOptions {
                    read_write: true,
                    ..Default::default()
                },
            });
        }
        for mount in &step.mounts {
            match &mount.source {
                // StepOutput is a single-file handoff: copy it into this box's
                // working dir (isolate cannot bind-mount a file, and the
                // producer dir holds the contestant's source - never mount it).
                MountSource::StepOutput { from_step, file } => {
                    let src = resolve_step_output_src(
                        from_step,
                        file,
                        step_working_dirs,
                        &step.depends_on,
                    )
                    .await?;
                    stage_step_output_file(&src, &mount.inside_path, &env.working_dir).await?;
                }
                // PlatformTool is a read-only directory mount (the tools dir at
                // the parent of inside_path) - see platform_tool_directory_rule.
                MountSource::PlatformTool { name } => {
                    let tools_dir = self.tools_dir.as_deref().ok_or_else(|| {
                        anyhow!(
                            "step requests platform tool '{name}' but no [worker.tools] dir is configured"
                        )
                    })?;
                    directory_rules.push(platform_tool_directory_rule(
                        &mount.inside_path,
                        name,
                        tools_dir,
                    )?);
                }
            }
        }

        let run_opts = RunOptions {
            resource_limits: step.conf.resource_limits.clone(),
            wait: true,
            as_uid: None,
            as_gid: None,
            stdin: stdin_path,
            stdout: stdout_path,
            stderr: stderr_path,
            env_rules: step.conf.env_rules.clone(),
            directory_rules,
        };

        let exec_result = match self
            .sandbox_manager
            .execute(env.box_id.as_str(), step.argv.clone(), &run_opts)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                // The sandbox failed to run the step AT ALL (fork/spawn EAGAIN,
                // isolate init failure, ...). This is an infrastructure fault, never
                // the contestant's output, so tag it InternalError: precheck_verdict
                // / interpret_fused_result then classify it as a self-healing
                // SystemError instead of letting a default "UNKNOWN" fall through to
                // the checker, which -- now that the channel keep-alive delivers EOF
                // -- would score the empty channel as a terminal WrongAnswer that
                // never self-heals. Malformed-operation errors fail earlier, in
                // prepare_io, and deliberately never reach this arm.
                error!(step_id = %step.id, error = %e, "Step execution failed");
                return Ok(TaskExecutionResult {
                    task_id: step.id.clone(),
                    success: false,
                    sandbox_result: ExecutionResult {
                        sandbox_status: SandboxStatus::InternalError,
                        status: "XX".to_string(),
                        message: format!(
                            "step '{}' failed to execute (sandbox error): {e}",
                            step.id
                        ),
                        ..ExecutionResult::default()
                    },
                    collected_outputs: HashMap::new(),
                });
            }
        };

        let success = exec_result.exit_code == Some(0);
        let collected_outputs = match self
            .collect_output(&env.working_dir, step, &exec_result)
            .await
        {
            Ok(outputs) => outputs,
            Err(e) => {
                // The step already ran to a clean exit; the failure here is
                // POST-sandbox output collection (a blob-store upload blip, a
                // transient stat/canonicalize error). That is an infrastructure
                // fault, never the contestant's output, so tag it InternalError so
                // precheck_verdict / interpret_fused_result route it to a
                // self-healing SystemError -- exactly like the sandbox-execute error
                // arm above. Propagating `?` here instead would surface as an `Err`
                // to `execute_step_with_deps`, whose catch arm substitutes a terminal
                // `ExecutionResult::default()` ("UNKNOWN"): a transient upload blip
                // would then finalize a permanent wrong verdict that never resolves
                // on retry. Malformed-operation errors fail earlier, in prepare_io,
                // and never reach this arm.
                error!(step_id = %step.id, error = %e, "Step output collection failed");
                return Ok(TaskExecutionResult {
                    task_id: step.id.clone(),
                    success: false,
                    sandbox_result: ExecutionResult {
                        sandbox_status: SandboxStatus::InternalError,
                        status: "XX".to_string(),
                        message: format!(
                            "step '{}' output collection failed (infra error): {e}",
                            step.id
                        ),
                        ..ExecutionResult::default()
                    },
                    collected_outputs: HashMap::new(),
                });
            }
        };

        Ok(TaskExecutionResult {
            task_id: step.id.clone(),
            success,
            sandbox_result: exec_result,
            collected_outputs,
        })
    }
}

/// A producer's keep-alive writer on one channel FIFO, plus the metadata the
/// release routine needs to close it at the right moment. See
/// `release_channel_keepalive`.
struct KeepAlive {
    /// Path of the channel FIFO this keep-alive holds open.
    fifo_path: PathBuf,
    /// The O_RDWR keep-alive fd -- a writer, so a consumer's `open(O_RDONLY)`
    /// rendezvous never blocks. The release loop drops and re-opens it in place.
    file: std::fs::File,
    /// Set once the consumer step has finished, ending the release. `None`
    /// when the channel has no consumer we can detect from `IOConfig` (an
    /// argv-opened reader); the release then happens right after the first
    /// drop.
    consumer_done: Option<Arc<ConsumerDone>>,
}

/// Set once a channel's consumer step has finished. Release routines wait on
/// it instead of polling, so a finished consumer releases its producer's
/// keep-alive at once.
#[derive(Default)]
struct ConsumerDone {
    flag: AtomicBool,
    notify: tokio::sync::Notify,
}

impl ConsumerDone {
    fn set(&self) {
        self.flag.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    fn is_set(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Wait until the consumer is done or `timeout` elapses; returns whether
    /// it is done.
    async fn wait(&self, timeout: Duration) -> bool {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        // Register before checking the flag, so a `set` in between is seen.
        notified.as_mut().enable();
        if self.is_set() {
            return true;
        }
        let _ = tokio::time::timeout(timeout, notified).await;
        self.is_set()
    }
}

/// Channels this step *produces* (writes): any output redirect (stdout/stderr)
/// bound to a `Pipe` whose name is a declared operation channel. The consumer
/// side (a `Pipe` on `stdin`) is deliberately excluded -- only a producer's
/// write end needs a keep-alive.
fn step_produced_channels(step: &Step, channel_names: &HashSet<String>) -> Vec<String> {
    let mut produced = Vec::new();
    for target in [&step.io.stdout, &step.io.stderr] {
        if let IOTarget::Pipe { name } = target
            && channel_names.contains(name)
        {
            produced.push(name.clone());
        }
    }
    produced
}

/// Channels this step *consumes* (reads): a `Pipe` on `stdin` whose name is a
/// declared operation channel. Mirror of `step_produced_channels`; used to map a
/// channel to the consumer whose completion releases the producer's keep-alive.
fn step_consumed_channels(step: &Step, channel_names: &HashSet<String>) -> Vec<String> {
    let mut consumed = Vec::new();
    if let IOTarget::Pipe { name } = &step.io.stdin
        && channel_names.contains(name)
    {
        consumed.push(name.clone());
    }
    consumed
}

/// Resolve each channel to its producer/consumer step id for keep-alive wiring.
///
/// Two sources, in priority order:
///  1. **stdio-pipe scan** -- a step whose stdout/stderr (producer) or stdin
///     (consumer) is an `IOTarget::Pipe` on a declared channel. Authoritative
///     where present (the `redirect`-mode contestant is detected this way).
///  2. **explicit `Channel.producer_step`/`consumer_step`** -- fills only the
///     gaps the scan left. A step that opens a FIFO via a *raw argv path* (the
///     communication-evaluator manager on every channel, and the contestant in
///     `fifo_args` mode) is not a `Pipe` on any stdio slot, so the scan cannot
///     see it. Without this, such argv-opened endpoints get no keep-alive and a
///     non-participating peer wedges until its isolate wall-time (up to the
///     manager's 150s wall -> a slot-exhaustion vector).
///
/// `or_insert` keeps the scan authoritative where both agree; the explicit
/// declaration only supplies what the scan could not observe.
fn resolve_channel_roles(
    tasks: &[Step],
    channels: &[Channel],
    channel_names: &HashSet<String>,
) -> (HashMap<String, String>, HashMap<String, String>) {
    let mut producer_of: HashMap<String, String> = HashMap::new();
    let mut consumer_of: HashMap<String, String> = HashMap::new();
    for task in tasks {
        for ch in step_produced_channels(task, channel_names) {
            // First writer wins; a channel has a single logical producer.
            producer_of.entry(ch).or_insert_with(|| task.id.clone());
        }
        for ch in step_consumed_channels(task, channel_names) {
            // First reader wins; the fused pipeline has a single consumer.
            consumer_of.entry(ch).or_insert_with(|| task.id.clone());
        }
    }
    for channel in channels {
        if let Some(producer) = &channel.producer_step {
            producer_of
                .entry(channel.name.clone())
                .or_insert_with(|| producer.clone());
        }
        if let Some(consumer) = &channel.consumer_step {
            consumer_of
                .entry(channel.name.clone())
                .or_insert_with(|| consumer.clone());
        }
    }
    (producer_of, consumer_of)
}

/// Maps a box-local pipe, keyed by `(env_ref, pipe_name)`, to a step id (its
/// producer or its consumer). See [`resolve_local_pipe_roles`].
type LocalPipeRoles = HashMap<(String, String), String>;

/// Resolve each box-local (non-channel) pipe to its producer/consumer step id.
///
/// A `Pipe` whose name is NOT a declared channel is a per-environment FIFO at
/// `<env working_dir>/pipes/<name>` (see `prepare_io_target`). Two steps share
/// such a FIFO only when they name the same pipe in the *same* environment, so
/// the map is keyed by `(env_ref, pipe_name)`. Mirror of `resolve_channel_roles`,
/// minus the explicit `Channel` declarations (box-local pipes have none).
fn resolve_local_pipe_roles(
    tasks: &[Step],
    channel_names: &HashSet<String>,
) -> (LocalPipeRoles, LocalPipeRoles) {
    let mut producer_of: LocalPipeRoles = HashMap::new();
    let mut consumer_of: LocalPipeRoles = HashMap::new();
    for task in tasks {
        for target in [&task.io.stdout, &task.io.stderr] {
            if let IOTarget::Pipe { name } = target
                && !channel_names.contains(name)
            {
                producer_of
                    .entry((task.env_ref.clone(), name.clone()))
                    .or_insert_with(|| task.id.clone());
            }
        }
        if let IOTarget::Pipe { name } = &task.io.stdin
            && !channel_names.contains(name)
        {
            consumer_of
                .entry((task.env_ref.clone(), name.clone()))
                .or_insert_with(|| task.id.clone());
        }
    }
    (producer_of, consumer_of)
}

/// True if `path` exists and is a FIFO. Lets the keep-alive skip re-`mkfifo`ing a
/// pipe `prepare_io_target` already made (a benign create race) and confirm the
/// target is a FIFO before opening its O_RDWR write end.
fn path_is_fifo(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_fifo())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Release all of a producer step's channel keep-alives CONCURRENTLY.
///
/// A single producer may own several channels -- the communication-evaluator
/// manager (`num_processes >= 2`) writes `m_to_c0..m_to_c{n-1}`, one per
/// contestant process. Each `release_channel_keepalive` legitimately waits for
/// ITS channel's consumer to finish (see that fn), so releasing them one-by-one
/// would head-of-line block: a slow consumer on the first channel would delay EOF
/// delivery to every later channel's consumer, and a correct-but-EOF-dependent
/// peer already blocked in its final `read()` could blow its wall-time -> a
/// spurious TLE. The releases are independent (distinct FIFOs, distinct
/// done-flags), so join them and let each converge on its own consumer's flag.
async fn release_channel_keepalives(keepalives: Vec<KeepAlive>) {
    join_all(keepalives.into_iter().map(release_channel_keepalive)).await;
}

/// Runaway guard for `release_channel_keepalive`'s writer-restore loop. NOT a
/// functional timeout -- the consumer-done flag is the real bound; this only caps
/// a pathological consumer that neither opens nor finishes. Must exceed the
/// consumer isolate's max lifetime (`worker_hard_timeout`, ~630s for a 600s-wall
/// checker) so a live-but-slow consumer is never pre-empted (the original 120s
/// ceiling did not, and wedged the FIFO until the isolate wall-time). See
/// `release_channel_keepalive`.
const CHANNEL_RELEASE_MAX_WAIT: Duration = Duration::from_secs(900);
/// How often buffered producer output is re-checked while the consumer drains
/// it (the consumer finishing ends the wait at once).
const CHANNEL_RELEASE_STEP: Duration = Duration::from_millis(20);
/// First and longest time the writer stays dropped while waiting for the
/// consumer to finish. The window doubles each round, so a reader blocked in
/// `read()` sees EOF however late the scheduler runs it.
const CHANNEL_RELEASE_FIRST_WINDOW: Duration = Duration::from_millis(20);
const CHANNEL_RELEASE_MAX_WINDOW: Duration = Duration::from_secs(1);

/// True iff the FIFO behind `file` has bytes queued for reading right now.
///
/// `file` is the O_RDWR keep-alive fd, so it is itself a writer -> the pipe can
/// never report POLLHUP here (writers != 0). POLLIN therefore means *real
/// buffered producer output*, never an end-of-stream artifact. A zero timeout
/// makes this a non-blocking readiness snapshot.
#[cfg(unix)]
fn fifo_has_buffered_input(file: &std::fs::File) -> bool {
    use std::os::unix::io::AsRawFd;

    let mut pfd = libc::pollfd {
        fd: file.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // 0ms timeout: never blocks. On any poll error report "no data" so release
    // proceeds exactly as before -- we never hold the FIFO open for bytes we
    // cannot confirm are actually queued.
    let rc = unsafe { libc::poll(&mut pfd, 1, 0) };
    rc > 0 && (pfd.revents & libc::POLLIN) != 0
}

/// Release a producer's channel keep-alive, delivering EOF to the consumer only
/// after the consumer has actually opened the FIFO.
///
/// Dropping the keep-alive the instant the producer resolves races the
/// consumer: under fork/exec (EAGAIN) pressure the checker's isolate can reach
/// its `open(/channels/<name>, O_RDONLY)` after a failed or fast producer has
/// resolved, so with no writer left that open blocks until the isolate
/// wall-time.
///
/// Two FIFO facts make EOF delivery level-triggered instead:
///   * a reader blocked in `read()` returns EOF whenever it next runs with no
///     writer present, so the writer is kept away for a whole window, not for
///     an instant;
///   * a reader blocked in `open(O_RDONLY)` waits for a writer to *open* (the
///     kernel counts writer opens), so briefly re-opening the writer lets it
///     through, and dropping it again right after leaves its first `read()` an
///     EOF.
///
/// Each round drops the writer, then waits on the consumer's done signal for a
/// window that doubles up to `CHANNEL_RELEASE_MAX_WINDOW`. A finished consumer
/// ends the release at once; otherwise the writer is re-opened for a late
/// opener and the next round drops it again. Buffered producer output is never
/// discarded: while bytes are queued the keep-alive stays open for the
/// consumer to drain them.
///   * a channel with an **identified consumer** (`consumer_done: Some`, from
///     the stdio-pipe scan OR an explicit `consumer_step`) releases once that
///     consumer is done;
///   * a channel with **no detectable consumer** (`consumer_done: None`, a
///     genuine orphan) releases right after the first drop;
///   * either kind releases at once if the FIFO path has vanished (operation
///     cleanup removed the channels dir).
///
/// The consumer's done signal, not a reader probe, decides release: a probe
/// cannot tell our consumer from a forked child that inherited a dup of the
/// keep-alive fd before its `execve`. `CHANNEL_RELEASE_MAX_WAIT` is only a
/// runaway guard, set above the consumer isolate's maximum lifetime
/// (`worker_hard_timeout`) so a live-but-slow consumer is never abandoned.
async fn release_channel_keepalive(keepalive: KeepAlive) {
    let KeepAlive {
        fifo_path,
        file,
        consumer_done,
    } = keepalive;
    // Held writer; `None` means dropped. Re-opened in place on the wait path.
    let mut held = Some(file);
    let mut waited = Duration::ZERO;
    let mut window = CHANNEL_RELEASE_FIRST_WINDOW;

    loop {
        // Preserve the producer's buffered output. If the producer already wrote
        // its bytes and exited but the consumer has not opened yet -- common for a
        // small output that fits the pipe buffer -- dropping this O_RDWR fd would
        // take readers to zero and the kernel would DISCARD those bytes, and the
        // late-opening consumer would read empty input. While bytes are queued,
        // keep this fd and let the consumer drain them first; its writer side
        // keeps the consumer's open() rendezvous unblocked throughout. Only an
        // identified consumer is waited for; a genuine orphan releases at once.
        if let Some(f) = held.as_ref()
            && let Some(done) = consumer_done.as_ref()
            && waited < CHANNEL_RELEASE_MAX_WAIT
            && fifo_has_buffered_input(f)
        {
            if done.wait(CHANNEL_RELEASE_STEP).await {
                return;
            }
            waited += CHANNEL_RELEASE_STEP;
            continue;
        }

        // Drop our writer. A reader blocked in read() returns EOF once it runs
        // with no writer left, so the writer stays away for the whole window
        // below rather than being restored at once.
        drop(held.take());

        // We deliberately do NOT probe for a reader to decide release:
        // `open(O_WRONLY|O_NONBLOCK)` reports whether *any* reader exists, and a
        // concurrent isolate `fork` inherits a dup of this keep-alive's O_RDWR fd
        // (O_CLOEXEC only closes it at the child's `execve`), so the probe can
        // count our own lineage. The consumer's done signal is the only
        // authoritative one. A genuine orphan has none: the drop above already
        // delivered EOF to any blocked reader, so release now.
        let Some(done) = consumer_done.as_ref() else {
            return;
        };
        // The FIFO is gone (operation cleanup removed the channels dir): nothing
        // is left to serve. A plain stat cannot be fooled by an inherited dup.
        if done.wait(window).await || !fifo_path.exists() || waited >= CHANNEL_RELEASE_MAX_WAIT {
            return;
        }
        waited += window;
        window = (window * 2).min(CHANNEL_RELEASE_MAX_WINDOW);

        // Consumer still running and possibly not opened yet. A FIFO reader
        // blocked in open() waits for a writer to *open* (the kernel's
        // writer-open counter), not for one to stay open, so re-opening the
        // writer lets it through; the next iteration drops it again at once and
        // the consumer's first read() sees EOF. A transient re-open failure just
        // leaves `held` empty for this round.
        held = open_channel_keepalive(&fifo_path);
    }
}

/// Open a keep-alive fd on a channel FIFO. `O_RDWR` is deliberate: unlike
/// `O_RDONLY`/`O_WRONLY`, opening a FIFO read-write never blocks on the
/// open-rendezvous, so holding this fd guarantees a concurrent consumer's
/// `open(O_RDONLY)` returns immediately. Dropping it later delivers EOF.
/// A failure here is non-fatal: the consumer simply reverts to relying on the
/// worker hard-timeout backstop, so we warn and continue rather than abort the
/// whole operation.
fn open_channel_keepalive(fifo_path: &Path) -> Option<std::fs::File> {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fifo_path)
    {
        Ok(file) => Some(file),
        Err(e) => {
            warn!(
                path = %fifo_path.display(),
                error = %e,
                "Failed to open channel keep-alive fd; consumer relies on the worker hard timeout"
            );
            None
        }
    }
}

/// Create each channel's FIFO in `dir`, readable and writable by the sandbox
/// users.
async fn create_channel_fifos(dir: &Path, channels: &[Channel]) -> Result<()> {
    for channel in channels {
        let fifo_path = dir.join(&channel.name);
        create_fifo(&fifo_path)
            .with_context(|| format!("mkfifo failed for channel {}", channel.name))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) =
                std::fs::set_permissions(&fifo_path, std::fs::Permissions::from_mode(0o666))
            {
                warn!(path = %fifo_path.display(), error = %e, "Failed to set permissions on channel FIFO");
            }
        }
        debug!(channel = %channel.name, "Created shared channel FIFO");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o777)) {
            warn!(path = %dir.display(), error = %e, "Failed to set permissions on channels directory");
        }
    }
    Ok(())
}

#[cfg(test)]
mod channel_keepalive_tests {
    use super::*;
    use broccoli_types::types::{IOConfig, StepKind};

    fn step_with_io(id: &str, io: IOConfig) -> Step {
        Step {
            id: id.to_string(),
            kind: StepKind::default(),
            env_ref: "env".to_string(),
            argv: vec![],
            conf: RunOptions::default(),
            io,
            collect: vec![],
            depends_on: vec![],
            cache: None,
            mounts: vec![],
        }
    }

    #[test]
    fn produced_channels_counts_stdout_pipe_only() {
        let channels: HashSet<String> = ["sol_out".to_string()].into_iter().collect();

        // stdout Pipe on a declared channel -> produced.
        let producer = step_with_io(
            "exec",
            IOConfig {
                stdout: IOTarget::Pipe {
                    name: "sol_out".to_string(),
                },
                ..Default::default()
            },
        );
        assert_eq!(
            step_produced_channels(&producer, &channels),
            vec!["sol_out".to_string()]
        );

        // stdin Pipe (the consumer side) is NOT a producer.
        let consumer = step_with_io(
            "check",
            IOConfig {
                stdin: IOTarget::Pipe {
                    name: "sol_out".to_string(),
                },
                ..Default::default()
            },
        );
        assert!(step_produced_channels(&consumer, &channels).is_empty());

        // A Pipe that is not a declared channel is a box-local pipe, not a channel.
        let box_pipe = step_with_io(
            "other",
            IOConfig {
                stdout: IOTarget::Pipe {
                    name: "not_a_channel".to_string(),
                },
                ..Default::default()
            },
        );
        assert!(step_produced_channels(&box_pipe, &channels).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn keepalive_unblocks_reader_open_and_missing_path_is_none() {
        // A path that does not exist -> None (non-fatal).
        assert!(
            open_channel_keepalive(Path::new("/nonexistent/broccoli/keepalive/fifo")).is_none()
        );

        // Create a real FIFO and prove the keep-alive writer lets an O_RDONLY
        // open return immediately instead of blocking on the FIFO rendezvous.
        let dir = std::env::temp_dir().join(format!("broccoli-ka-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("sol_out");
        create_fifo(&fifo).expect("mkfifo");

        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");

        let reader_path = fifo.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            // Without a writer present this open would block forever.
            let opened = std::fs::OpenOptions::new().read(true).open(&reader_path);
            let _ = tx.send(opened.is_ok());
        });

        let unblocked = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("O_RDONLY open blocked despite keep-alive writer");
        assert!(unblocked, "reader failed to open the FIFO");

        drop(keepalive);
        reader.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn consumed_channels_counts_stdin_pipe_only() {
        let channels: HashSet<String> = ["sol_out".to_string()].into_iter().collect();

        // stdin Pipe on a declared channel -> consumed.
        let consumer = step_with_io(
            "check",
            IOConfig {
                stdin: IOTarget::Pipe {
                    name: "sol_out".to_string(),
                },
                ..Default::default()
            },
        );
        assert_eq!(
            step_consumed_channels(&consumer, &channels),
            vec!["sol_out".to_string()]
        );

        // stdout Pipe (the producer side) is NOT a consumer.
        let producer = step_with_io(
            "exec",
            IOConfig {
                stdout: IOTarget::Pipe {
                    name: "sol_out".to_string(),
                },
                ..Default::default()
            },
        );
        assert!(step_consumed_channels(&producer, &channels).is_empty());

        // A stdin Pipe that is not a declared channel is a box-local pipe.
        let box_pipe = step_with_io(
            "other",
            IOConfig {
                stdin: IOTarget::Pipe {
                    name: "not_a_channel".to_string(),
                },
                ..Default::default()
            },
        );
        assert!(step_consumed_channels(&box_pipe, &channels).is_empty());
    }

    #[test]
    fn resolve_channel_roles_merges_scan_and_explicit_declarations() {
        // Two channels. `piped` is wired via stdio pipes (redirect-style, visible
        // to the scan). `argv` is opened only via raw argv paths on both ends
        // (fifo_args-style, invisible to the scan) and relies on the explicit
        // producer_step/consumer_step declarations.
        let channel_names: HashSet<String> = ["piped".to_string(), "argv".to_string()]
            .into_iter()
            .collect();

        let producer = step_with_io(
            "prod",
            IOConfig {
                stdout: IOTarget::Pipe {
                    name: "piped".to_string(),
                },
                ..Default::default()
            },
        );
        let consumer = step_with_io(
            "cons",
            IOConfig {
                stdin: IOTarget::Pipe {
                    name: "piped".to_string(),
                },
                ..Default::default()
            },
        );
        // The manager opens `argv` via argv only -> no Pipe on any stdio slot.
        let manager = step_with_io("mgr", IOConfig::default());

        let channels = vec![
            // Scan already resolves `piped`; an explicit (wrong) declaration must
            // NOT override the authoritative scan result.
            Channel {
                name: "piped".to_string(),
                buffer_size: Some(8192),
                producer_step: Some("someone_else".to_string()),
                consumer_step: Some("someone_else".to_string()),
            },
            // `argv` is resolvable ONLY through the explicit declaration.
            Channel {
                name: "argv".to_string(),
                buffer_size: Some(8192),
                producer_step: Some("mgr".to_string()),
                consumer_step: Some("cons".to_string()),
            },
        ];

        let (producer_of, consumer_of) =
            resolve_channel_roles(&[producer, consumer, manager], &channels, &channel_names);

        // Scan is authoritative for `piped` despite the conflicting declaration.
        assert_eq!(producer_of.get("piped"), Some(&"prod".to_string()));
        assert_eq!(consumer_of.get("piped"), Some(&"cons".to_string()));
        // `argv` is filled entirely from the explicit declaration.
        assert_eq!(producer_of.get("argv"), Some(&"mgr".to_string()));
        assert_eq!(consumer_of.get("argv"), Some(&"cons".to_string()));
    }

    #[cfg(unix)]
    fn make_fifo(suffix: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "broccoli-release-test-{}-{}",
            std::process::id(),
            suffix
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("sol_out");
        create_fifo(&fifo).expect("mkfifo");
        (dir, fifo)
    }

    // The bug case: the keep-alive is handed to `release_channel_keepalive` and
    // the consumer opens the FIFO LATE (after the producer already resolved).
    // The release routine must keep a writer alive until the late open, then
    // deliver EOF -- proving no permanent openat hang.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_unblocks_a_late_opening_consumer_with_eof() {
        use std::io::Read;

        let (dir, fifo) = make_fifo("late-open");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");
        let consumer_done = Arc::new(ConsumerDone::default());

        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let reader_path = fifo.clone();
        let done = consumer_done.clone();
        let reader = std::thread::spawn(move || {
            // Opens only once the release is under way (signalled below).
            go_rx.recv().unwrap();
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .open(&reader_path)
                .expect("late O_RDONLY open must eventually rendezvous");
            let mut buf = Vec::new();
            let n = f.read_to_end(&mut buf).expect("read to EOF");
            done.set();
            (n, buf)
        });

        let release = tokio::spawn(release_channel_keepalive(KeepAlive {
            fifo_path: fifo.clone(),
            file: keepalive,
            consumer_done: Some(consumer_done.clone()),
        }));
        go_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), release)
            .await
            .expect("release must return, not hang")
            .unwrap();

        let (n, buf) = reader.join().unwrap();
        assert_eq!(n, 0, "consumer must see clean EOF (empty), got {buf:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The data-loss counterpart to the late-open EOF test: the producer writes its
    // output and exits BEFORE the consumer opens -- a small payload that fits the
    // pipe buffer, so the producer never needs a reader to drain and can finish
    // first. Releasing must NOT discard those buffered bytes: the late-opening
    // consumer must read exactly the producer's output, then EOF. Dropping the
    // O_RDWR keep-alive while bytes are still queued takes readers to zero and the
    // kernel discards the buffer -- the root cause of the burst false-WrongAnswers
    // (a fast AC solution's output vanishing into an empty comparison).
    #[cfg(unix)]
    #[tokio::test]
    async fn release_preserves_buffered_output_for_a_late_opening_consumer() {
        use std::io::{Read, Write};

        let (dir, fifo) = make_fifo("late-open-data");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");

        // Producer writes then closes while the keep-alive is the only reader: the
        // bytes sit in the pipe buffer. Models the fast solution isolate finishing
        // before the slow checker isolate reaches its stdin open under load.
        {
            let mut w = std::fs::OpenOptions::new()
                .write(true)
                .open(&fifo)
                .expect("producer O_WRONLY open rendezvous with keep-alive reader");
            w.write_all(b"42\n").expect("producer write");
        } // producer closed

        let consumer_done = Arc::new(ConsumerDone::default());
        let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
        let reader_path = fifo.clone();
        let done = consumer_done.clone();
        let reader = std::thread::spawn(move || {
            // Opens only once the release is under way, after the producer
            // already wrote and exited.
            go_rx.recv().unwrap();
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .open(&reader_path)
                .expect("late O_RDONLY open must rendezvous");
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).expect("read to EOF");
            done.set();
            buf
        });

        let release = tokio::spawn(release_channel_keepalive(KeepAlive {
            fifo_path: fifo.clone(),
            file: keepalive,
            consumer_done: Some(consumer_done.clone()),
        }));
        go_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), release)
            .await
            .expect("release must return, not hang")
            .unwrap();

        let buf = reader.join().unwrap();
        assert_eq!(
            buf, b"42\n",
            "consumer must receive the producer's buffered bytes, not empty (got {buf:?})"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The common case: the consumer has already rendezvoused (via the keep-alive
    // writer) and is blocked in read(). Releasing must deliver EOF by dropping the
    // writer -- the probe must not re-block it.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_delivers_eof_to_a_reader_blocked_in_read() {
        use std::io::Read;

        let (dir, fifo) = make_fifo("blocked-read");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");
        let (opened_tx, opened_rx) = std::sync::mpsc::channel::<()>();

        let reader_path = fifo.clone();
        let reader = std::thread::spawn(move || {
            // Opens immediately (rendezvous with the keep-alive writer), then
            // blocks in read() until the writer goes away.
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .open(&reader_path)
                .expect("O_RDONLY open rendezvous with keep-alive");
            opened_tx.send(()).unwrap();
            let mut buf = Vec::new();
            let n = f.read_to_end(&mut buf).expect("read to EOF");
            (n, buf)
        });
        // The reader holds the FIFO open before the release starts.
        opened_rx.recv().unwrap();

        tokio::time::timeout(
            Duration::from_secs(10),
            release_channel_keepalive(KeepAlive {
                fifo_path: fifo.clone(),
                file: keepalive,
                consumer_done: None,
            }),
        )
        .await
        .expect("release must return, not hang");

        let (n, buf) = reader.join().unwrap();
        assert_eq!(n, 0, "reader must see EOF (empty), got {buf:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // An identified consumer already blocked in read() gets EOF from the first
    // drop, finishes, and its done signal ends the release.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_ends_once_a_reading_consumer_finishes() {
        use std::io::Read;

        let (dir, fifo) = make_fifo("reading-consumer");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");
        let consumer_done = Arc::new(ConsumerDone::default());
        let (opened_tx, opened_rx) = std::sync::mpsc::channel::<()>();

        let reader_path = fifo.clone();
        let done = consumer_done.clone();
        let reader = std::thread::spawn(move || {
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .open(&reader_path)
                .expect("O_RDONLY open rendezvous with keep-alive");
            opened_tx.send(()).unwrap();
            let mut buf = Vec::new();
            let n = f.read_to_end(&mut buf).expect("read to EOF");
            done.set();
            n
        });
        // The consumer holds the FIFO open before the release starts.
        opened_rx.recv().unwrap();

        tokio::time::timeout(
            Duration::from_secs(10),
            release_channel_keepalive(KeepAlive {
                fifo_path: fifo.clone(),
                file: keepalive,
                consumer_done: Some(consumer_done.clone()),
            }),
        )
        .await
        .expect("release must end once the consumer is done");

        assert_eq!(reader.join().unwrap(), 0, "consumer must see clean EOF");
        assert!(consumer_done.is_set());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // When the consumer has already finished (its `done` flag is set) and there is
    // no reader to unblock, release must return promptly instead of spinning to
    // the runaway ceiling.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_returns_promptly_when_consumer_already_done() {
        let (dir, fifo) = make_fifo("already-done");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");
        let consumer_done = Arc::new(ConsumerDone::default());
        consumer_done.set();

        tokio::time::timeout(
            Duration::from_secs(2),
            release_channel_keepalive(KeepAlive {
                fifo_path: fifo.clone(),
                file: keepalive,
                consumer_done: Some(consumer_done),
            }),
        )
        .await
        .expect("release must not spin to the fallback when the consumer is done");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Regression guard: a channel with no IOConfig-identifiable consumer
    // (`consumer_done: None`, e.g. the communication-evaluator manager that opens
    // the FIFO via a raw argv path) and no reader currently present must release
    // PROMPTLY -- it must NOT run the restore-writer retry loop and stall the whole
    // layer up to CHANNEL_RELEASE_MAX_WAIT. Bounding the await at 2s proves no spin.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_returns_promptly_when_no_identifiable_consumer() {
        let (dir, fifo) = make_fifo("no-consumer");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");

        tokio::time::timeout(
            Duration::from_secs(2),
            release_channel_keepalive(KeepAlive {
                fifo_path: fifo.clone(),
                file: keepalive,
                consumer_done: None,
            }),
        )
        .await
        .expect("release must not spin when there is no detectable consumer to wait for");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Regression guard for the burst-wedge root cause: the writer-restore ceiling
    // is a runaway guard, NOT a functional timeout, so it MUST sit above the
    // consumer isolate's maximum lifetime. Otherwise a live-but-slow consumer
    // (its isolate reaches `open()` late under fork/exec pressure) is abandoned
    // mid-flight and the FIFO wedges until the 600s isolate wall-time. The worst
    // case consumer is the checker: --wall-time 600s + --extra-time + the worker
    // hard-timeout margin. The original 120s ceiling failed this; assert a floor
    // safely above the checker lifetime.
    #[test]
    fn release_ceiling_exceeds_consumer_isolate_lifetime() {
        assert!(
            CHANNEL_RELEASE_MAX_WAIT >= Duration::from_secs(660),
            "release ceiling {CHANNEL_RELEASE_MAX_WAIT:?} must exceed the consumer isolate lifetime"
        );
    }

    // The retry loop keeps a live consumer (`Some(flag)` unset) served across
    // transient probe hiccups, but a truly-removed FIFO path (operation cleanup)
    // is terminal: its probe open returns ENOENT and the consumer's own open would
    // fail too, so release must stop promptly rather than spin to the runaway
    // ceiling. Removing the dir out from under the loop drives exactly that ENOENT.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_returns_when_fifo_path_removed_even_if_consumer_live() {
        let (dir, fifo) = make_fifo("path-removed");
        let keepalive = open_channel_keepalive(&fifo).expect("keep-alive opens O_RDWR");
        // Consumer still "live" (flag unset): only the missing path may end this.
        let consumer_done = Arc::new(ConsumerDone::default());
        std::fs::remove_dir_all(&dir).unwrap();

        tokio::time::timeout(
            Duration::from_secs(2),
            release_channel_keepalive(KeepAlive {
                fifo_path: fifo.clone(),
                file: keepalive,
                consumer_done: Some(consumer_done),
            }),
        )
        .await
        .expect("release must return promptly when the FIFO path is gone (ENOENT)");
    }

    // Head-of-line-blocking regression: a producer step that owns MULTIPLE
    // channels must release them concurrently, not one-after-another. This is the
    // communication-evaluator `num_processes >= 2` topology -- `run_manager`
    // produces `m_to_c0..m_to_c{n-1}`, each read by a DIFFERENT contestant. Each
    // per-channel release legitimately waits for its own consumer's done-flag, so
    // a sequential release lets a slow consumer on the first channel delay EOF
    // delivery to every later channel's consumer. A correct-but-EOF-dependent peer
    // process, already blocked in its final read(), would then sit idle behind the
    // slow one and can blow its wall-time -> spurious TLE. Here channel A's
    // consumer never finishes within the assertion window while channel B's
    // consumer is already blocked in read(); B MUST receive EOF promptly, proving
    // its release is not queued behind A.
    #[cfg(unix)]
    #[tokio::test]
    async fn release_keepalives_do_not_head_of_line_block() {
        use std::io::Read;

        let (dir_a, fifo_a) = make_fifo("hol-slow");
        let (dir_b, fifo_b) = make_fifo("hol-ready");
        let ka_a = open_channel_keepalive(&fifo_a).expect("A keep-alive opens O_RDWR");
        let ka_b = open_channel_keepalive(&fifo_b).expect("B keep-alive opens O_RDWR");

        // A's consumer is slow: its flag stays unset (and no reader opens A), so
        // `release_channel_keepalive(A)` alone would spin re-opening until the flag
        // flips. B's consumer is ready NOW, blocked in read() awaiting EOF.
        let flag_a = Arc::new(ConsumerDone::default());
        let flag_b = Arc::new(ConsumerDone::default());

        let b_path = fifo_b.clone();
        let b_done = flag_b.clone();
        let b_reader = std::thread::spawn(move || {
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .open(&b_path)
                .expect("B O_RDONLY rendezvous with keep-alive");
            let mut buf = Vec::new();
            let n = f.read_to_end(&mut buf).expect("B read to EOF");
            b_done.set();
            n
        });
        // Release A (slow) FIRST, then B (ready), together. A sequential release
        // would block on A and never reach B within the window.
        let releaser = tokio::spawn(release_channel_keepalives(vec![
            KeepAlive {
                fifo_path: fifo_a.clone(),
                file: ka_a,
                consumer_done: Some(flag_a.clone()),
            },
            KeepAlive {
                fifo_path: fifo_b.clone(),
                file: ka_b,
                consumer_done: Some(flag_b.clone()),
            },
        ]));

        // B must get EOF and set its flag promptly, even though A is still pending.
        assert!(
            flag_b.wait(Duration::from_secs(10)).await,
            "B's consumer must receive EOF without waiting behind slow channel A"
        );

        // Let A finish so the releaser returns and nothing leaks.
        flag_a.set();
        tokio::time::timeout(Duration::from_secs(5), releaser)
            .await
            .expect("releaser returns once A's consumer is done")
            .expect("release task did not panic");

        assert_eq!(b_reader.join().unwrap(), 0, "B must have seen clean EOF");
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }
}

// Verdict-routing regression tests for the post-sandbox output-collection stage.
#[cfg(all(test, unix))]
mod step_execution_tests {
    use super::*;
    use crate::models::operation::file_cacher::UnavailableFileCacher;
    use crate::models::operation::sandbox::mock::MockSandboxManager;
    use crate::models::operation::task_cache::NoopTaskCacheStore;
    use broccoli_types::types::{IOConfig, StepKind};

    fn collecting_step(id: &str, argv: Vec<String>, collect: Vec<String>) -> Step {
        Step {
            id: id.to_string(),
            kind: StepKind::default(),
            env_ref: "env".to_string(),
            argv,
            conf: RunOptions::default(),
            io: IOConfig::default(),
            collect,
            depends_on: vec![],
            cache: None,
            mounts: vec![],
        }
    }

    // A post-sandbox output-collection failure (a blob-store upload error, after
    // the step already ran to a CLEAN exit) is an INFRASTRUCTURE fault, not a
    // contestant determination. It must surface through `execute_step` as a
    // self-healing InternalError ("XX") -- the same tag the sandbox-execute error
    // arm uses -- NOT as an `Err`. An `Err` here is caught by the caller's arm in
    // `execute_step_with_deps`, which substitutes a terminal `ExecutionResult::default()`
    // ("UNKNOWN", non-self-healing): the transient upload blip would then finalize
    // a spurious permanent wrong verdict that never resolves on retry.
    #[tokio::test]
    async fn collect_output_upload_failure_is_self_healing_internal_error() {
        // PID + a process-unique counter so a temp dir never collides with a
        // concurrently-running sibling test (matches the `make_fifo` convention).
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let uniq = SEQ.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!(
            "broccoli-collect-fail-{}-{uniq}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);

        // The mock sandbox runs a real child: exit 0 leaves the collect file behind.
        let mock = MockSandboxManager::new(base.clone());
        let box_id = allocate_box_id();
        let working_dir = mock
            .create_sandbox(Some(box_id.as_str()))
            .await
            .expect("mock sandbox creates");

        let (metrics, _provider) =
            common::observability::init_metrics_local("broccoli-worker-test");
        let handler = OperationHandler::new(
            Box::new(mock.clone()),
            // Upload always fails -> models a transient blob-store fault that hits
            // only AFTER a successful run, on the collect path.
            Box::new(UnavailableFileCacher::new("test-induced upload fault")),
            Box::new(NoopTaskCacheStore),
            crate::toolchain_fingerprint::ToolchainFingerprint::Known("test-fingerprint".into()),
            metrics,
        );

        let step = collecting_step(
            "run",
            vec!["/bin/sh".into(), "-c".into(), "printf hi > out.txt".into()],
            vec!["out.txt".into()],
        );
        let mut environments = HashMap::new();
        environments.insert(
            "env".to_string(),
            EnvironmentList::new("env".to_string(), box_id, working_dir.clone()),
        );

        let result = handler
            .execute_step(&step, &environments, &HashMap::new(), None, &HashSet::new())
            .await
            .expect(
                "a post-run collect/upload infra failure must NOT propagate as Err \
                 (Err routes to a terminal UNKNOWN); it must return Ok(InternalError)",
            );

        assert_eq!(
            result.sandbox_result.sandbox_status,
            SandboxStatus::InternalError,
            "collect/upload infra failure must be tagged InternalError so it self-heals"
        );
        assert_eq!(result.sandbox_result.status, "XX");
        assert!(!result.success, "an infra-failed step is not a success");
        assert!(
            result.collected_outputs.is_empty(),
            "no collected outputs are trustworthy when collection failed"
        );

        let _ = std::fs::remove_dir_all(&base);
    }
}
