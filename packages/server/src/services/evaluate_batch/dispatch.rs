// NOTE: `start_evaluate_batch` is not exercised by direct unit tests here. It
// consumes a full `EvaluateHostDeps` graph (plugin manager, DB connection,
// blob store, evaluator/operation registries) that can only be wired up in the
// integration suite. The FanoutSemaphore that bounds fan-out is tested in
// `crate::dispatcher::fanout` and the end-to-end bounded fan-out behaviour
// relies on the integration suite + stress-test harness for verification.

use super::*;

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

    let (batch_tx, batch_rx) = flume::unbounded();
    let pending_count = Arc::new(AtomicUsize::new(test_case_count));

    deps.evaluate_batches.insert(
        batch_id.clone(),
        BatchState {
            result_rx: batch_rx,
            pending_count: pending_count.clone(),
            created_at: Instant::now(),
            cleanup_keys: Arc::new(Vec::new()),
            poisoned: AtomicBool::new(false),
        },
    );

    tracing::info!(
        caller = %caller_plugin_id,
        batch_id = %batch_id,
        problem_type = %problem_type,
        test_case_count = test_case_count,
        evaluator_plugin = %evaluator.plugin_id,
        evaluator_fn = %evaluator.function_name,
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
                KeyValue::new("evaluator.function", evaluator.function_name.clone()),
            ],
        );
        metrics
            .batch_active
            .add(1, &[KeyValue::new("batch.kind", "evaluate")]);
        metrics.batch_pending_items.add(
            test_case_count as i64,
            &[KeyValue::new("batch.kind", "evaluate")],
        );
    }

    if is_batch_evaluator_callback_path(&evaluator) {
        spawn_batch_evaluator_callback_dispatch(
            deps,
            batch_id.clone(),
            resolved_inputs,
            batch_tx,
            pending_count,
            evaluator.plugin_id.clone(),
            problem_type,
        );
        return Ok(batch_id);
    }

    // Bounded fan-out: a dedicated dispatcher task acquires one fan-out
    // permit per test case BEFORE spawning the per-tc worker. The permit is
    // held inside the spawned task for its whole lifetime, so the count of
    // live test-case tasks server-wide is bounded by `fanout_slots` regardless
    // of incoming batch size. Without this, a 1000-submission burst with 20
    // testcases/submission would spawn 20,000 tasks in milliseconds and all
    // would contend on the inner `evaluator_slots` semaphore.
    //
    // The dispatcher is wrapped in a DispatchGuard (defined below) so that if
    // the loop ever panics mid-iteration, the remaining test cases are drained
    // with SystemError verdicts rather than leaving the batch's pending_count
    // hung above zero (which would block `next_evaluate_result` callers
    // indefinitely). The closed-semaphore path inside the loop handles its own
    // cleanup synchronously; the guard only fires for unexpected unwinds.
    let fanout = deps.fanout_slots.clone();
    let pm = deps.plugin_manager.clone();
    let eval_plugin_id = evaluator.plugin_id.clone();
    let eval_fn_name = evaluator.function_name.clone();
    let evaluator_slots = deps.evaluator_slots.clone();
    let metrics = deps.metrics.clone();
    let batch_tx_for_dispatcher = batch_tx.clone();
    let pending_for_dispatcher = pending_count.clone();
    let undispatched_tc_ids: Vec<i32> = resolved_inputs.iter().map(|tc| tc.test_case_id).collect();

    tokio::spawn(async move {
        let mut guard = DispatchGuard {
            pending: undispatched_tc_ids,
            batch_tx: batch_tx_for_dispatcher.clone(),
            pending_count: pending_for_dispatcher.clone(),
            metrics: metrics.clone(),
        };

        for tc_input in resolved_inputs {
            let tc_id = tc_input.test_case_id;
            let permit = match fanout.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    // Semaphore closed (server shutdown). Mark the remaining
                    // test cases SystemError so waiters don't hang.
                    send_system_error(
                        &batch_tx_for_dispatcher,
                        tc_id,
                        "Fan-out semaphore closed (server shutting down)".into(),
                    );
                    decrement_pending(&pending_for_dispatcher, metrics.as_ref());
                    guard.claim(tc_id);
                    continue;
                }
            };

            // Claim this tc_id from the guard BEFORE spawning: once the worker
            // is spawned, it owns the cleanup obligation. If the spawn itself
            // unwinds (impossible today but cheap to be defensive), the guard
            // would otherwise double-cleanup.
            guard.claim(tc_id);

            let pm = pm.clone();
            let eval_plugin_id = eval_plugin_id.clone();
            let eval_fn_name = eval_fn_name.clone();
            let problem_type = problem_type.clone();
            let evaluator_slots = evaluator_slots.clone();
            let batch_tx = batch_tx_for_dispatcher.clone();
            let pending = pending_for_dispatcher.clone();
            let metrics = metrics.clone();

            let span = tracing::info_span!(
                "evaluate_test_case",
                test_case_id = tc_id,
                evaluator_plugin = %eval_plugin_id,
                evaluator_function = %eval_fn_name
            );

            tokio::spawn(
                async move {
                    // Hold the fan-out permit for the lifetime of this task.
                    let _fanout_permit = permit;

                    let evaluator_wait_start = Instant::now();
                    let _permit = match evaluator_slots.acquire_owned().await {
                        Ok(permit) => {
                            record_evaluator_semaphore_wait(
                                metrics.as_ref(),
                                &eval_plugin_id,
                                &eval_fn_name,
                                &problem_type,
                                "success",
                                evaluator_wait_start.elapsed(),
                            );
                            permit
                        }
                        Err(_) => {
                            record_evaluator_semaphore_wait(
                                metrics.as_ref(),
                                &eval_plugin_id,
                                &eval_fn_name,
                                &problem_type,
                                "closed",
                                evaluator_wait_start.elapsed(),
                            );
                            send_system_error(
                                &batch_tx,
                                tc_id,
                                "Evaluator dispatcher is shutting down".into(),
                            );
                            decrement_pending(&pending, metrics.as_ref());
                            return;
                        }
                    };

                    let input_bytes = match serde_json::to_vec(&tc_input) {
                        Ok(bytes) => bytes,
                        Err(e) => {
                            send_system_error(
                                &batch_tx,
                                tc_id,
                                format!("Failed to serialize evaluator input: {}", e),
                            );
                            decrement_pending(&pending, metrics.as_ref());
                            return;
                        }
                    };

                    // Retry on plugin pool contention. Plugin pool exhaustion is a transient
                    // backpressure signal, not a permanent failure of the contestant's submission
                    // - looping until we get a permit preserves the verdict semantics. Other
                    // plugin errors (load failures, execution faults, deserialization) are
                    // genuine SystemErrors and fall through to the final send_system_error.
                    match call_raw_with_pool_retry(
                        pm.as_ref(),
                        &eval_plugin_id,
                        &eval_fn_name,
                        input_bytes,
                        PoolRetryPolicy::default(),
                    )
                    .await
                    {
                        Ok(result_bytes) => {
                            match serde_json::from_slice::<TestCaseVerdict>(&result_bytes) {
                                Ok(verdict) => {
                                    let _ = batch_tx.send(verdict);
                                }
                                Err(e) => {
                                    send_system_error(
                                        &batch_tx,
                                        tc_id,
                                        format!("Failed to deserialize evaluator result: {}", e),
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            send_system_error(
                                &batch_tx,
                                tc_id,
                                format!("Evaluator call failed: {}", e),
                            );
                        }
                    }
                    decrement_pending(&pending, metrics.as_ref());
                }
                .instrument(span),
            );
        }
    });

    Ok(batch_id)
}

