use super::*;

/// The evaluator plugin a batch dispatches to, with what every call into it
/// needs.
#[derive(Clone)]
struct EvaluatorStage {
    pm: Arc<dyn plugin_core::traits::PluginManager>,
    plugin_id: String,
}

impl EvaluatorStage {
    /// Call `function` on the evaluator with JSON `input` as background work.
    /// How many evaluator guests run at once is admission's business.
    async fn call(&self, function: &str, input_bytes: Vec<u8>) -> anyhow::Result<Vec<u8>> {
        let options = self.pm.get_config().background_call();
        Ok(self
            .pm
            .call_raw(&self.plugin_id, function, input_bytes, options)
            .await?)
    }
}

pub async fn start_evaluate_batch(
    caller_plugin_id: String,
    deps: EvaluateHostDeps,
    input: StartEvaluateBatchInput,
) -> anyhow::Result<String> {
    let problem_type = input.problem_type.clone();
    let evaluator = {
        let registry = deps.evaluator_registry.read().await;
        registry.get(&problem_type).cloned()
    }
    .ok_or_else(|| anyhow!("No evaluator registered for problem type: {}", problem_type))?;

    let batch_id = Uuid::new_v4().to_string();
    let resolved_inputs = resolve_inputs(&caller_plugin_id, &deps, input, &batch_id).await?;
    let test_case_count = resolved_inputs.len();

    let batch_tx = deps.evaluate_batches.open(
        &batch_id,
        resolved_inputs.iter().map(|tc| tc.test_case_id.to_string()),
        EvaluateCases::new(resolved_inputs.iter().map(|tc| tc.test_case_id)),
    );

    tracing::info!(
        caller = %caller_plugin_id,
        batch_id = %batch_id,
        problem_type = %problem_type,
        test_case_count = test_case_count,
        evaluator_plugin = %evaluator.plugin_id,
        evaluator_fn = %evaluator.functions.entry(),
        "Starting evaluate batch"
    );

    if let Some(metrics) = deps.metrics.as_ref() {
        metrics.batch_started_total.add(
            1,
            &[
                KeyValue::new("batch.kind", "evaluate"),
                KeyValue::new("plugin.id", caller_plugin_id.clone()),
                KeyValue::new("problem.type", problem_type.clone()),
                KeyValue::new("evaluator.plugin.id", evaluator.plugin_id.clone()),
                KeyValue::new(
                    "evaluator.function",
                    evaluator.functions.entry().to_string(),
                ),
            ],
        );
    }

    let stage = EvaluatorStage {
        pm: deps.plugin_manager.get()?,
        plugin_id: evaluator.plugin_id.clone(),
    };

    let function = match evaluator.functions {
        EvaluatorFunctions::Direct { function } => function,
        EvaluatorFunctions::Staged { prepare, interpret } => {
            spawn_staged_dispatch(
                stage,
                StagedFunctions { prepare, interpret },
                deps.fanout.clone(),
                deps.operation_deps.clone(),
                batch_id.clone(),
                resolved_inputs,
                batch_tx,
            );
            return Ok(batch_id);
        }
    };

    spawn_fanout(
        deps.fanout.clone(),
        resolved_inputs,
        batch_tx,
        move |tc_input, batch_tx| {
            let stage = stage.clone();
            let function = function.clone();
            let span = tracing::info_span!(
                "evaluate_test_case",
                test_case_id = tc_input.test_case_id,
                evaluator_plugin = %stage.plugin_id,
                evaluator_function = %function
            );
            async move {
                let tc_id = tc_input.test_case_id;
                let verdict = evaluate_case(&stage, &function, tc_input)
                    .await
                    .unwrap_or_else(|e| system_error_verdict(tc_id, format!("{e:#}")));
                batch_tx.deliver(verdict);
            }
            .instrument(span)
        },
    );

    Ok(batch_id)
}

