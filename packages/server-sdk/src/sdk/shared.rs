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
