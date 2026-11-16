//! Contest helpers for plugins. A route's contest, its phase and the
//! caller's access to it come from the host (`PluginHttpRequest::contest`),
//! never from plugin SQL.

#[cfg(feature = "guest")]
use crate::error::SdkError;

#[cfg(feature = "guest")]
pub fn load_config<T: serde::de::DeserializeOwned + Default>(
    host: &crate::sdk::Host,
    contest_id: i32,
) -> Result<T, SdkError> {
    Ok(
        serde_json::from_value(host.config.get_contest(contest_id, "contest")?.config)
            .unwrap_or_default(),
    )
}