/// Evaluate one test case with a plain (per-case) evaluator function.
async fn evaluate_case(
    stage: &EvaluatorStage,
    function: &str,
    tc_input: BuildEvalOpsInput,
) -> anyhow::Result<TestCaseVerdict> {
    let input_bytes =
        serde_json::to_vec(&tc_input).with_context(|| "Failed to serialize evaluator input")?;
    let result_bytes = stage
        .call(function, input_bytes)
        .await
        .with_context(|| "Evaluator call failed")?;
    serde_json::from_slice::<TestCaseVerdict>(&result_bytes)
        .with_context(|| "Failed to deserialize evaluator result")
}

/// Bounded fan-out: a dedicated dispatcher task acquires one fan-out permit
/// per test case BEFORE spawning `run_case` for it. The permit is held for
/// the spawned task's whole lifetime, so the test cases this server has
/// outstanding are bounded by the live worker slots (see
/// [`crate::dispatcher::fanout`]) whatever the incoming batch size.
///
/// Every case gets exactly one result: `run_case` delivers it, and a
/// [`DispatchGuard`] delivers one for every case not yet handed off if the
/// loop unwinds.
fn spawn_fanout<F, Fut>(
    fanout: crate::dispatcher::fanout::Fanout,
    inputs: Vec<BuildEvalOpsInput>,
    batch_tx: BatchSender<TestCaseVerdict>,
    run_case: F,
) where
    F: Fn(BuildEvalOpsInput, BatchSender<TestCaseVerdict>) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let undispatched: Vec<i32> = inputs.iter().map(|tc| tc.test_case_id).collect();
    tokio::spawn(async move {
        let mut guard = DispatchGuard {
            pending: undispatched,
            batch_tx: batch_tx.clone(),
        };
        for tc_input in inputs {
            let tc_id = tc_input.test_case_id;
            let permit = fanout.acquire().await;
            // Claim BEFORE spawning: the spawned task owns the delivery from
            // here on, so an unwind must not deliver this case again.
            guard.claim(tc_id);
            let case = run_case(tc_input, batch_tx.clone());
            tokio::spawn(async move {
                let _fanout_permit = permit;
                case.await;
            });
        }
    });
}

/// The functions of a staged evaluator (see `EvaluatorFunctions::Staged`).
#[derive(Clone)]
struct StagedFunctions {
    prepare: String,
    interpret: String,
}

/// Run a batch on a staged evaluator: each case is prepared, its operations
/// run without holding a plugin instance, and finished cases are interpreted
/// in groups of up to [`INTERPRET_COALESCE`].
fn spawn_staged_dispatch(
    stage: EvaluatorStage,
    functions: StagedFunctions,
    fanout: crate::dispatcher::fanout::Fanout,
    operation_deps: crate::host_funcs::context::OperationHostDeps,
    batch_id: String,
    resolved_inputs: Vec<BuildEvalOpsInput>,
    batch_tx: BatchSender<TestCaseVerdict>,
) {
    let (ready_tx, ready_rx) =
        mpsc::channel::<EvaluateOperationResultInput>(INTERPRET_COALESCE * 2);

    tokio::spawn(run_interpret_aggregator(
        stage.clone(),
        functions.interpret,
        ready_rx,
        batch_tx.clone(),
    ));
    let prepare = functions.prepare;

    spawn_fanout(
        fanout,
        resolved_inputs,
        batch_tx,
        move |tc_input, batch_tx| {
            let stage = stage.clone();
            let operation_deps = operation_deps.clone();
            let ready_tx = ready_tx.clone();
            let batch_id = batch_id.clone();
            let prepare = prepare.clone();
            let tc_id = tc_input.test_case_id;
            let span = tracing::info_span!(
                "staged_evaluate_test_case",
                test_case_id = tc_id,
                evaluator_plugin = %stage.plugin_id
            );
            async move {
                let result =
                    prepare_and_run_case(&stage, &prepare, operation_deps, batch_id, tc_input)
                        .await;
                match result {
                    Ok(ready_case) => {
                        if ready_tx.send(ready_case).await.is_err() {
                            batch_tx.deliver(system_error_verdict(
                                tc_id,
                                "Evaluator interpret aggregator stopped",
                            ));
                        }
                    }
                    Err(e) => batch_tx.deliver(system_error_verdict(tc_id, format!("{e:#}"))),
                }
            }
            .instrument(span)
        },
    );
}

