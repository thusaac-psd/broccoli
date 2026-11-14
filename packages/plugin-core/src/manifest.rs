//! The manifest types live in `broccoli_types::manifest` so tooling outside
//! the host parses `plugin.toml` with the same definitions; this module adds
//! the host-side loading steps.

use std::collections::HashMap;
use std::path::Path;

pub use broccoli_types::manifest::*;

use crate::error::PluginError;

/// Parses a `plugin.toml`. Config schema includes are left unresolved (see
/// [`resolve_schema_includes`]).
pub fn parse_manifest(toml_content: &str) -> Result<PluginManifest, PluginError> {
    let manifest: PluginManifest = toml::from_str(toml_content)
        .map_err(|e| PluginError::LoadFailed(format!("Invalid manifest syntax: {}", e)))?;
    if let Some(server) = &manifest.server
        && !server.sql.read.is_empty()
        && !server.permissions.iter().any(|p| p == "sql")
    {
        return Err(PluginError::LoadFailed(
            "[server.sql] read scopes need the \"sql\" permission".into(),
        ));
    }
    Ok(manifest)
}

/// Replaces the properties of every config namespace that names an external
/// `schema` file with the properties read from that file. Schema paths are
/// resolved against `root_dir` and must stay inside it.
pub fn resolve_schema_includes(
    manifest: &mut PluginManifest,
    root_dir: &Path,
) -> Result<(), PluginError> {
    let canonical_root = root_dir.canonicalize().map_err(|e| {
        PluginError::LoadFailed(format!(
            "Failed to canonicalize plugin root '{}': {}",
            root_dir.display(),
            e
        ))
    })?;

    for (ns_name, ns) in &mut manifest.config {
        if let Some(ref schema_path) = ns.schema {
            let full_path = root_dir.join(schema_path);
            let canonical_path = full_path.canonicalize().map_err(|e| {
                PluginError::LoadFailed(format!(
                    "Failed to resolve schema file '{}' for config namespace '{}': {}",
                    full_path.display(),
                    ns_name,
                    e
                ))
            })?;
            if !canonical_path.starts_with(&canonical_root) {
                return Err(PluginError::LoadFailed(format!(
                    "Schema path '{}' for config namespace '{}' escapes plugin directory",
                    schema_path, ns_name
                )));
            }
            let content = std::fs::read_to_string(&canonical_path).map_err(|e| {
                PluginError::LoadFailed(format!(
                    "Failed to read schema file '{}' for config namespace '{}': {}",
                    full_path.display(),
                    ns_name,
                    e
                ))
            })?;
            let properties: HashMap<String, SchemaProperty> =
                toml::from_str(&content).map_err(|e| {
                    PluginError::LoadFailed(format!(
                        "Invalid schema file '{}' for config namespace '{}': {}",
                        full_path.display(),
                        ns_name,
                        e
                    ))
                })?;
            ns.properties = properties;
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_schema_includes_loads_external_file() {
        let dir = tempfile::tempdir().unwrap();
        let schema_dir = dir.path().join("config");
        std::fs::create_dir_all(&schema_dir).unwrap();

        std::fs::write(
            schema_dir.join("settings.schema.toml"),
            r#"
                [timeout]
                type = "number"
                default = 30.0

                [name]
                type = "string"
                default = "test"
            "#,
        )
        .unwrap();

        let mut manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            description: None,
            server: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::from([(
                "settings".into(),
                ConfigNamespace {
                    description: Some("External".into()),
                    scopes: vec![ConfigScope::Plugin],
                    schema: Some("config/settings.schema.toml".into()),
                    properties: HashMap::new(),
                },
            )]),
        };

        resolve_schema_includes(&mut manifest, dir.path()).unwrap();

        let ns = &manifest.config["settings"];
        assert_eq!(ns.properties.len(), 2);
        assert_eq!(ns.properties["timeout"].schema_type, "number");
        assert_eq!(ns.properties["name"].schema_type, "string");
    }

    #[test]
    fn resolve_schema_includes_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let outside_file = dir.path().join("secret.toml");
        std::fs::write(
            &outside_file,
            r#"
                [timeout]
                type = "number"
            "#,
        )
        .unwrap();

        let plugin_dir = dir.path().join("my-plugin");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        let mut manifest = PluginManifest {
            name: "test".into(),
            version: "1.0.0".into(),
            description: None,
            server: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::from([(
                "evil".into(),
                ConfigNamespace {
                    description: None,
                    scopes: vec![ConfigScope::Plugin],
                    schema: Some("../secret.toml".into()),
                    properties: HashMap::new(),
                },
            )]),
        };

        let err = resolve_schema_includes(&mut manifest, &plugin_dir).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes plugin directory"),
            "Expected path traversal error, got: {}",
            msg,
        );
    }

    #[test]
    fn standard_checkers_manifest_parses() {
        let toml_content = include_str!("../../../plugins/standard-checkers/plugin.toml");
        let manifest: PluginManifest = toml::from_str(toml_content).unwrap();

        assert_eq!(manifest.name, "standard-checkers");

        let server = manifest.server.unwrap();
        assert!(server.permissions.contains(&"config:read".to_string()));

        let testlib = &manifest.config["testlib"];
        assert_eq!(testlib.scopes, vec![ConfigScope::Plugin]);
        assert!(testlib.properties.contains_key("compile_time_limit_s"));
        assert!(testlib.properties.contains_key("cpp"));

        let schema = testlib.to_json_schema();
        assert_eq!(schema["type"], "object");
        assert!(
            schema["properties"]["compile_time_limit_s"]["minimum"]
                .as_f64()
                .is_some()
        );

        let compile_time = &schema["properties"]["compile_time_limit_s"];
        assert_eq!(compile_time["multipleOf"], 0.5);
        assert_eq!(compile_time["x-precision"], 1);
        assert_eq!(compile_time["x-unit"], "s");

        let compile_mem = &schema["properties"]["compile_memory_limit_kb"];
        assert_eq!(compile_mem["multipleOf"], 1024.0);
        assert_eq!(compile_mem["x-unit"], "KB");
        assert!(compile_mem.get("x-precision").is_none());
    }

    const SQL_PLUGIN: &str = r#"
