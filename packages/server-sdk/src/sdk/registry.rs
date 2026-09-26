use crate::error::SdkError;

// No mock-state field: the host-target mock impl below always returns
// `Ok(())`, so there is nothing to record/replay and no consumer to
// cfg-gate a state field to. See checker.rs for the same reasoning -- a
// previous `#[cfg(not(target_arch = "wasm32"))] inner: RegistryMock` field
// was a zero-sized, never-read placeholder that warned on host builds.
pub struct Registry {}

#[cfg(target_arch = "wasm32")]
impl Registry {
    pub fn register_contest_type(
        &self,
        contest_type: &str,
        submission_handler: &str,
        code_run_handler: &str,
    ) -> Result<(), SdkError> {
        let input = serde_json::json!({
            "type": contest_type,
            "submission_handler": submission_handler,
            "code_run_handler": code_run_handler,
        });
        unsafe { crate::host::raw::register_contest_type(serde_json::to_string(&input)?)? };
        Ok(())
    }

    pub fn register_evaluator(&self, evaluator_type: &str, handler: &str) -> Result<(), SdkError> {
        let input = serde_json::json!({
            "type": evaluator_type,
            "handler": handler,
        });
        unsafe { crate::host::raw::register_evaluator(serde_json::to_string(&input)?)? };
        Ok(())
    }

    /// Register a checker resolver + interpreter for a format (checker fusion).
    /// `resolve_handler` builds the `CheckerStage`; `interpret_handler` turns the
    /// small check result into a verdict.
    pub fn register_checker_resolver(
        &self,
        format: &str,
        resolve_handler: &str,
        interpret_handler: &str,
    ) -> Result<(), SdkError> {
        let input = serde_json::json!({
            "format": format,
            "resolve_handler": resolve_handler,
            "interpret_handler": interpret_handler,
        });
        unsafe { crate::host::raw::register_checker_resolver(serde_json::to_string(&input)?)? };
        Ok(())
    }

    pub fn register_language_resolver(
        &self,
        language_id: &str,
        function_name: &str,
        display_name: &str,
        default_filename: &str,
        extensions: &[&str],
        template: &str,
    ) -> Result<(), SdkError> {
        let input = serde_json::json!({
            "language_id": language_id,
            "function_name": function_name,
            "display_name": display_name,
            "default_filename": default_filename,
            "extensions": extensions,
            "template": template,
        });
        unsafe { crate::host::raw::register_language_resolver(serde_json::to_string(&input)?)? };
        Ok(())
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Registry {
    pub fn register_contest_type(
        &self,
        _contest_type: &str,
        _submission_handler: &str,
        _code_run_handler: &str,
    ) -> Result<(), SdkError> {
        Ok(())
    }

    pub fn register_evaluator(
        &self,
        _evaluator_type: &str,
        _handler: &str,
    ) -> Result<(), SdkError> {
        Ok(())
    }

    pub fn register_checker_resolver(
        &self,
        _format: &str,
        _resolve_handler: &str,
        _interpret_handler: &str,
    ) -> Result<(), SdkError> {
        Ok(())
    }

    pub fn register_language_resolver(
        &self,
        _language_id: &str,
        _function_name: &str,
        _display_name: &str,
        _default_filename: &str,
        _extensions: &[&str],
        _template: &str,
    ) -> Result<(), SdkError> {
        Ok(())
    }
}
