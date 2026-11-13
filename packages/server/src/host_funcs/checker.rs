use crate::registry::CheckerStageRegistry;
use broccoli_server_sdk::types::{
    CheckerStage, CheckerVerdict, InterpretCheckerInput, ResolveCheckerInput,
};
use extism::{Function, UserData, Val, ValType};
use plugin_core::traits::PluginInvoker;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Checker fusion: resolve + interpret host functions. These dispatch to a
// plugin's resolve/interpret fns via CheckerStageRegistry, mirroring
// `language::resolve_language_fn`. The host stays generic - no checker logic.
// Unknown format / plugin error surface as a host-fn error (the SDK wrapper
// returns Err; the evaluator maps that to SystemError).
// ---------------------------------------------------------------------------

struct CheckerStageContext {
    caller_plugin_id: String,
    checker_stage_registry: CheckerStageRegistry,
    plugin_manager: Arc<dyn PluginInvoker>,
}

impl super::HostFnContext for CheckerStageContext {
    fn plugin_id(&self) -> &str {
        &self.caller_plugin_id
    }
}

fn checker_stage_data(
    caller_plugin_id: String,
    checker_stage_registry: CheckerStageRegistry,
    plugin_manager: Arc<dyn PluginInvoker>,
) -> UserData<Arc<CheckerStageContext>> {
    super::host_data(CheckerStageContext {
        caller_plugin_id,
        checker_stage_registry,
        plugin_manager,
    })
}

pub fn create_resolve_checker_function(
    plugin_id: String,
    checker_stage_registry: CheckerStageRegistry,
    plugin_manager: Arc<dyn PluginInvoker>,
) -> Function {
    Function::new(
        "resolve_checker",
        [ValType::I64],
        [ValType::I64],
        checker_stage_data(plugin_id, checker_stage_registry, plugin_manager),
        resolve_checker_fn,
    )
}

pub fn create_interpret_checker_function(
    plugin_id: String,
    checker_stage_registry: CheckerStageRegistry,
    plugin_manager: Arc<dyn PluginInvoker>,
) -> Function {
    Function::new(
        "interpret_checker_result",
        [ValType::I64],
        [ValType::I64],
        checker_stage_data(plugin_id, checker_stage_registry, plugin_manager),
        interpret_checker_fn,
    )
}

/// Look up the format's handler, call the selected plugin fn, and return its raw
/// output bytes. The caller validates + writes them. Unknown format / plugin
/// error -> `Err` (surfaced to the SDK as an error, mapped to SystemError).
fn checker_stage_call(
    user_data: &UserData<Arc<CheckerStageContext>>,
    host_fn_name: &'static str,
    format: &str,
    select_handler_fn: impl FnOnce(&crate::registry::CheckerStageHandlers) -> String,
    request_bytes: Vec<u8>,
) -> Result<Vec<u8>, extism::Error> {
    let ctx = super::HostCall::enter(user_data, host_fn_name)?;

    let handler = tokio::runtime::Handle::current()
        .block_on(async { ctx.checker_stage_registry.read().await.get(format).cloned() })
        .ok_or_else(|| {
            extism::Error::msg(format!("No checker resolver registered for '{format}'"))
        })?;

    let function_name = select_handler_fn(&handler);
    tracing::debug!(
        caller = %ctx.caller_plugin_id,
        handler_plugin = %handler.plugin_id,
        handler_fn = %function_name,
        checker_format = %format,
        "Dispatching checker stage call"
    );

    tokio::runtime::Handle::current()
        .block_on(async {
            ctx.plugin_manager
                .call_raw(
                    &handler.plugin_id,
                    &function_name,
                    request_bytes,
                    plugin_core::executor::CallOptions::nested(),
                )
                .await
        })
        .map_err(|e| extism::Error::msg(format!("Checker stage plugin error: {e}")))
}

fn resolve_checker_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<Arc<CheckerStageContext>>,
) -> Result<(), extism::Error> {
    let input: ResolveCheckerInput = super::read_json(plugin, inputs, "input")?;
    let request_bytes = serde_json::to_vec(&input)
        .map_err(|e| extism::Error::msg(format!("Failed to serialize resolve input: {e}")))?;

    let output_bytes = checker_stage_call(
        &user_data,
        "resolve_checker",
        &input.format,
        |h| h.resolve_fn.clone(),
        request_bytes,
    )?;

    // Validate the plugin returned a well-formed CheckerStage before handing it
    // back, so a malformed stage fails here rather than deep in op assembly.
    let _stage: CheckerStage = serde_json::from_slice(&output_bytes)
        .map_err(|e| extism::Error::msg(format!("Failed to deserialize checker stage: {e}")))?;

    super::write_bytes(plugin, outputs, &output_bytes)
}

fn interpret_checker_fn(
    plugin: &mut extism::CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    user_data: UserData<Arc<CheckerStageContext>>,
) -> Result<(), extism::Error> {
    let input: InterpretCheckerInput = super::read_json(plugin, inputs, "input")?;
    let request_bytes = serde_json::to_vec(&input)
        .map_err(|e| extism::Error::msg(format!("Failed to serialize interpret input: {e}")))?;

    let output_bytes = checker_stage_call(
        &user_data,
        "interpret_checker_result",
        &input.format,
        |h| h.interpret_fn.clone(),
        request_bytes,
    )?;

    let _verdict: CheckerVerdict = serde_json::from_slice(&output_bytes)
        .map_err(|e| extism::Error::msg(format!("Failed to deserialize checker verdict: {e}")))?;

    super::write_bytes(plugin, outputs, &output_bytes)
}