name = "p"
version = "1.0.0"
[server]
entry = "p.wasm"
permissions = ["sql"]
"#;

    #[test]
    fn a_declared_sql_read_scope_parses() {
        let manifest = parse_manifest(&format!(
            "{SQL_PLUGIN}[server.sql]\nread = [\"submission.files\"]\n"
        ))
        .expect("a grantable scope");
        let read = &manifest.server.unwrap().sql.read;
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].table(), read[0].column()), ("submission", "files"));
    }

    #[test]
    fn no_sql_section_grants_no_content_columns() {
        let manifest = parse_manifest(SQL_PLUGIN).expect("manifest");
        assert!(manifest.server.unwrap().sql.read.is_empty());
    }

    #[test]
    fn an_unknown_sql_read_scope_is_rejected() {
        for scope in [
            "user.password",
            "refresh_tokens.token_hash",
            "submission",
            "nope.x",
        ] {
            let err = parse_manifest(&format!("{SQL_PLUGIN}[server.sql]\nread = [\"{scope}\"]\n"))
                .expect_err(scope)
                .to_string();
            assert!(err.contains("unknown SQL read scope"), "{scope}: {err}");
        }
        let err = parse_manifest(&format!("{SQL_PLUGIN}[server.sql]\nwrite = []\n"))
            .expect_err("unknown key")
            .to_string();
        assert!(err.contains("unknown field"), "{err}");
    }

    #[test]
    fn read_scopes_without_the_sql_permission_are_rejected() {
        let manifest = SQL_PLUGIN.replace(r#"["sql"]"#, r#"["logger"]"#);
        let err = parse_manifest(&format!(
            "{manifest}[server.sql]\nread = [\"submission.files\"]\n"
        ))
        .expect_err("scope without the permission")
        .to_string();
        assert!(err.contains("\"sql\" permission"), "{err}");
    }
}
