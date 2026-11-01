use anyhow::Result;
use async_trait::async_trait;
use broccoli_types::types::HookResponse;
use common::event::GenericEvent;
use common::hook::{GenericHook, GenericHookAction, HookAction};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::retry::{PoolRetryPolicy, call_raw_with_pool_retry};
use crate::traits::PluginManager;

pub const PLUGIN_RUNTIME_ERROR_CODE: &str = "__BROCCOLI_PLUGIN_RUNTIME_ERROR";

#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HookScope {
    /// Runs where the plugin applies: for a problem, contest, or contest
    /// problem whose plugin config enables it, and always for submissions to
    /// a contest whose type this plugin registered. See
    /// `server::hooks::resource_enablements`.
    #[default]
    Resource,
    /// Runs for every event on the server.
    Global,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    #[default]
    Blocking,
    Notify,
}

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

/// Interpret a hook guest's raw output bytes into a host action.
///
/// Fail-closed on unparseable bytes (a broken guest must not silently pass a
/// Blocking gate); a valid-JSON wrong-shape response is a pass, since not every
/// plugin implements every hook.
fn interpret_hook_output(
    output_bytes: &[u8],
    topic: &str,
    plugin_id: &str,
    function_name: &str,
) -> GenericHookAction {
    match serde_json::from_slice::<serde_json::Value>(output_bytes) {
        Err(e) => {
            tracing::error!(
                plugin_id = %plugin_id,
                function = %function_name,
                "Hook returned non-JSON output (fail-closed): {e}",
            );
            let detail = serde_json::json!({
                "code": PLUGIN_RUNTIME_ERROR_CODE,
                "message": format!("Plugin '{plugin_id}' hook '{function_name}' returned a non-JSON response"),
                "status_code": 500,
            });
            GenericHookAction::Reject(detail.to_string())
        }
        Ok(value) => match serde_json::from_value::<HookResponse>(value) {
            Ok(hook_response) => into_hook_action(hook_response, topic),
            Err(e) => {
                tracing::warn!(
                    plugin_id = %plugin_id,
                    function = %function_name,
                    "Hook returned a wrong-shape response, treating as pass: {e}",
                );
                GenericHookAction::Pass
            }
        },
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
    type Context = ();

    fn id(&self) -> &str {
        &self.plugin_id
    }

    fn topics(&self) -> &[String] {
        &self.topics
    }

    async fn on_event(&self, _ctx: (), event: &GenericEvent) -> Result<GenericHookAction> {
        // Serialize the payload once outside the retry loop. A serialization
        // failure here is a genuine error (not transient backpressure), so we
        // fail-closed with Reject - matching the prior `call::<_, _>` path
        // which would have surfaced this as `PluginError::Serialization`.
        let input_bytes = match serde_json::to_vec(&event.payload) {
            Ok(bytes) => bytes,
            Err(e) => {
                tracing::error!(
                    plugin_id = %self.plugin_id,
                    function = %self.function_name,
                    "Failed to serialize hook payload (fail-closed): {e}",
                );
                let detail = serde_json::json!({
                    "code": PLUGIN_RUNTIME_ERROR_CODE,
                    "message": format!("Plugin '{}' hook '{}' failed: {e}", self.plugin_id, self.function_name),
                    "status_code": 500,
                });
                return Ok(GenericHookAction::Reject(detail.to_string()));
            }
        };

        // Pool timeouts are transient backpressure: every WASM instance is
        // busy but a free slot will appear shortly. Retrying transparently
        // here matches the synchronous evaluator path and prevents a
        // fail-closed Reject (HTTP 500) for what is purely a server-side
        // overload signal (UP#14h).
        let call_result = call_raw_with_pool_retry(
            self.plugin_manager.as_ref(),
            &self.plugin_id,
            &self.function_name,
            input_bytes,
            PoolRetryPolicy::default(),
        )
        .await;

        match call_result {
            Ok(output_bytes) => Ok(interpret_hook_output(
                &output_bytes,
                &event.topic,
                &self.plugin_id,
                &self.function_name,
            )),
            Err(e) => {
                tracing::error!(
                    plugin_id = %self.plugin_id,
                    function = %self.function_name,
                    "Hook WASM call failed (fail-closed): {e}",
                );
                let detail = serde_json::json!({
                    "code": PLUGIN_RUNTIME_ERROR_CODE,
                    "message": format!("Plugin '{}' hook '{}' failed: {e}", self.plugin_id, self.function_name),
                    "status_code": 500,
                });
                Ok(GenericHookAction::Reject(detail.to_string()))
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

    // Kernel audit (Nov 2026): the wrong-shape -> Pass arm above exists for
    // JSON with no `action` tag at all, but it also swallows responses that
    // clearly say `"action":"reject"` and merely mistype a field. Those must
    // not turn a Blocking gate (cooldown / submission-limit) into an allow.
    // Expected: Reject (either the plugin's own or a 500 PLUGIN_RUNTIME_ERROR,
    // matching the non-JSON fail-closed arm). Actual today: Pass.
    const MISTYPED_REJECTS: &[&[u8]] = &[
        br#"{"action":"reject","status_code":"429"}"#,
        br#"{"action":"reject","status_code":70000}"#,
        br#"{"action":"reject","message":null}"#,
        br#"{"action":"reject","code":42}"#,
    ];

    #[test]
    #[ignore = "BUG: interpret_hook_output fails OPEN on a reject with a mistyped field"]
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

    /// Pins today's (buggy) behaviour so a fix is a deliberate, visible change:
    /// delete this test together with the `#[ignore]` above when fixing.
    #[test]
    fn mistyped_reject_currently_passes() {
        for raw in MISTYPED_REJECTS {
            let action = interpret_hook_output(raw, TOPIC, "p", "f");
            assert!(matches!(action, GenericHookAction::Pass));
        }
    }
}
