use crate::host_funcs::context::OperationHostDeps;
use crate::services::operation_batch;
use broccoli_server_sdk::types::OperationTask;
use broccoli_server_sdk::types::host_io::{BatchId, NextResultInput, NextResultOutput};
use common::worker::TaskResult;
use extism::{Function, UserData, Val, ValType};
use std::sync::Arc;
use std::time::Duration;

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
        super::host_function(
            "start_operation_batch",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            super::Waits::Blocks,
            start_operation_batch_fn,
        ),
        super::host_function(
            "get_next_operation_result",
            [ValType::I64],
            [ValType::I64],
            user_data.clone(),
            super::Waits::Blocks,
            get_next_operation_result_fn,
        ),
        super::host_function(
            "cancel_operation_batch",
            [ValType::I64],
            [],
            user_data,
            super::Waits::Returns,
            cancel_operation_batch_fn,
        ),
    ]
}

fn start_operation_batch_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let operations: Vec<OperationTask> = super::read_json(plugin, inputs, "operations")?;
    let call = super::HostCall::enter(&user_data, "start_operation_batch")?;

    let batch_id =
        tokio::runtime::Handle::current().block_on(operation_batch::start_operation_batch(
            call.plugin_id.clone(),
            call.deps.for_current_call(),
            operations,
        ))?;

    super::write_json(plugin, outputs, &BatchId { batch_id }, "batch_id")
}

fn get_next_operation_result_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let input: NextResultInput = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "get_next_operation_result")?;

    let result = operation_batch::next_operation_result(
        &call.plugin_id,
        &call.deps.operation_batches,
        &input.batch_id,
        super::within_call_deadline(Duration::from_millis(input.timeout_ms)),
    )?;

    // The guest reads what the worker reported for the operation.
    let result = NextResultOutput {
        result: result.map(|task: TaskResult| task.output),
    };
    super::write_json(plugin, outputs, &result, "result")
}

fn cancel_operation_batch_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    _outputs: &mut [Val],
    user_data: DispatchUserData,
) -> Result<(), extism::Error> {
    let input: BatchId = super::read_json(plugin, inputs, "input")?;
    let call = super::HostCall::enter(&user_data, "cancel_operation_batch")?;

    operation_batch::cancel_operation_batch(
        &call.plugin_id,
        &call.deps.operation_batches,
        &call.deps.operation_waiters,
        &input.batch_id,
    );

    Ok(())
}
