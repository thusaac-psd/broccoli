use crate::error::SdkError;
#[cfg(target_arch = "wasm32")]
use crate::types::InterpretCheckerInput;
use crate::types::{CheckerRunOutcome, CheckerStage, CheckerVerdict, ResolveCheckerInput};

// No mock-state field: unlike `Db`/`Eval`/`Config`/etc., the host-target mock
// impl below has nothing to record or replay -- it always returns a fixed
// error -- so there is no wasm32-or-otherwise consumer to cfg-gate a state
// field to. A previous `#[cfg(not(target_arch = "wasm32"))] inner: CheckerMock`
// field carried a zero-sized, never-read placeholder and triggered a
// "field is never read" warning on host builds.
pub struct Checker {}

#[cfg(target_arch = "wasm32")]
impl Checker {
    /// Checker fusion: ask the registered checker plugin to resolve a stage to
    /// splice into the run op. `input.format` selects the checker.
    pub fn resolve(&self, input: &ResolveCheckerInput) -> Result<CheckerStage, SdkError> {
        let response_json =
            unsafe { crate::host::raw::resolve_checker(serde_json::to_string(input)?)? };
        Ok(serde_json::from_str(&response_json)?)
    }

    /// Checker fusion: turn the check step's small result into a verdict via the
    /// registered checker plugin's interpreter.
    pub fn interpret(
        &self,
        format: &str,
        result: &CheckerRunOutcome,
    ) -> Result<CheckerVerdict, SdkError> {
        let input = InterpretCheckerInput {
            format: format.to_string(),
            result: result.clone(),
        };
        let response_json =
            unsafe { crate::host::raw::interpret_checker_result(serde_json::to_string(&input)?)? };
        Ok(serde_json::from_str(&response_json)?)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Checker {
    pub fn resolve(&self, _input: &ResolveCheckerInput) -> Result<CheckerStage, SdkError> {
        Err(SdkError::Other("Mock checker not implemented".into()))
    }

    pub fn interpret(
        &self,
        _format: &str,
        _result: &CheckerRunOutcome,
    ) -> Result<CheckerVerdict, SdkError> {
        Err(SdkError::Other("Mock checker not implemented".into()))
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::types::JudgeFile;

    fn checker() -> Checker {
        Checker {}
    }

    #[test]
    fn resolve_mock_returns_err_off_wasm() {
        let input = ResolveCheckerInput {
            format: "tokens".to_string(),
            answer: JudgeFile::inline("ans\n"),
            test_input: JudgeFile::Missing,
            problem_id: None,
            config: None,
            output_binding: crate::types::OutputMode::Stream {
                channel: "verdict".to_string(),
            },
        };
        assert!(checker().resolve(&input).is_err());
    }

    #[test]
    fn interpret_mock_returns_err_off_wasm() {
        let result = CheckerRunOutcome {
            exit_code: Some(0),
            stderr: String::new(),
            sandbox_status: Some("OK".to_string()),
        };
        assert!(checker().interpret("tokens", &result).is_err());
    }
}
