use super::*;

pub fn start_detached_windowed_evaluate(
    caller_plugin_id: String,
    deps: EvaluateHostDeps,
    input: StartDetachedWindowedEvaluateInput,
) -> anyhow::Result<DetachedEvaluateSession> {
    if input.callback_fn.trim().is_empty() {
        return Err(anyhow!("Detached evaluate callback_fn cannot be empty"));
    }

    let session_id = Uuid::new_v4().to_string();
    let session = DetachedEvaluateSession {
        session_id: session_id.clone(),
    };
    tokio::spawn(run_detached_windowed_evaluate(
        caller_plugin_id,
        deps,
        session_id,
        input,
    ));
    Ok(session)
}

/// [`WindowedSession`] driver for detached evaluate batches. Evaluate adds three
/// things over the plain operation shape: a server-side retry pool for timed-out
/// ops, after-judging completion hooks, and Redis op-cancel-key signaling on
/// cancellation. The slot id is the test case id, and the verdict already arrives
/// typed (`decode` is identity).
struct EvaluateDriver {
    caller_plugin_id: String,
    session_id: String,
    callback_fn: String,
    deps: EvaluateHostDeps,
    problem_type: String,
    /// The judgement this session judges, when it judges one.
    submission_completion: Option<DetachedSubmissionCompletion>,
    /// Clones of cheap-bodied test cases, so a timed-out op can be re-dispatched.
    retry_pool: HashMap<i32, StartEvaluateCaseInput>,
    /// Per-test-case retry attempts consumed so far.
    op_retries: HashMap<i32, u32>,
}

#[async_trait]
impl WindowedSession for EvaluateDriver {
    type Item = StartEvaluateCaseInput;
    type Raw = TestCaseVerdict;
    type Final = TestCaseVerdict;
    type SlotId = i32;

    fn plugin_id(&self) -> &str {
        &self.caller_plugin_id
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }

    fn slot_id(&self, _index: usize, item: &StartEvaluateCaseInput) -> i32 {
        item.test_case_id
    }

    async fn start_slot(
        &self,
        slot_id: i32,
        item: StartEvaluateCaseInput,
        timeout: Duration,
        tx: mpsc::Sender<SlotOutcome<i32, TestCaseVerdict>>,
    ) -> anyhow::Result<String> {
        let batch_id = start_evaluate_batch(
            self.caller_plugin_id.clone(),
            self.deps.clone(),
            StartEvaluateBatchInput {
                problem_type: self.problem_type.clone(),
                test_cases: vec![item],
            },
        )
        .await?;
        let wait_batch_id = batch_id.clone();
        let wait_plugin_id = self.caller_plugin_id.clone();
        let deps = self.deps.clone();
        tokio::spawn(async move {
            // Await the verdict as a cheap future instead of holding a blocking
            // thread for the test case's whole execution.
            let result = next_evaluate_result_async(
                &wait_plugin_id,
                &deps.evaluate_batches,
                &wait_batch_id,
                timeout,
            )
            .await;
            let _ = tx
                .send(SlotOutcome {
                    slot_id,
                    batch_id: wait_batch_id,
                    result,
                })
                .await;
        });
        Ok(batch_id)
    }

    fn decode(&self, raw: TestCaseVerdict) -> Result<TestCaseVerdict, String> {
        Ok(raw)
    }

    fn timeout_message(&self, slot_id: i32) -> String {
        format!("Test case {slot_id} timed out")
    }

    async fn call_callback(
        &self,
        event: SlotEvent<i32, TestCaseVerdict>,
        state: serde_json::Value,
        completed: usize,
        total: usize,
    ) -> anyhow::Result<SessionDecision<i32>> {
        let cb_event = match event {
            // The verdict carries its own test_case_id, so the slot id is redundant.
            SlotEvent::Result { result, .. } => DetachedEvaluateCallbackEvent::Result { result },
            SlotEvent::Timeout { message } => DetachedEvaluateCallbackEvent::Timeout { message },
            SlotEvent::Exhausted => DetachedEvaluateCallbackEvent::Exhausted,
        };
        let input = DetachedEvaluateCallbackInput {
            session_id: self.session_id.clone(),
            state,
            event: cb_event,
            completed,
            total,
        };
        let output = call_detached_evaluate_callback(
            self.deps.plugin_manager.get()?.as_ref(),
            &self.caller_plugin_id,
            &self.callback_fn,
            input,
        )
        .await?;
        Ok(SessionDecision {
            state: output.state,
            action: match output.action {
                DetachedEvaluateCallbackAction::Continue => SessionAction::Continue,
                DetachedEvaluateCallbackAction::Finish => SessionAction::Finish,
                DetachedEvaluateCallbackAction::Cancel => SessionAction::Cancel,
            },
            refill: output.refill,
            cancel_ids: output.cancel_test_case_ids,
        })
    }

