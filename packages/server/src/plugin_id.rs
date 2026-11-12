//! [`PluginId`]: the only form in which a plugin id reaches a filesystem
//! path. Every path the host builds from a plugin id (the bundled plugin
//! directory, the upload cache) takes a `PluginId`, so a string from a URL,
//! a database row or an archive has to pass [`PluginId::parse`] first.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

/// A plugin id that is safe as a single path segment: 1 to 128 characters,
/// starting with an ASCII letter or digit, otherwise only ASCII letters,
/// digits, `_` and `-`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct PluginId(String);

/// A string that is not a valid [`PluginId`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Invalid plugin ID '{0}': it must be 1-128 characters, start with a letter or digit, and \
     contain only letters, digits, '-' or '_'"
)]
pub struct InvalidPluginId(pub String);

impl PluginId {
    pub fn parse(id: impl Into<String>) -> Result<Self, InvalidPluginId> {
        let id = id.into();
        let valid = id.len() <= 128
            && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if valid {
            Ok(Self(id))
        } else {
            Err(InvalidPluginId(id))
        }
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

    #[test]
    fn accepts_path_safe_ids_only() {
        for ok in ["a", "Plugin_1-x", &"a".repeat(128)] {
            assert!(PluginId::parse(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "_a",
            "-a",
            "a.b",
            "..",
            "a/b",
            "../var/cache",
            "a b",
            &"a".repeat(129),
        ] {
            assert!(PluginId::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn deserializing_validates() {
        let ok: PluginId = serde_json::from_str("\"demo\"").unwrap();
        assert_eq!(ok.as_str(), "demo");
        assert!(serde_json::from_str::<PluginId>("\"../demo\"").is_err());
    }
}
