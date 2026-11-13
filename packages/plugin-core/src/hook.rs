use anyhow::Result;
use async_trait::async_trait;
use broccoli_types::types::HookResponse;
use common::event::GenericEvent;
use common::hook::{GenericHook, GenericHookAction, HookAction};
use std::sync::Arc;

pub use broccoli_types::manifest::{HookMode, HookScope};

use crate::executor::CallOptions;
use crate::traits::PluginManager;

pub const PLUGIN_RUNTIME_ERROR_CODE: &str = "__BROCCOLI_PLUGIN_RUNTIME_ERROR";

/// Map a plugin's `HookResponse` onto the host's hook action. Free function
/// (rather than an inherent impl) because `HookResponse` is the shared
/// guest<->host contract type owned by `broccoli-types` (re-exported via `broccoli-server-sdk`).
fn into_hook_action(response: HookResponse, original_topic: &str) -> GenericHookAction {
    match response {
        HookResponse::Pass => HookAction::Pass,
        HookResponse::Stop => HookAction::Stop,
        HookResponse::Reject {
            code,
            details,
            message,
            status_code,
        } => {
            let mut detail = serde_json::json!({
                "code": code,
                "message": message,
                "status_code": status_code,
            });
            if let Some(d) = details {
                detail["details"] = d;
            }
            HookAction::Reject(detail.to_string())
        }
        HookResponse::Modified { event } => {
            let generic = GenericEvent {
                topic: event
                    .get("topic")
                    .and_then(|t| t.as_str())
                    .unwrap_or(original_topic)
                    .to_string(),
                payload: event,
            };
            HookAction::Modified(generic)
        }
    }
}

/// The fail-closed rejection used whenever a hook cannot produce a trustworthy
/// answer (payload serialization, WASM call failure, malformed output). One
/// constructor so every fail-closed path carries the same code and status.
fn runtime_error_reject(plugin_id: &str, function_name: &str, reason: &str) -> GenericHookAction {
    let detail = serde_json::json!({
        "code": PLUGIN_RUNTIME_ERROR_CODE,
        "message": format!("Plugin '{plugin_id}' hook '{function_name}' failed: {reason}"),
        "status_code": 500,
    });
    GenericHookAction::Reject(detail.to_string())
}

/// Interpret a hook guest's raw output bytes into a host action.
///
/// The only output that passes without being a valid `HookResponse` is a JSON
/// object with no `action` key at all (a plugin that declines to answer). Once
/// a guest names an action it has committed to the contract, so anything that
/// then fails to deserialize (a mistyped `status_code`, an unknown action, a
/// `null` message) fails closed exactly like non-JSON output: a Blocking gate
/// must never turn a malformed reject into an allow.
fn interpret_hook_output(
    output_bytes: &[u8],
    topic: &str,
    plugin_id: &str,
    function_name: &str,
) -> GenericHookAction {
    let value = match serde_json::from_slice::<serde_json::Value>(output_bytes) {
        Ok(value) => value,
        Err(e) => {
            tracing::error!(
                plugin_id = %plugin_id,
                function = %function_name,
                "Hook returned non-JSON output (fail-closed): {e}",
            );
            return runtime_error_reject(plugin_id, function_name, "returned a non-JSON response");
        }
    };
    if value.get("action").is_none() {
        tracing::warn!(
            plugin_id = %plugin_id,
            function = %function_name,
            "Hook returned a response without an `action`, treating as pass",
        );
        return GenericHookAction::Pass;
    }
    match serde_json::from_value::<HookResponse>(value) {
        Ok(hook_response) => into_hook_action(hook_response, topic),
        Err(e) => {
            tracing::error!(
                plugin_id = %plugin_id,
                function = %function_name,
                "Hook returned a malformed response (fail-closed): {e}",
            );
            runtime_error_reject(
                plugin_id,
                function_name,
                &format!("returned a malformed response: {e}"),
            )
        }
    }
}

pub struct PluginHook<M: PluginManager + ?Sized> {
    plugin_manager: Arc<M>,
    plugin_id: String,
    function_name: String,
    topics: Vec<String>,
    pub scope: HookScope,
    pub mode: HookMode,
}

