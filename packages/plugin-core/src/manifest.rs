//! The manifest types live in `broccoli_types::manifest` so tooling outside
//! the host parses `plugin.toml` with the same definitions; this module adds
//! the host-side loading steps.

use std::collections::HashMap;
use std::path::Path;

pub use broccoli_types::manifest::*;

use crate::error::PluginError;

/// Parses a `plugin.toml`. Config schema includes are left unresolved (see
/// [`resolve_schema_includes_with`]).
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
    let checker_sources: Vec<(&String, &ConfigNamespace)> = manifest
        .config
        .iter()
        .filter(|(_, ns)| ns.checker_source)
        .collect();
    if checker_sources.len() > 1 {
        return Err(PluginError::LoadFailed(
            "at most one config namespace may set checker_source".into(),
        ));
    }
    if let Some((name, ns)) = checker_sources.first()
        && ns.scopes != [ConfigScope::Problem]
    {
        return Err(PluginError::LoadFailed(format!(
            "config namespace '{name}' sets checker_source, so its scopes must be [\"problem\"]"
        )));
    }
    if let Some(server) = &manifest.server {
        for route in &server.routes {
            route.access().map_err(|e| {
                PluginError::LoadFailed(format!(
                    "Invalid route {} {}: {}",
                    route.method, route.path, e
                ))
            })?;
        }
    }
    Ok(manifest)
}

/// A reader of the files in plugin directory `root_dir`, given each path
/// as the manifest names it, relative to the directory. A path that leaves
/// the directory (through `..` or a link) is refused.
pub fn dir_reader(
    root_dir: &Path,
) -> Result<impl FnMut(&str) -> Result<String, String> + use<'_>, PluginError> {
    let canonical_root = root_dir.canonicalize().map_err(|e| {
        PluginError::LoadFailed(format!(
            "Failed to canonicalize plugin root '{}': {}",
            root_dir.display(),
            e
        ))
    })?;
    Ok(move |path: &str| {
        let full_path = root_dir.join(path);
        let canonical_path = full_path
            .canonicalize()
            .map_err(|e| format!("Failed to resolve '{}': {e}", full_path.display()))?;
        if !canonical_path.starts_with(&canonical_root) {
            return Err(format!("'{path}' escapes plugin directory"));
        }
        std::fs::read_to_string(&canonical_path)
            .map_err(|e| format!("Failed to read '{}': {e}", full_path.display()))
    })
}

/// Replaces the properties of every config namespace that names an external
/// `schema` file with the properties read from that file, through `read`
/// (given each `schema` path as the manifest names it), so a plugin that is
/// not on disk yet (an uploaded archive) resolves its includes exactly as
/// loading it will.
pub fn resolve_schema_includes_with(
    manifest: &mut PluginManifest,
    mut read: impl FnMut(&str) -> Result<String, String>,
) -> Result<(), PluginError> {
    for (ns_name, ns) in &mut manifest.config {
        if let Some(ref schema_path) = ns.schema {
            let content = read(schema_path).map_err(|e| {
                PluginError::LoadFailed(format!(
                    "Schema file for config namespace '{ns_name}': {e}"
                ))
            })?;
            let properties: HashMap<String, SchemaProperty> =
                toml::from_str(&content).map_err(|e| {
                    PluginError::LoadFailed(format!(
                        "Invalid schema file '{schema_path}' for config namespace '{ns_name}': {e}"
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
    fn a_schema_include_loads_its_file() {
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
            language_pack: None,
            config: HashMap::from([(
                "settings".into(),
                ConfigNamespace {
                    description: Some("External".into()),
                    scopes: vec![ConfigScope::Plugin],
                    schema: Some("config/settings.schema.toml".into()),
                    properties: HashMap::new(),
                    checker_source: false,
                    judging_input: false,
                    checker_languages: Default::default(),
                    validate: None,
                },
            )]),
        };

        resolve_schema_includes_with(&mut manifest, dir_reader(dir.path()).unwrap()).unwrap();

        let ns = &manifest.config["settings"];
        assert_eq!(ns.properties.len(), 2);
        assert_eq!(ns.properties["timeout"].schema_type, "number");
        assert_eq!(ns.properties["name"].schema_type, "string");
    }

    #[test]
    fn a_schema_include_outside_the_plugin_is_refused() {
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
            language_pack: None,
            config: HashMap::from([(
                "evil".into(),
                ConfigNamespace {
                    description: None,
                    scopes: vec![ConfigScope::Plugin],
                    schema: Some("../secret.toml".into()),
                    properties: HashMap::new(),
                    checker_source: false,
                    judging_input: false,
                    checker_languages: Default::default(),
                    validate: None,
                },
            )]),
        };

        let err = resolve_schema_includes_with(&mut manifest, dir_reader(&plugin_dir).unwrap())
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("escapes plugin directory"),
            "Expected path traversal error, got: {}",
            msg,
        );
    }

    fn with_namespaces(namespaces: &str) -> Result<PluginManifest, PluginError> {
        parse_manifest(&format!(
            "name = \"demo\"\nversion = \"0.1.0\"\n{namespaces}"
        ))
    }

    #[test]
    fn a_checker_source_namespace_is_problem_scoped_and_unique() {
        let one =
            with_namespaces("[config.sources]\nscopes = [\"problem\"]\nchecker_source = true\n")
                .unwrap();
        assert!(one.config["sources"].checker_source);

        let wrong_scope = with_namespaces(
            "[config.sources]\nscopes = [\"problem\", \"contest\"]\nchecker_source = true\n",
        );
        assert!(wrong_scope.is_err());

        let two = with_namespaces(
            "[config.a]\nscopes = [\"problem\"]\nchecker_source = true\n\
             [config.b]\nscopes = [\"problem\"]\nchecker_source = true\n",
        );
        assert!(two.is_err());
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
