//! Contest helpers for plugins. A route's contest, its phase and the
//! caller's access to it come from the host (`PluginHttpRequest::contest`),
//! never from plugin SQL.

#[cfg(feature = "guest")]
use crate::error::SdkError;

/// The contest's `contest` config namespace as the plugin's config type, with
/// the schema's defaults filled in by the host.
#[cfg(feature = "guest")]
pub fn load_config<T: serde::de::DeserializeOwned>(
    host: &crate::sdk::Host,
    contest_id: i32,
) -> Result<T, SdkError> {
    host.config.get_contest(contest_id, "contest")?.parse()
}