/// Prepare one case and run its operations to completion.
async fn prepare_and_run_case(
    stage: &EvaluatorStage,
    prepare: &str,
    operation_deps: crate::host_funcs::context::OperationHostDeps,
    evaluate_batch_id: String,
    tc_input: BuildEvalOpsInput,
) -> anyhow::Result<EvaluateOperationResultInput> {
    let input_bytes = serde_json::to_vec(&tc_input)
        .with_context(|| "Failed to serialize evaluator prepare input")?;
    let prepared_bytes = stage
        .call(prepare, input_bytes)
        .await
        .with_context(|| "Evaluator prepare call failed")?;

    let mut prepared = serde_json::from_slice::<PreparedEvaluateCase>(&prepared_bytes)
        .with_context(|| "Failed to deserialize evaluator prepared case")?;
    if prepared.operations.is_empty() {
        return Err(anyhow!("Evaluator prepared no operations"));
    }
    tag_operation_tasks_for_evaluate_batch(
        &mut prepared.operations,
        &evaluate_batch_id,
        tc_input.test_case_id,
    );

    let operation_count = prepared.operations.len();
    let operation_batch_id = crate::services::operation_batch::start_operation_batch(
        stage.plugin_id.clone(),
        operation_deps.clone(),
        prepared.operations,
    )
    .await
    .with_context(|| "Failed to start prepared operation batch")?;

    let operation_results = wait_for_operation_results(
        &stage.plugin_id,
        &operation_deps,
        &operation_batch_id,
        operation_count,
        Duration::from_millis(prepared.result_timeout_ms),
    )
    .await?;

    Ok(EvaluateOperationResultInput {
        case: tc_input,
        operation_results,
    })
}

/// Large infrastructure ceiling for a staged evaluator's operation-result wait.
/// The per-op `timeout` is the small solution-derived budget; under load
/// (queueing + cold blob IO) that elapses while the worker is still alive and
/// processing. Bounding the wait by it converts SYSTEM slowness into a
/// SystemError. Extend up to this ceiling instead so slow degrades to slow, not
/// failed; the solution's real time limit is enforced inside isolate, a dead
/// worker's task is redelivered by the queue, and a task the queue lost fails
/// within one ledger check. Zero in tests so the explicit `timeout` still
/// governs.
#[cfg(not(test))]
fn staged_result_infra_floor() -> Duration {
    Duration::from_secs(30 * 60)
}
#[cfg(test)]
fn staged_result_infra_floor() -> Duration {
    Duration::from_secs(0)
}

async fn wait_for_operation_results(
    plugin_id: &str,
    operation_deps: &crate::host_funcs::context::OperationHostDeps,
    operation_batch_id: &str,
    expected_count: usize,
    timeout: Duration,
) -> anyhow::Result<Vec<OperationResult>> {
    let batches = &operation_deps.operation_batches;
    let ceiling = timeout.max(staged_result_infra_floor());
    // Collect the operation results by awaiting each as a future - no blocking
    // thread is held for the batch's lifetime, and a cancelled/superseded batch
    // (dropped sender) ends the wait immediately instead of leaking a thread to
    // the infra ceiling.
    let started = Instant::now();
    let mut task_results = Vec::with_capacity(expected_count);
    for _ in 0..expected_count {
        let remaining = ceiling.saturating_sub(started.elapsed());
        let result = if remaining.is_zero() {
            None
        } else {
            crate::services::operation_batch::next_operation_result_async(
                plugin_id,
                batches,
                operation_batch_id,
                remaining,
            )
            .await?
        };
        let Some(result) = result else {
            crate::services::operation_batch::cancel_operation_batch(
                plugin_id,
                batches,
                &operation_deps.operation_waiters,
                operation_batch_id,
            );
            return Err(anyhow!(
                "Operation batch {} timed out waiting for {} result(s)",
                operation_batch_id,
                expected_count
            ));
        };
        task_results.push(result);
    }

    operation_results_from_task_results(task_results)
}

