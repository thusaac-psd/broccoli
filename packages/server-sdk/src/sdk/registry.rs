use crate::error::SdkError;

// No mock-state field: the host-target mock impl below always returns
// `Ok(())`, so there is nothing to record/replay and no consumer to
// cfg-gate a state field to. See checker.rs for the same reasoning -- a
// previous `#[cfg(not(target_arch = "wasm32"))] inner: RegistryMock` field
// was a zero-sized, never-read placeholder that warned on host builds.
pub struct Registry {}

#[cfg(target_arch = "wasm32")]
use crate::types::host_io::{
    EvaluatorHandlers, RegisterCheckerResolverInput, RegisterContestTypeInput,
    RegisterEvaluatorInput, RegisterLanguageResolverInput,
};

#[cfg(target_arch = "wasm32")]
impl Registry {
    pub fn register_contest_type(
        &self,
        contest_type: &str,
        submission_handler: &str,
        code_run_handler: &str,
    ) -> Result<(), SdkError> {
        let input = RegisterContestTypeInput {
            contest_type: contest_type.to_string(),
            submission_handler: submission_handler.to_string(),
            code_run_handler: code_run_handler.to_string(),
            submission_admission: false,
        };
        unsafe { crate::host::raw::register_contest_type(serde_json::to_string(&input)?)? };
        Ok(())
    }

    pub fn register_contest_type_with_admission(
        &self,
        contest_type: &str,
        submission_handler: &str,
        code_run_handler: &str,
    ) -> Result<(), SdkError> {
        let input = RegisterContestTypeInput {
            contest_type: contest_type.to_string(),
            submission_handler: submission_handler.to_string(),
            code_run_handler: code_run_handler.to_string(),
            submission_admission: true,
        };
        unsafe { crate::host::raw::register_contest_type(serde_json::to_string(&input)?)? };
        Ok(())
    }

    /// Register `handler` to evaluate test cases of `evaluator_type`, one
    /// call per case. The handler starts and awaits its own operations.
    /// `uses_checker` says whether it checks output with the problem's
    /// checker (`resolve_checker`): work on its problems needs a checker
    /// plugin only when it does.
    pub fn register_evaluator(
        &self,
        evaluator_type: &str,
        handler: &str,
        uses_checker: bool,
    ) -> Result<(), SdkError> {
        let input = RegisterEvaluatorInput {
            problem_type: evaluator_type.to_string(),
            handlers: EvaluatorHandlers::Direct {
                handler: handler.to_string(),
            },
            uses_checker,
        };
        unsafe { crate::host::raw::register_evaluator(serde_json::to_string(&input)?)? };
        Ok(())
    }

    /// Register a staged evaluator for `evaluator_type`. The host calls
    /// `prepare_handler` with a `BuildEvalOpsInput` and expects a
    /// `PreparedEvaluateCase`, runs its operations itself (holding no plugin
    /// instance while they run), then calls `interpret_handler` with an
    /// `EvaluateOperationResultsInput` covering several finished cases and
    /// expects one `TestCaseVerdict` per case. `uses_checker` is as for
    /// [`Self::register_evaluator`].
    pub fn register_staged_evaluator(
        &self,
        evaluator_type: &str,
        prepare_handler: &str,
        interpret_handler: &str,
        uses_checker: bool,
    ) -> Result<(), SdkError> {
        let input = RegisterEvaluatorInput {
            problem_type: evaluator_type.to_string(),
            handlers: EvaluatorHandlers::Staged {
                prepare_handler: prepare_handler.to_string(),
                interpret_handler: interpret_handler.to_string(),
            },
            uses_checker,
        };
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
        let input = RegisterCheckerResolverInput {
            checker_format: format.to_string(),
            resolve_handler: resolve_handler.to_string(),
            interpret_handler: interpret_handler.to_string(),
        };
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
        let input = RegisterLanguageResolverInput {
            language_id: language_id.to_string(),
            function_name: function_name.to_string(),
            display_name: display_name.to_string(),
            default_filename: default_filename.to_string(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            template: template.to_string(),
        };
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

    pub fn register_contest_type_with_admission(
        &self,
        contest_type: &str,
        submission_handler: &str,
        code_run_handler: &str,
    ) -> Result<(), SdkError> {
        self.register_contest_type(contest_type, submission_handler, code_run_handler)
    }

    pub fn register_evaluator(
        &self,
        _evaluator_type: &str,
        _handler: &str,
        _uses_checker: bool,
    ) -> Result<(), SdkError> {
        Ok(())
    }

    pub fn register_staged_evaluator(
        &self,
        _evaluator_type: &str,
        _prepare_handler: &str,
        _interpret_handler: &str,
        _uses_checker: bool,
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
