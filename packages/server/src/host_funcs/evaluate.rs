use broccoli_server_sdk::types::host_io::{
    BatchId, CancelTestCasesInput, CancelTestCasesOutput, NextResultInput, NextResultOutput,
};
use broccoli_server_sdk::types::{StartDetachedWindowedEvaluateInput, StartEvaluateBatchInput};
use extism::{Function, UserData, Val, ValType};
use std::sync::Arc;
use std::time::Duration;

use crate::host_funcs::context::EvaluateHostDeps;
use crate::services::evaluate_batch;

struct EvaluateContext {
    plugin_id: String,
    deps: EvaluateHostDeps,
}

impl super::HostFnContext for EvaluateContext {
    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
}

type EvaluateUserData = UserData<Arc<EvaluateContext>>;

pub fn create_evaluate_functions(plugin_id: String, deps: EvaluateHostDeps) -> Vec<Function> {
    let user_data = super::host_data(EvaluateContext { plugin_id, deps });

    vec![
        super::host_function(
            "start_evaluate_batch",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            super::Waits::Blocks,
            start_evaluate_batch_fn,
        ),
        super::host_function(
            "start_detached_windowed_evaluate",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            super::Waits::Returns,
            start_detached_windowed_evaluate_fn,
        ),
        super::host_function(
            "get_next_evaluate_result",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            super::Waits::Blocks,
            get_next_evaluate_result_fn,
        ),
        super::host_function(
            "cancel_evaluate_batch",
            [ValType::I64],
            [],
            user_data.clone(),
            super::Waits::Blocks,
            cancel_evaluate_batch_fn,
        ),
        super::host_function(
            "cancel_evaluate_test_cases",
            [ValType::I64],
            [ValType::I64],
            user_data,
            super::Waits::Blocks,
            cancel_evaluate_test_cases_fn,
        ),
    ]
}

fn start_detached_windowed_evaluate_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: EvaluateUserData,
) -> Result<(), extism::Error> {
    let input: StartDetachedWindowedEvaluateInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "start_detached_windowed_evaluate")?;

    let session = evaluate_batch::start_detached_windowed_evaluate(
        call.plugin_id.clone(),
        call.deps.for_current_call(),
        input,
    )
    .map_err(|e| extism::Error::msg(e.to_string()))?;
    super::write_json(plugin, outputs, &session, "session")
}

fn start_evaluate_batch_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: EvaluateUserData,
) -> Result<(), extism::Error> {
    let input: StartEvaluateBatchInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "start_evaluate_batch")?;

    let batch_id = tokio::runtime::Handle::current()
        .block_on(evaluate_batch::start_evaluate_batch(
            call.plugin_id.clone(),
            call.deps.for_current_call(),
            input,
        ))
        .map_err(|e| extism::Error::msg(e.to_string()))?;

    super::write_json(plugin, outputs, &BatchId { batch_id }, "batch_id")
}

fn get_next_evaluate_result_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: EvaluateUserData,
) -> Result<(), extism::Error> {
    let input: NextResultInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "get_next_evaluate_result")?;
    let deps = &call.deps;

    let result = evaluate_batch::next_evaluate_result(
        &call.plugin_id,
        &deps.evaluate_batches,
        &input.batch_id,
        super::within_call_deadline(Duration::from_millis(input.timeout_ms)),
    )
    .map_err(|e| extism::Error::msg(e.to_string()))?;
    super::write_json(plugin, outputs, &NextResultOutput { result }, "result")
}

fn cancel_evaluate_batch_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: EvaluateUserData,
) -> Result<(), extism::Error> {
    let input: BatchId = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "cancel_evaluate_batch")?;
    let deps = &call.deps;
    if deps.cancel_primitive_enabled
        && let Some(client) = deps.redis_client.as_ref()
    {
        let op_task_ids = deps.evaluate_batches.operation_task_ids(&input.batch_id);
        let client = client.clone();
        let batch_id = input.batch_id.clone();
        tokio::runtime::Handle::current().block_on(async move {
            if let Err(e) = common::cancel::set_cancel_batch_key(&client, &batch_id)
                .await
            {
                tracing::warn!(error = %e, batch_id = %batch_id, "Failed to set Redis batch cancel key");
            }
            if !op_task_ids.is_empty()
                && let Err(e) =
                    common::cancel::set_cancel_op_keys(&client, &op_task_ids).await
            {
                tracing::warn!(error = %e, batch_id = %batch_id, "Failed to set Redis op cancel keys");
            }
        });
    }

    evaluate_batch::cancel_evaluate_batch(&call.plugin_id, &deps.evaluate_batches, &input.batch_id);

    Ok(())
}

fn cancel_evaluate_test_cases_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: EvaluateUserData,
) -> Result<(), extism::Error> {
    let input: CancelTestCasesInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "cancel_evaluate_test_cases")?;
    let deps = &call.deps;

    let count = deps
        .evaluate_batches
        .cancel_cases(&input.evaluate_batch_id, &input.test_case_ids);

    if deps.cancel_primitive_enabled
        && let Some(client) = deps.redis_client.as_ref()
    {
        let op_task_ids = deps
            .evaluate_batches
            .operation_task_ids_for_cases(&input.evaluate_batch_id, &input.test_case_ids);
        if !op_task_ids.is_empty() {
            let client = client.clone();
            let batch_id = input.evaluate_batch_id.clone();
            tokio::runtime::Handle::current().block_on(async move {
                if let Err(e) =
                    common::cancel::set_cancel_op_keys(&client, &op_task_ids).await
                {
                    tracing::warn!(error = %e, batch_id = %batch_id, "Failed to set Redis op cancel keys");
                }
            });
        }
    }

    super::write_json(plugin, outputs, &CancelTestCasesOutput { count }, "count")
}
