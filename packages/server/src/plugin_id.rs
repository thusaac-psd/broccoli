//! [`PluginId`]: the only form in which a plugin id reaches a filesystem
//! path. Every path the host builds from a plugin id (the bundled plugin
//! directory, the upload cache) takes a `PluginId`, so a string from a URL,
//! a database row or an archive has to pass [`PluginId::parse`] first.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

pub use broccoli_api_types::plugin_id::InvalidPluginId;

/// A plugin id that passes the one plugin id rule
/// ([`broccoli_api_types::plugin_id::check_plugin_id`]): safe as a single path
/// segment, and speaking only for its plugin, never in a namespace of the
/// host's messages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PluginId(String);

impl PluginId {
    pub fn parse(id: impl Into<String>) -> Result<Self, InvalidPluginId> {
        let id = id.into();
        broccoli_api_types::plugin_id::check_plugin_id(&id)?;
        Ok(Self(id))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::ops::Deref for PluginId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PluginId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(raw).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PluginId` is the one rule: a path-unsafe id and a host namespace
    /// are refused as `check_plugin_id` refuses them.
    #[test]
    fn a_plugin_id_passes_the_one_rule() {
        assert_eq!(
            PluginId::parse("../var/cache"),
            Err(InvalidPluginId::Shape("../var/cache".into()))
        );
        assert_eq!(
            PluginId::parse("apiError"),
            Err(InvalidPluginId::HostNamespace("apiError".into()))
        );
        assert_eq!(PluginId::parse("ioi").unwrap().as_str(), "ioi");
    }

    #[test]
    fn deserializing_validates() {
        let ok: PluginId = serde_json::from_str("\"demo\"").unwrap();
        assert_eq!(ok.as_str(), "demo");
        assert!(serde_json::from_str::<PluginId>("\"../demo\"").is_err());
    }
}
