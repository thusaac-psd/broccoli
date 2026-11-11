use crate::host_funcs::context::OperationHostDeps;
use crate::services::operation_batch;
use broccoli_server_sdk::types::{OperationTask, StartDetachedWindowedOperationInput};
use common::worker::TaskResult;
use extism::{Function, UserData, Val, ValType};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

#[derive(Deserialize)]
struct GetNextResultInput {
    batch_id: String,
    timeout_ms: u64,
}

#[derive(Deserialize)]
struct CancelBatchInput {
    batch_id: String,
}

#[derive(Serialize)]
struct ResultResponse {
    result: Option<TaskResult>,
}

struct DispatchContext {
    plugin_id: String,
    deps: OperationHostDeps,
}

impl super::HostFnContext for DispatchContext {
    fn plugin_id(&self) -> &str {
        &self.plugin_id
    }
}

type DispatchUserData = UserData<Arc<DispatchContext>>;

pub fn create_dispatch_functions(plugin_id: String, deps: OperationHostDeps) -> Vec<Function> {
    let user_data = super::host_data(DispatchContext { plugin_id, deps });

    vec![
        Function::new(
            "start_operation_batch",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            start_operation_batch_fn,
        ),
        Function::new(
            "start_detached_windowed_operation",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            start_detached_windowed_operation_fn,
        ),
        Function::new(
            "get_next_operation_result",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            get_next_operation_result_fn,
        ),
        Function::new(
            "cancel_operation_batch",
            [ValType::I64],
            [],
            user_data,
            cancel_operation_batch_fn,
        ),
    ]
}

fn start_detached_windowed_operation_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let input: StartDetachedWindowedOperationInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "start_detached_windowed_operation")?;

    let manager =
        call.deps.plugin_manager.clone().ok_or_else(|| {
            extism::Error::msg("Plugin manager not available for detached operation")
        })?;
    let session = operation_batch::start_detached_windowed_operation(
        call.plugin_id.clone(),
        manager,
        call.deps.clone(),
        input,
    )
    .map_err(|e| extism::Error::msg(e.to_string()))?;

    super::write_json(plugin, outputs, &session, "session")
}

fn start_operation_batch_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let operations: Vec<OperationTask> = super::read_json(plugin, inputs, "operations")?;
    let call = super::HostCall::enter(&user_data, "start_operation_batch")?;

    let batch_id = tokio::runtime::Handle::current()
        .block_on(operation_batch::start_operation_batch(
            call.plugin_id.clone(),
            call.deps.clone(),
            operations,
        ))
        .map_err(|e| extism::Error::msg(e.to_string()))?;

    #[derive(Serialize)]
    struct BatchIdResponse {
        batch_id: String,
    }

    super::write_json(plugin, outputs, &BatchIdResponse { batch_id }, "batch_id")
}

fn get_next_operation_result_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let input: GetNextResultInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "get_next_operation_result")?;

    let result = operation_batch::next_operation_result(
        &call.plugin_id,
        &call.deps.operation_batches,
        call.deps.metrics.as_ref(),
        &input.batch_id,
        Duration::from_millis(input.timeout_ms),
    )
    .map_err(|e| extism::Error::msg(e.to_string()))?;

    super::write_json(plugin, outputs, &ResultResponse { result }, "result")
}

fn cancel_operation_batch_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let input: CancelBatchInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "cancel_operation_batch")?;

    operation_batch::cancel_operation_batch(
        &call.plugin_id,
        &call.deps.operation_batches,
        &call.deps.operation_waiters,
        call.deps.metrics.as_ref(),
        &input.batch_id,
    );

    Ok(())
}