fn is_batch_evaluator_callback_path(evaluator: &PluginHandler) -> bool {
    evaluator.plugin_id == BATCH_EVALUATOR_PLUGIN_ID
        && evaluator.function_name == BATCH_EVALUATOR_LEGACY_FN
}

fn spawn_batch_evaluator_callback_dispatch(
    deps: EvaluateHostDeps,
    batch_id: String,
    resolved_inputs: Vec<BuildEvalOpsInput>,
    batch_tx: flume::Sender<TestCaseVerdict>,
    pending_count: Arc<AtomicUsize>,
    evaluator_plugin_id: String,
    problem_type: String,
) {
    let fanout = deps.fanout_slots.clone();
    let pm = deps.plugin_manager.clone();
    let evaluator_slots = deps.evaluator_slots.clone();
    let metrics = deps.metrics.clone();
    let operation_deps = deps.operation_deps.clone();
    let (ready_tx, ready_rx) =
        mpsc::channel::<EvaluateOperationResultInput>(BATCH_EVALUATOR_CALLBACK_COALESCE * 2);
    let undispatched_tc_ids: Vec<i32> = resolved_inputs.iter().map(|tc| tc.test_case_id).collect();

    tokio::spawn(run_batch_evaluator_callback_aggregator(
        evaluator_plugin_id.clone(),
        problem_type.clone(),
        pm.clone(),
        evaluator_slots.clone(),
        ready_rx,
        batch_tx.clone(),
        pending_count.clone(),
        metrics.clone(),
    ));

    tokio::spawn(async move {
        let mut guard = DispatchGuard {
            pending: undispatched_tc_ids,
            batch_tx: batch_tx.clone(),
            pending_count: pending_count.clone(),
            metrics: metrics.clone(),
        };

        for tc_input in resolved_inputs {
            let tc_id = tc_input.test_case_id;
            let permit = match fanout.acquire().await {
                Ok(p) => p,
                Err(_) => {
                    send_system_error(
                        &batch_tx,
                        tc_id,
                        "Fan-out semaphore closed (server shutting down)".into(),
                    );
                    decrement_pending(&pending_count, metrics.as_ref());
                    guard.claim(tc_id);
                    continue;
                }
            };

            guard.claim(tc_id);

            let pm = pm.clone();
            let evaluator_slots = evaluator_slots.clone();
            let metrics = metrics.clone();
            let operation_deps = operation_deps.clone();
            let ready_tx = ready_tx.clone();
            let batch_tx = batch_tx.clone();
            let pending = pending_count.clone();
            let evaluator_plugin_id = evaluator_plugin_id.clone();
            let problem_type = problem_type.clone();
            let batch_id = batch_id.clone();

            let span = tracing::info_span!(
                "batch_evaluator_callback_test_case",
                test_case_id = tc_id,
                evaluator_plugin = %evaluator_plugin_id
            );

            tokio::spawn(
                async move {
                    let _fanout_permit = permit;
                    let result = run_batch_evaluator_case_continuation(
                        pm.as_ref(),
                        evaluator_slots,
                        operation_deps,
                        evaluator_plugin_id.clone(),
                        problem_type,
                        batch_id,
                        tc_input,
                        metrics.as_ref(),
                    )
                    .await;

                    match result {
                        Ok(ready_case) => {
                            if ready_tx.send(ready_case).await.is_err() {
                                send_system_error(
                                    &batch_tx,
                                    tc_id,
                                    "Batch-evaluator callback aggregator stopped".into(),
                                );
                                decrement_pending(&pending, metrics.as_ref());
                            }
                        }
                        Err(e) => {
                            send_system_error(&batch_tx, tc_id, format!("{e:#}"));
                            decrement_pending(&pending, metrics.as_ref());
                        }
                    }
                }
                .instrument(span),
            );
        }
    });
}