    async fn cancel_active(&self, active: &[(i32, String)]) {
        cancel_active_evaluate_slots(&self.caller_plugin_id, &self.deps, active).await;
    }

    async fn cancel_selected(&self, active: &mut Vec<(i32, String)>, ids: &HashSet<i32>) {
        cancel_evaluate_test_case_ids(&self.caller_plugin_id, &self.deps, active, ids).await;
    }

    async fn take_retry(
        &mut self,
        slot_id: i32,
        _batch_id: &str,
    ) -> Option<StartEvaluateCaseInput> {
        let attempts = self.op_retries.get(&slot_id).copied().unwrap_or(0);
        if attempts < MAX_DETACHED_OP_RETRIES {
            return self.retry_pool.get(&slot_id).cloned();
        }
        None
    }

    fn note_retry(&mut self, slot_id: i32, old_batch_id: &str) {
        let attempts = self.op_retries.entry(slot_id).or_insert(0);
        *attempts += 1;
        // The timed-out batch is replaced; nothing waits on it any more.
        self.deps
            .evaluate_batches
            .remove(old_batch_id, Removal::Superseded);
        tracing::warn!(
            caller_plugin_id = %self.caller_plugin_id,
            session_id = %self.session_id,
            test_case_id = slot_id,
            attempt = *attempts,
            max = MAX_DETACHED_OP_RETRIES,
            "Detached evaluate op timed out; re-dispatching on a fresh op batch"
        );
    }

    fn note_retry_failed(&self, slot_id: i32, error: &anyhow::Error) {
        tracing::error!(
            caller_plugin_id = %self.caller_plugin_id,
            session_id = %self.session_id,
            test_case_id = slot_id,
            error = %error,
            "Failed to re-dispatch timed-out detached evaluate op; surfacing timeout"
        );
    }

    async fn on_end(&self, end: SessionEnd) {
        let Some(completion) = self.submission_completion.as_ref() else {
            return;
        };
        let (error_code, error_message) = match end {
            SessionEnd::Interrupted => return,
            SessionEnd::Aborted(error) => (
                SESSION_ABORTED,
                format!("The contest plugin's judging callback failed: {error}"),
            ),
            SessionEnd::Decided(_) => (
                SESSION_ENDED_WITHOUT_VERDICT,
                "The contest plugin ended judging without recording a verdict".to_string(),
            ),
        };
        finish_judgement(&self.deps, completion, error_code, &error_message).await;
    }
}

async fn run_detached_windowed_evaluate(
    caller_plugin_id: String,
    deps: EvaluateHostDeps,
    session_id: String,
    input: StartDetachedWindowedEvaluateInput,
) {
    let StartDetachedWindowedEvaluateInput {
        batch,
        concurrency,
        result_timeout_ms,
        callback_fn,
        state,
        submission_completion,
    } = input;
    let StartEvaluateBatchInput {
        problem_type,
        test_cases,
    } = batch;
    // Retain a clone of each cheap-bodied test case so a timed-out op can be
    // re-dispatched (see MAX_DETACHED_OP_RETRIES). Large inline bodies are
    // skipped to keep memory bounded; under the object_storage backend every
    // body is a hash ref, so this map is a few KB per case and retry covers all
    // test cases.
    let retry_pool: HashMap<i32, StartEvaluateCaseInput> = test_cases
        .iter()
        .filter(|tc| test_case_cheap_to_retain(tc))
        .map(|tc| (tc.test_case_id, tc.clone()))
        .collect();
    // Every operation the session starts runs for its judgement.
    let mut deps = deps;
    deps.operation_deps.judgement =
        submission_completion
            .as_ref()
            .map(|completion| common::worker::JudgementRef {
                submission_id: completion.submission_id,
                judgement_id: completion.judgement_id,
            });
    let driver = EvaluateDriver {
        caller_plugin_id,
        session_id,
        callback_fn,
        deps,
        problem_type,
        submission_completion,
        retry_pool,
        op_retries: HashMap::new(),
    };
    run_windowed_session(
        driver,
        test_cases,
        concurrency,
        Duration::from_millis(result_timeout_ms),
        state,
    )
    .await;
}

