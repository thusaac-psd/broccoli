// The judged-column `SET`-list builder is single-sourced in broccoli-types so
// this guest SDK and the server host-fn mirror cannot drift; re-exported here
// for the historical `sdk::shared::push_judge_sets` path.
//
// Gated to match its only consumer, the wasm32-only `impl CodeRuns::update` in
// code_runs.rs. Without the gate this re-export is unused on host targets and
// every plugin's host-target build (`cargo test`, `cargo clippy`) emits an
// unused-import warning for it. That warning never failed CI because
// `-D warnings` applies to the crate being linted, not to its dependencies,
// and because the `guest` feature that plugins build the SDK with is not
// enabled by any workspace-scoped gate.
#[cfg(target_arch = "wasm32")]
pub(super) use crate::types::push_judge_sets;

#[cfg(target_arch = "wasm32")]
use crate::error::SdkError;
#[cfg(target_arch = "wasm32")]
use serde::de::DeserializeOwned;
#[cfg(target_arch = "wasm32")]
use serde_json::Value as JsonValue;

#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize)]
pub(super) struct HostDbResponse {
    pub data: Option<JsonValue>,
    pub error: Option<String>,
}

#[cfg(target_arch = "wasm32")]
impl HostDbResponse {
    pub fn into_result(self) -> Result<Option<JsonValue>, SdkError> {
        if let Some(err) = self.error {
            return Err(SdkError::Database(err));
        }
        Ok(self.data)
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn parse_rows<T: DeserializeOwned>(data: Option<JsonValue>) -> Result<Vec<T>, SdkError> {
    match data {
        Some(v) => Ok(serde_json::from_value(v)?),
        None => Ok(Vec::new()),
    }
}

#[cfg(target_arch = "wasm32")]
pub(super) fn parse_affected(data: Option<JsonValue>) -> u64 {
    data.and_then(|v| v.as_u64()).unwrap_or(0)
}

#[cfg(target_arch = "wasm32")]
pub(super) fn raw_execute(sql: &str, args: &[impl serde::Serialize]) -> Result<u64, SdkError> {
    let args_json = serde_json::to_string(args)?;
    let result_json = unsafe { crate::host::raw::db_execute(sql.to_string(), args_json)? };
    let resp: HostDbResponse = serde_json::from_str(&result_json)?;
    Ok(parse_affected(resp.into_result()?))
}
