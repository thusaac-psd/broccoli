//! The one rule for a plugin id, which every tool that names one applies:
//! the server's `PluginId`, plugin loading and upload, and the developer
//! CLI's scaffolding. An id is a path-safe name that speaks only for its
//! plugin, so it is never a namespace of the host's own messages.

use std::fmt;

include!(concat!(env!("OUT_DIR"), "/host_namespaces.rs"));

/// The namespace of identifier names (`identifier.<kind>.<id>`).
pub const IDENTIFIER_NAMESPACE: &str = "identifier";
/// The namespaces of a plugin's manifest text (`plugin.<id>.…`,
/// `config.<id>.…`).
pub const MANIFEST_TEXT_NAMESPACES: [&str; 2] = ["plugin", "config"];
/// The namespace of the host's error texts (`apiError.<CODE>`).
pub const API_ERROR_NAMESPACE: &str = "apiError";

/// Whether `id` is a namespace the host speaks in: the first segment of a key
/// of the host's catalog (derived from the English pack by `build.rs`), or a
/// key family the platform reads for every plugin (identifier names, manifest
/// text, error texts). A plugin's own keys are the ones under its id, so a
/// plugin with such an id would speak as the host; no plugin may take one
/// (compared ignoring ASCII case).
pub fn is_host_namespace(id: &str) -> bool {
    HOST_CATALOG_NAMESPACES
        .iter()
        .chain(&MANIFEST_TEXT_NAMESPACES)
        .chain(&[IDENTIFIER_NAMESPACE, API_ERROR_NAMESPACE])
        .any(|ns| ns.eq_ignore_ascii_case(id))
}

/// A string that is not a valid plugin id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidPluginId {
    /// Not a path-safe name.
    Shape(String),
    /// A namespace of the host's messages ([`is_host_namespace`]).
    HostNamespace(String),
}

impl fmt::Display for InvalidPluginId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shape(id) => write!(
                f,
                "Invalid plugin ID '{id}': it must be 1-128 characters, start with a letter or \
                 digit, and contain only letters, digits, '-' or '_'"
            ),
            Self::HostNamespace(id) => write!(
                f,
                "Invalid plugin ID '{id}': it names a namespace of the host's own messages"
            ),
        }
    }
}

impl std::error::Error for InvalidPluginId {}

/// Check `id` against the one plugin id rule: 1 to 128 characters, starting
/// with an ASCII letter or digit, otherwise only ASCII letters, digits, `_`
/// and `-`, and not a host namespace.
pub fn check_plugin_id(id: &str) -> Result<(), InvalidPluginId> {
    let shaped = id.len() <= 128
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !shaped {
        return Err(InvalidPluginId::Shape(id.to_string()));
    }
    if is_host_namespace(id) {
        return Err(InvalidPluginId::HostNamespace(id.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Every namespace the host's catalog speaks in, and every key family
    /// the platform reads for all plugins, is refused as a plugin id.
    #[test]
    fn no_plugin_id_names_a_host_namespace() {
        let catalog: toml::Table = include_str!(env!("HOST_CATALOG")).parse().unwrap();
        assert!(!catalog.is_empty());
        for key in catalog.keys() {
            let namespace = key.split('.').next().unwrap();
            for id in [namespace.to_string(), namespace.to_ascii_uppercase()] {
                assert_eq!(
                    check_plugin_id(&id),
                    Err(InvalidPluginId::HostNamespace(id.clone())),
                    "{key}"
                );
            }
        }
        for family in ["plugin", "config", "identifier", "apiError"] {
            assert!(is_host_namespace(family), "{family}");
        }
    }

    /// Every plugin in the repository, and every test fixture plugin, has an
    /// id the rule accepts.
    #[test]
    fn every_plugin_in_the_repository_has_a_valid_id() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut checked = 0;
        for dir in [
            "plugins",
            "packages/server/tests/fixtures",
            "packages/server/tests/fixtures_timer_negative",
        ] {
            for entry in std::fs::read_dir(root.join(dir)).expect(dir) {
                let path = entry.unwrap().path();
                if !path.join("plugin.toml").is_file() {
                    continue;
                }
                let id = path.file_name().unwrap().to_str().unwrap();
                assert_eq!(check_plugin_id(id), Ok(()), "{}", path.display());
                checked += 1;
            }
        }
        assert!(checked > 10, "found only {checked} plugins");
    }

    #[test]
    fn a_plugin_id_is_a_path_safe_name() {
        for ok in ["a", "Plugin_1-x", &"a".repeat(128)] {
            assert_eq!(check_plugin_id(ok), Ok(()), "{ok}");
        }
        for bad in ["", "_a", "-a", "a.b", "..", "a/b", "a b", &"a".repeat(129)] {
            assert_eq!(
                check_plugin_id(bad),
                Err(InvalidPluginId::Shape(bad.to_string())),
                "{bad}"
            );
        }
    }
}