// Evaluator dispatch stage: pm handle, semaphore, host deps, plugin/problem ids,
// batch id, per-case input, metrics - all distinct values.
#[allow(clippy::too_many_arguments)]
async fn run_batch_evaluator_case_continuation(
    pm: &dyn plugin_core::traits::PluginManager,
    evaluator_slots: Arc<tokio::sync::Semaphore>,
    operation_deps: crate::host_funcs::context::OperationHostDeps,
    evaluator_plugin_id: String,
    problem_type: String,
    evaluate_batch_id: String,
    tc_input: BuildEvalOpsInput,
    metrics: Option<&common::metrics::Metrics>,
) -> anyhow::Result<EvaluateOperationResultInput> {
    let prepare_wait_start = Instant::now();
    let _prepare_permit = evaluator_slots
        .acquire()
        .await
        .map_err(|_| anyhow!("Evaluator dispatcher is shutting down"))?;
    record_evaluator_semaphore_wait(
        metrics,
        &evaluator_plugin_id,
        BATCH_EVALUATOR_PREPARE_FN,
        &problem_type,
        "success",
        prepare_wait_start.elapsed(),
    );

    let input_bytes = serde_json::to_vec(&tc_input)
        .with_context(|| "Failed to serialize batch-evaluator prepare input")?;
    let prepared_bytes = call_raw_with_pool_retry(
        pm,
        &evaluator_plugin_id,
        BATCH_EVALUATOR_PREPARE_FN,
        input_bytes,
        PoolRetryPolicy::default(),
    )
    .await
    .with_context(|| "Batch-evaluator prepare callback failed")?;
    drop(_prepare_permit);

    let mut prepared = serde_json::from_slice::<PreparedEvaluateCase>(&prepared_bytes)
        .with_context(|| "Failed to deserialize batch-evaluator prepared case")?;
    if prepared.operations.is_empty() {
        return Err(anyhow!("Batch-evaluator prepared no operations"));
    }
    tag_operation_tasks_for_evaluate_batch(
        &mut prepared.operations,
        &evaluate_batch_id,
        tc_input.test_case_id,
    );

    let operation_count = prepared.operations.len();
    let operation_batch_id = crate::services::operation_batch::start_operation_batch(
        evaluator_plugin_id.clone(),
        operation_deps.clone(),
        prepared.operations,
    )
    .await
    .with_context(|| "Failed to start prepared operation batch")?;

    let operation_results = wait_for_operation_results(
        evaluator_plugin_id,
        operation_deps,
        operation_batch_id,
        operation_count,
        Duration::from_millis(prepared.result_timeout_ms),
    )
    .await?;

    Ok(EvaluateOperationResultInput {
        case: tc_input,
        operation_results,
    })
}

