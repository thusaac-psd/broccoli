pub mod api;
pub mod config;
pub mod judge;
pub mod standings;

#[cfg(target_arch = "wasm32")]
mod plugin {
    use broccoli_server_sdk::prelude::*;
    use extism_pdk::{FnResult, plugin_fn};

    #[plugin_fn]
    pub fn init() -> FnResult<String> {
        let host = Host::new();
        host.registry.register_contest_type(
            "codelink",
            "handle_codelink_submission",
            "handle_codelink_code_run",
        )?;
        Ok("ok".into())
    }

    #[plugin_fn]
    pub fn handle_codelink_submission(input: String) -> FnResult<String> {
        let host = Host::new();
        let req: OnSubmissionInput = serde_json::from_str(&input)?;
        let output = match crate::judge::judge(&host, &req) {
            Ok(()) | Err(SdkError::StaleEpoch) => OnSubmissionOutput {
                success: true,
                error_message: None,
            },
            Err(e) => OnSubmissionOutput {
                success: false,
                error_message: Some(e.to_string()),
            },
        };
        Ok(serde_json::to_string(&output)?)
    }

    #[plugin_fn]
    pub fn on_codelink_eval_result(input: String) -> FnResult<String> {
        let input: DetachedEvaluateCallbackInput = serde_json::from_str(&input)?;
        let output = crate::judge::handle_eval_callback(&Host::new(), input);
        Ok(serde_json::to_string(&output)?)
    }

    #[plugin_fn]
    pub fn handle_codelink_code_run(input: String) -> FnResult<String> {
        Ok(broccoli_server_sdk::evaluator::handle_code_run(
            &Host::new(),
            &input,
        )?)
    }

    #[plugin_fn]
    pub fn api_contest_info(input: String) -> FnResult<String> {
        run_api_handler(&input, crate::api::handle_contest_info)
    }

    #[plugin_fn]
    pub fn api_standings(input: String) -> FnResult<String> {
        run_api_handler(&input, crate::api::handle_standings)
    }
}