impl<M: PluginManager + ?Sized> PluginHook<M> {
    pub fn new(
        plugin_manager: Arc<M>,
        plugin_id: String,
        function_name: String,
        topics: Vec<String>,
        scope: HookScope,
        mode: HookMode,
    ) -> Self {
        Self {
            plugin_manager,
            plugin_id,
            function_name,
            topics,
            scope,
            mode,
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    pub fn mode(&self) -> &HookMode {
        &self.mode
    }
}

#[async_trait]
impl<M: PluginManager + Send + Sync + ?Sized + 'static> GenericHook for PluginHook<M> {
    /// The dispatch site's class and deadline: the same hook serves request
    /// paths and background notifications.
    type Context = CallOptions;

    fn id(&self) -> &str {
        &self.plugin_id
    }

    fn topics(&self) -> &[String] {
        &self.topics
    }

    async fn on_event(
        &self,
        options: CallOptions,
        event: &GenericEvent,
    ) -> Result<GenericHookAction> {
        // A payload that does not serialize fails closed like any other
        // hook that cannot produce a trustworthy answer.
        let input_bytes = match serde_json::to_vec(&event.payload) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(
                    plugin_id = %self.plugin_id,
                    function = %self.function_name,
                    "Failed to serialize hook payload (fail-closed): {e}",
                );
                return Ok(runtime_error_reject(
                    &self.plugin_id,
                    &self.function_name,
                    &e.to_string(),
                ));
            }
        };

        let call_result = self
            .plugin_manager
            .call_raw(&self.plugin_id, &self.function_name, input_bytes, options)
            .await;

        match call_result {
            Ok(output_bytes) => Ok(interpret_hook_output(
                &output_bytes,
                &event.topic,
                &self.plugin_id,
                &self.function_name,
            )),
            // Not an answer from the plugin: the caller turns it into "try
            // again", never into a decision.
            Err(e) if e.is_unavailable() => Err(e.into()),
            Err(e) => {
                tracing::error!(
                    plugin_id = %self.plugin_id,
                    function = %self.function_name,
                    "Hook WASM call failed (fail-closed): {e}",
                );
                Ok(runtime_error_reject(
                    &self.plugin_id,
                    &self.function_name,
                    &e.to_string(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOPIC: &str = "before_submission";

    #[test]
    fn non_json_output_fails_closed_reject() {
        // The regression guard: a broken guest emitting garbage must REJECT so a
        // Blocking gate (cooldown / submission-limit) blocks, not silently passes.
        let action = interpret_hook_output(b"kaboom, not json", TOPIC, "p", "f");
        assert!(matches!(action, GenericHookAction::Reject(_)));
    }

    #[test]
    fn empty_output_fails_closed_reject() {
        let action = interpret_hook_output(b"", TOPIC, "p", "f");
        assert!(matches!(action, GenericHookAction::Reject(_)));
    }

    #[test]
    fn wrong_shape_json_passes() {
        // Valid JSON with no `action` tag is not a HookResponse -> pass (a plugin
        // that does not implement this hook must not block the request).
        let action = interpret_hook_output(br#"{"unrelated":true}"#, TOPIC, "p", "f");
        assert!(matches!(action, GenericHookAction::Pass));
    }

    #[test]
    fn valid_reject_response_rejects() {
        let action =
            interpret_hook_output(br#"{"action":"reject","status_code":429}"#, TOPIC, "p", "f");
        let GenericHookAction::Reject(detail) = action else {
            panic!("expected reject");
        };
        assert!(detail.contains("429"));
    }

    #[test]
    fn valid_pass_response_passes() {
        let action = interpret_hook_output(br#"{"action":"pass"}"#, TOPIC, "p", "f");
        assert!(matches!(action, GenericHookAction::Pass));
    }

    // A response that names an action but
    // mistypes a field must not turn a Blocking gate (cooldown /
    // submission-limit) into an allow; it fails closed like non-JSON output.
    const MISTYPED_REJECTS: &[&[u8]] = &[
        br#"{"action":"reject","status_code":"429"}"#,
        br#"{"action":"reject","status_code":70000}"#,
        br#"{"action":"reject","message":null}"#,
        br#"{"action":"reject","code":42}"#,
        br#"{"action":"rejected"}"#,
    ];

    #[test]
    fn mistyped_reject_fails_closed() {
        for raw in MISTYPED_REJECTS {
            let action = interpret_hook_output(raw, TOPIC, "p", "f");
            assert!(
                matches!(action, GenericHookAction::Reject(_)),
                "{} was not interpreted as Reject",
                String::from_utf8_lossy(raw)
            );
        }
    }

    #[test]
    fn malformed_output_reject_carries_runtime_error_code() {
        let action = interpret_hook_output(MISTYPED_REJECTS[0], TOPIC, "p", "f");
        let GenericHookAction::Reject(detail) = action else {
            panic!("expected reject");
        };
        let detail: serde_json::Value = serde_json::from_str(&detail).unwrap();
        assert_eq!(detail["code"], PLUGIN_RUNTIME_ERROR_CODE);
        assert_eq!(detail["status_code"], 500);
    }
}
