use crate::error::SdkError;
use crate::types::{ResolveLanguageInput, ResolveLanguageOutput};

// No mock-state field: the host-target mock impl below always returns a
// fixed error, so there is nothing to record/replay and no consumer to
// cfg-gate a state field to. See checker.rs for the same reasoning -- a
// previous `#[cfg(not(target_arch = "wasm32"))] inner: LanguageMock` field
// was a zero-sized, never-read placeholder that warned on host builds.
pub struct Language {}

#[cfg(target_arch = "wasm32")]
impl Language {
    pub fn resolve(&self, input: &ResolveLanguageInput) -> Result<ResolveLanguageOutput, SdkError> {
        let response_json =
            unsafe { crate::host::raw::resolve_language(serde_json::to_string(input)?)? };
        Ok(serde_json::from_str(&response_json)?)
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Language {
    pub fn resolve(
        &self,
        _input: &ResolveLanguageInput,
    ) -> Result<ResolveLanguageOutput, SdkError> {
        Err(SdkError::Other("Mock language not implemented".into()))
    }
}