async fn run_interpret_aggregator(
    stage: EvaluatorStage,
    interpret: String,
    mut ready_rx: mpsc::Receiver<EvaluateOperationResultInput>,
    batch_tx: BatchSender<TestCaseVerdict>,
) {
    let mut pending = Vec::with_capacity(INTERPRET_COALESCE);
    while let Some(ready_case) = ready_rx.recv().await {
        if let Some(items) = push_ready_case_for_callback(&mut pending, ready_case) {
            flush_interpret(&stage, &interpret, &batch_tx, items).await;
        }
    }

    if !pending.is_empty() {
        flush_interpret(&stage, &interpret, &batch_tx, pending).await;
    }
}

/// Deliver one verdict per item: the evaluator's, or a SystemError when the
/// interpret call failed or omitted the case.
async fn flush_interpret(
    stage: &EvaluatorStage,
    interpret: &str,
    batch_tx: &BatchSender<TestCaseVerdict>,
    items: Vec<EvaluateOperationResultInput>,
) {
    let case_ids: Vec<i32> = items.iter().map(|item| item.case.test_case_id).collect();
    let mut verdicts_by_case = match call_interpret(stage, interpret, items).await {
        Ok(verdicts) => verdicts
            .into_iter()
            .map(|verdict| (verdict.test_case_id, verdict))
            .collect::<HashMap<_, _>>(),
        Err(e) => {
            for tc_id in case_ids {
                batch_tx.deliver(system_error_verdict(tc_id, format!("{e:#}")));
            }
            return;
        }
    };

    for tc_id in case_ids {
        let verdict = verdicts_by_case.remove(&tc_id).unwrap_or_else(|| {
            system_error_verdict(tc_id, "Evaluator interpret call omitted testcase verdict")
        });
        batch_tx.deliver(verdict);
    }

    for extra in verdicts_by_case.into_values() {
        tracing::warn!(
            test_case_id = extra.test_case_id,
            "Evaluator interpret call returned an unexpected testcase verdict"
        );
    }
}

async fn call_interpret(
    stage: &EvaluatorStage,
    interpret: &str,
    items: Vec<EvaluateOperationResultInput>,
) -> anyhow::Result<Vec<TestCaseVerdict>> {
    let input = EvaluateOperationResultsInput { results: items };
    let input_bytes = serde_json::to_vec(&input)
        .with_context(|| "Failed to serialize evaluator interpret input")?;
    let output_bytes = stage
        .call(interpret, input_bytes)
        .await
        .with_context(|| "Evaluator interpret call failed")?;

    serde_json::from_slice::<Vec<TestCaseVerdict>>(&output_bytes)
        .with_context(|| "Failed to deserialize evaluator interpret verdicts")
}