/// Large infrastructure ceiling for the batch-evaluator operation-result wait.
/// The per-op `timeout` is the small solution-derived budget; under load
/// (queueing + cold blob IO) that elapses while the worker is still alive and
/// processing. Bounding the wait by it converts SYSTEM slowness into a
/// SystemError. Extend up to this ceiling instead so slow degrades to slow, not
/// failed; the solution's real time limit is enforced inside isolate, and a dead
/// worker is reclaimed by the dispatcher stuck-detector. Zero in tests so the
/// explicit `timeout` still governs.
#[cfg(not(test))]
fn batch_evaluator_result_infra_floor() -> Duration {
    Duration::from_secs(30 * 60)
}
#[cfg(test)]
fn batch_evaluator_result_infra_floor() -> Duration {
    Duration::from_secs(0)
}

async fn wait_for_operation_results(
    plugin_id: String,
    operation_deps: crate::host_funcs::context::OperationHostDeps,
    operation_batch_id: String,
    expected_count: usize,
    timeout: Duration,
) -> anyhow::Result<Vec<OperationResult>> {
    let batches = operation_deps.operation_batches.clone();
    let waiters = operation_deps.operation_waiters.clone();
    let metrics = operation_deps.metrics.clone();
    let ceiling = timeout.max(batch_evaluator_result_infra_floor());
    // Collect the operation results by awaiting each as a future - no blocking
    // thread is held for the batch's lifetime, and a cancelled/superseded batch
    // (dropped sender) ends the wait immediately instead of leaking a thread to
    // the infra ceiling.
    let started = Instant::now();
    let mut task_results = Vec::with_capacity(expected_count);
    for _ in 0..expected_count {
        let remaining = ceiling
            .checked_sub(started.elapsed())
            .unwrap_or(Duration::ZERO);
        if remaining.is_zero() {
            crate::services::operation_batch::cancel_operation_batch(
                &plugin_id,
                &batches,
                &waiters,
                metrics.as_ref(),
                &operation_batch_id,
            );
            return Err(anyhow!(
                "Operation batch {} timed out waiting for {} result(s)",
                operation_batch_id,
                expected_count
            ));
        }
        match crate::services::operation_batch::next_operation_result_async(
            &plugin_id,
            &batches,
            metrics.as_ref(),
            &operation_batch_id,
            remaining,
        )
        .await?
        {
            Some(result) => task_results.push(result),
            None => {
                crate::services::operation_batch::cancel_operation_batch(
                    &plugin_id,
                    &batches,
                    &waiters,
                    metrics.as_ref(),
                    &operation_batch_id,
                );
                return Err(anyhow!(
                    "Operation batch {} timed out waiting for {} result(s)",
                    operation_batch_id,
                    expected_count
                ));
            }
        }
    }

    operation_results_from_task_results(task_results)
}

// Aggregator task: plugin/problem ids, pm handle, semaphore, ready/verdict
// channels, pending-count atomic - distinct values, not a struct-shaped bag.
#[allow(clippy::too_many_arguments)]
async fn run_batch_evaluator_callback_aggregator(
    evaluator_plugin_id: String,
    problem_type: String,
    pm: Arc<dyn plugin_core::traits::PluginManager>,
    evaluator_slots: Arc<tokio::sync::Semaphore>,
    mut ready_rx: mpsc::Receiver<EvaluateOperationResultInput>,
    batch_tx: flume::Sender<TestCaseVerdict>,
    pending_count: Arc<AtomicUsize>,
    metrics: Option<common::metrics::Metrics>,
) {
    let mut pending = Vec::with_capacity(BATCH_EVALUATOR_CALLBACK_COALESCE);
    while let Some(ready_case) = ready_rx.recv().await {
        if let Some(items) = push_ready_case_for_callback(&mut pending, ready_case) {
            flush_batch_evaluator_callback(
                &evaluator_plugin_id,
                &problem_type,
                pm.as_ref(),
                evaluator_slots.clone(),
                &batch_tx,
                &pending_count,
                metrics.as_ref(),
                items,
            )
            .await;
        }
    }

    if !pending.is_empty() {
        flush_batch_evaluator_callback(
            &evaluator_plugin_id,
            &problem_type,
            pm.as_ref(),
            evaluator_slots,
            &batch_tx,
            &pending_count,
            metrics.as_ref(),
            std::mem::take(&mut pending),
        )
        .await;
    }
}