/// `error_code` of a judgement whose judging session aborted before the
/// plugin recorded a verdict.
pub const SESSION_ABORTED: &str = "PLUGIN_SESSION_ABORTED";
/// `error_code` of a judgement whose plugin ended its judging session without
/// recording a verdict.
pub const SESSION_ENDED_WITHOUT_VERDICT: &str = "PLUGIN_NO_VERDICT";

/// Leave the judgement a session judged terminal: finalize it as
/// `SystemError` if the plugin did not (a no-op when it did, or when the
/// judgement was since requeued or superseded), then fire its after-judging
/// hooks.
async fn finish_judgement(
    deps: &EvaluateHostDeps,
    completion: &DetachedSubmissionCompletion,
    error_code: &str,
    error_message: &str,
) {
    match crate::services::judging::fail_dispatch(
        &deps.db,
        crate::services::judging::Unit::Judgement,
        completion.judgement_id,
        completion.judge_epoch,
        error_code,
        error_message,
    )
    .await
    {
        Ok(true) => tracing::warn!(
            submission_id = completion.submission_id,
            judgement_id = completion.judgement_id,
            error_code,
            "Judging session ended without a verdict; finalized as SystemError"
        ),
        Ok(false) => {}
        Err(e) => tracing::error!(
            submission_id = completion.submission_id,
            judgement_id = completion.judgement_id,
            error = %e,
            "Failed to finalize the judgement of an ended judging session; the stuck-job detector requeues it"
        ),
    }
    fire_after_judging_hooks_for_detached_completion(
        &deps.db,
        deps.hook_registry.clone(),
        &deps.contest_type_registry,
        completion,
    )
    .await;
}

async fn call_detached_evaluate_callback(
    plugin_manager: &dyn plugin_core::traits::PluginManager,
    plugin_id: &str,
    callback_fn: &str,
    input: DetachedEvaluateCallbackInput,
) -> anyhow::Result<DetachedEvaluateCallbackOutput> {
    let input_bytes = serde_json::to_vec(&input)
        .with_context(|| "Failed to serialize detached evaluate callback input")?;
    let output_bytes = plugin_manager
        .call_raw(
            plugin_id,
            callback_fn,
            input_bytes,
            plugin_manager.get_config().background_call(),
        )
        .await
        .with_context(|| "Detached evaluate callback failed")?;
    serde_json::from_slice::<DetachedEvaluateCallbackOutput>(&output_bytes)
        .with_context(|| "Failed to deserialize detached evaluate callback output")
}

async fn cancel_active_evaluate_slots(
    plugin_id: &str,
    deps: &EvaluateHostDeps,
    active: &[(i32, String)],
) {
    for (_, batch_id) in active {
        set_evaluate_cancel_op_keys(plugin_id, deps, batch_id, None).await;
        cancel_evaluate_batch(plugin_id, &deps.evaluate_batches, batch_id);
    }
}

async fn cancel_evaluate_test_case_ids(
    plugin_id: &str,
    deps: &EvaluateHostDeps,
    active: &mut Vec<(i32, String)>,
    test_case_ids: &HashSet<i32>,
) {
    let cancelled = active
        .iter()
        .filter(|(test_case_id, _)| test_case_ids.contains(test_case_id))
        .map(|(_, batch_id)| batch_id.clone())
        .collect::<Vec<_>>();

    for batch_id in &cancelled {
        set_evaluate_cancel_op_keys(plugin_id, deps, batch_id, Some(test_case_ids)).await;
    }

    active.retain(|(test_case_id, batch_id)| {
        if test_case_ids.contains(test_case_id) {
            cancel_evaluate_batch(plugin_id, &deps.evaluate_batches, batch_id);
            false
        } else {
            true
        }
    });
}

async fn set_evaluate_cancel_op_keys(
    plugin_id: &str,
    deps: &EvaluateHostDeps,
    batch_id: &str,
    test_case_ids: Option<&HashSet<i32>>,
) {
    if !deps.cancel_primitive_enabled {
        return;
    }
    let Some(client) = deps.redis_client.as_ref() else {
        return;
    };

    let op_task_ids = match test_case_ids {
        Some(test_case_ids) => {
            let ids = test_case_ids.iter().copied().collect::<Vec<_>>();
            deps.evaluate_batches
                .operation_task_ids_for_cases(batch_id, &ids)
        }
        None => deps.evaluate_batches.operation_task_ids(batch_id),
    };
    if op_task_ids.is_empty() {
        return;
    }

    if let Err(e) = common::cancel::set_cancel_op_keys(client, &op_task_ids).await {
        tracing::warn!(
            plugin_id = %plugin_id,
            batch_id = %batch_id,
            error = %e,
            "Failed to set Redis op cancel keys for detached evaluate cancellation"
        );
    }
}