async fn resolve_inputs(
    caller_plugin_id: &str,
    deps: &EvaluateHostDeps,
    input: StartEvaluateBatchInput,
    evaluate_batch_id: &str,
) -> anyhow::Result<Vec<BuildEvalOpsInput>> {
    let Some((problem_id, solution_language)) = validate_batch_shape(&input.test_cases)? else {
        return Ok(Vec::new());
    };

    let problem_model = problem::Entity::find_by_id(problem_id)
        .one(&deps.db)
        .await
        .with_context(|| "Failed to query problem")?
        .ok_or_else(|| anyhow!("Problem {} not found", problem_id))?;

    let checker_ns = format!("{}:checker", caller_plugin_id);
    let checker_config_model = deps
        .plugin_config_cache
        .get(&deps.db, "problem", &problem_id.to_string(), &checker_ns)
        .await
        .with_context(|| "Failed to query checker config")?;

    let af_models = additional_file::Entity::find()
        .filter(additional_file::Column::ProblemId.eq(problem_id))
        .filter(additional_file::Column::Language.eq(solution_language.as_str()))
        .all(&deps.db)
        .await
        .with_context(|| "Failed to query additional_files")?;

    let additional_file_refs: Vec<FileRef> = af_models
        .into_iter()
        .map(|r| FileRef {
            filename: r.path,
            content_type: r.content_type,
            blob_hash: r.content_hash,
            read_token: None,
        })
        .collect();

    let checker_format = Some(problem_model.checker_format.clone());
    // The host does not inline the checker source: checker plugins read it
    // from their own per-problem config (e.g. `standard-checkers:checker_source`).

    let checker_config_value: Option<serde_json::Value> =
        checker_config_model.map(|pc| pc.config.clone());

    let test_cases = input.test_cases;
    let db_needed_ids: Vec<i32> = test_cases
        .iter()
        .filter(|tc| !tc.is_custom && (tc.input.is_missing() || tc.expected_output.is_missing()))
        .map(|tc| tc.test_case_id)
        .collect();
    let mut db_case_map: HashMap<i32, test_case::Model> = if db_needed_ids.is_empty() {
        HashMap::new()
    } else {
        test_case::Entity::find()
            .filter(test_case::Column::ProblemId.eq(problem_id))
            .filter(test_case::Column::Id.is_in(db_needed_ids))
            .all(&deps.db)
            .await
            .with_context(|| "Failed to query test case data")?
            .into_iter()
            .map(|tc| (tc.id, tc))
            .collect()
    };

    let mut resolved = Vec::with_capacity(test_cases.len());
    for tc in test_cases {
        let db_case =
            if !tc.is_custom && (tc.input.is_missing() || tc.expected_output.is_missing()) {
                Some(db_case_map.remove(&tc.test_case_id).ok_or_else(|| {
                    anyhow!("Test case {} not found in database", tc.test_case_id)
                })?)
            } else {
                None
            };

        let (db_input, db_input_blob_hash, db_expected_output, db_expected_output_blob_hash) =
            match db_case {
                Some(tc) => (
                    Some(tc.input),
                    tc.input_blob_hash,
                    Some(tc.expected_output),
                    tc.expected_output_blob_hash,
                ),
                None => (None, None, None, None),
            };

        let test_input = resolve_evaluate_body(
            tc.input,
            db_input,
            db_input_blob_hash,
            "input.txt",
            "evaluate input",
            deps.blob_store.clone(),
        )
        .await?;
        let expected_output = resolve_evaluate_body(
            tc.expected_output,
            db_expected_output,
            db_expected_output_blob_hash,
            "answer.txt",
            "evaluate answer",
            deps.blob_store.clone(),
        )
        .await?;
        let tc_checker_format = if expected_output.present {
            checker_format.clone()
        } else {
            Some("none".to_string())
        };

        resolved.push(BuildEvalOpsInput {
            problem_id: tc.problem_id,
            test_case_id: tc.test_case_id,
            solution_source: tc.solution_source,
            solution_language: tc.solution_language,
            time_limit_ms: tc.time_limit_ms,
            memory_limit_kb: tc.memory_limit_kb,
            contest_id: tc.contest_id,
            evaluate_batch_id: Some(evaluate_batch_id.to_string()),
            test_input: test_input.file,
            expected_output: expected_output.file,
            checker_format: tc_checker_format,
            checker_config: checker_config_value.clone(),
            additional_file_refs: additional_file_refs.clone(),
            target_worker_id: tc.target_worker_id,
        });
    }

    Ok(resolved)
}