// Callback flush: plugin/problem ids, pm handle, semaphore, verdict channel,
// pending-count atomic, metrics - distinct values.
#[allow(clippy::too_many_arguments)]
async fn flush_batch_evaluator_callback(
    evaluator_plugin_id: &str,
    problem_type: &str,
    pm: &dyn plugin_core::traits::PluginManager,
    evaluator_slots: Arc<tokio::sync::Semaphore>,
    batch_tx: &flume::Sender<TestCaseVerdict>,
    pending_count: &AtomicUsize,
    metrics: Option<&common::metrics::Metrics>,
    items: Vec<EvaluateOperationResultInput>,
) {
    let callback_wait_start = Instant::now();
    let permit = match evaluator_slots.acquire().await {
        Ok(permit) => {
            record_evaluator_semaphore_wait(
                metrics,
                evaluator_plugin_id,
                BATCH_EVALUATOR_CALLBACK_FN,
                problem_type,
                "success",
                callback_wait_start.elapsed(),
            );
            permit
        }
        Err(_) => {
            for item in items {
                send_system_error(
                    batch_tx,
                    item.case.test_case_id,
                    "Evaluator dispatcher is shutting down".into(),
                );
                decrement_pending(pending_count, metrics);
            }
            return;
        }
    };

    let result = call_batch_evaluator_callback(evaluator_plugin_id, pm, items.clone()).await;
    drop(permit);

    let mut verdicts_by_case = match result {
        Ok(verdicts) => verdicts
            .into_iter()
            .map(|verdict| (verdict.test_case_id, verdict))
            .collect::<HashMap<_, _>>(),
        Err(e) => {
            for item in items {
                send_system_error(batch_tx, item.case.test_case_id, format!("{e:#}"));
                decrement_pending(pending_count, metrics);
            }
            return;
        }
    };

    for item in items {
        let verdict = verdicts_by_case
            .remove(&item.case.test_case_id)
            .unwrap_or_else(|| {
                system_error_verdict(
                    item.case.test_case_id,
                    "Batch-evaluator callback omitted testcase verdict",
                )
            });
        let _ = batch_tx.send(verdict);
        decrement_pending(pending_count, metrics);
    }

    for extra in verdicts_by_case.into_values() {
        tracing::warn!(
            test_case_id = extra.test_case_id,
            "Batch-evaluator callback returned an unexpected testcase verdict"
        );
    }
}

async fn call_batch_evaluator_callback(
    evaluator_plugin_id: &str,
    pm: &dyn plugin_core::traits::PluginManager,
    items: Vec<EvaluateOperationResultInput>,
) -> anyhow::Result<Vec<TestCaseVerdict>> {
    let input = EvaluateOperationResultsInput { results: items };
    let input_bytes = serde_json::to_vec(&input)
        .with_context(|| "Failed to serialize batch-evaluator callback input")?;
    let output_bytes = call_raw_with_pool_retry(
        pm,
        evaluator_plugin_id,
        BATCH_EVALUATOR_CALLBACK_FN,
        input_bytes,
        PoolRetryPolicy::default(),
    )
    .await
    .with_context(|| "Batch-evaluator result callback failed")?;

    serde_json::from_slice::<Vec<TestCaseVerdict>>(&output_bytes)
        .with_context(|| "Failed to deserialize batch-evaluator callback verdicts")
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
    let checker_config_model = plugin_config::Entity::find_by_id((
        "problem".to_string(),
        problem_id.to_string(),
        checker_ns,
    ))
    .one(&deps.db)
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
    // The checker source is no longer inlined by the host: checker plugins read it
    // from their own per-problem config (e.g. `standard-checkers:checker_source`).

    let checker_config_value: Option<serde_json::Value> = checker_config_model.map(|pc| pc.config);

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
