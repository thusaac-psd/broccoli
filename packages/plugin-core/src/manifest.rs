use std::collections::HashMap;
use std::fmt::Display;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::PluginError;
use crate::hook::{HookMode, HookScope};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: Option<String>,

    pub server: Option<ServerConfig>,

    pub worker: Option<WorkerConfig>,

    pub web: Option<WebConfig>,

    #[serde(default)]
    pub translations: HashMap<String, String>,

    #[serde(default)]
    pub config: HashMap<String, ConfigNamespace>,
}

impl PluginManifest {
    pub fn has_server(&self) -> bool {
        self.server.is_some()
    }

    pub fn has_worker(&self) -> bool {
        self.worker.is_some()
    }

    pub fn has_web(&self) -> bool {
        self.web.is_some()
    }

    pub fn has_translations(&self) -> bool {
        !self.translations.is_empty()
    }

    pub fn is_hollow(&self) -> bool {
        !self.has_server() && !self.has_worker() && !self.has_web() && !self.has_translations()
    }

    pub fn resolve_schema_includes(&mut self, root_dir: &Path) -> Result<(), PluginError> {
        let canonical_root = root_dir.canonicalize().map_err(|e| {
            PluginError::LoadFailed(format!(
                "Failed to canonicalize plugin root '{}': {}",
                root_dir.display(),
                e
            ))
        })?;

        for (ns_name, ns) in &mut self.config {
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
                let properties: HashMap<String, SchemaProperty> = toml::from_str(&content)
                    .map_err(|e| {
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
}

impl Display for PluginManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (v{})", self.name, self.version)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerConfig {
    pub entry: String,

    #[serde(default)]
    pub permissions: Vec<String>,

    #[serde(default)]
    pub routes: Vec<ServerRouteConfig>,

    #[serde(default)]
    pub hooks: Vec<HookDeclaration>,

    #[serde(default)]
    pub queries: Vec<ServerQuery>,

    #[serde(default)]
    pub timers: Vec<ServerTimer>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct HookDeclaration {
    pub topic: String,
    pub function: String,
    #[serde(default)]
    pub scope: HookScope,
    #[serde(default)]
    pub mode: HookMode,
}

/// A `[[server.queries]]` declaration. Unlike `[[server.hooks]]` (which
/// answers "should this event proceed"), a query answers "here is a decision
/// for each of these resources" -- e.g. the visibility kernel's per-resource
/// access query. Declaring a query is itself the opt-in; it requires no
/// separate permission grant.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerQuery {
    pub topic: String,
    pub function: String,
}

/// A `[[server.timers]]` declaration: the function invoked when one of this
/// plugin's scheduled timers comes due. A plugin has at most one, and
/// declaring it is the opt-in for receiving callbacks -- scheduling them
/// additionally requires the `timer` permission.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServerTimer {
    pub function: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, utoipa::ToSchema)]
pub struct ServerRouteConfig {
    pub method: String,

    pub path: String,

    pub handler: String,

    #[serde(default)]
    pub permission: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WorkerConfig {
    pub entry: String,

    #[serde(default)]
    pub permissions: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct WebConfig {
    pub root: String,

    pub entry: String,

    #[serde(default)]
    pub components: ComponentMap,

    #[serde(default)]
    pub slots: Vec<WebSlotConfig>,

    #[serde(default)]
    pub routes: Vec<WebRouteConfig>,

    #[serde(default)]
    pub css: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, utoipa::ToSchema, Default)]
#[schema(example = json!({
    "MyComponent": "MyComponent",
    "MyPage": "MyPage"
}))]
pub struct ComponentMap(HashMap<String, String>);

#[derive(Debug, Deserialize, Serialize, Clone, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum WebSlotPosition {
    Append,
    Prepend,
    Replace,
    Before,
    After,
    Wrap,
}

#[derive(Debug, Deserialize, Serialize, Clone, utoipa::ToSchema)]
pub struct WebSlotConfig {
    pub name: String,

    pub position: WebSlotPosition,

    pub component: String,

    pub priority: Option<u32>,

    #[serde(default)]
    pub permission: Option<String>,

    /// Restrict rendering to contests of this type. When set, the host only
    /// renders this slot entry on contest pages whose contest matches this
    /// type (e.g. "ioi", "icpc"). When unset, the slot is rendered everywhere.
    #[serde(default)]
    pub contest_type: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, utoipa::ToSchema)]
pub struct WebRouteConfig {
    pub path: String,

    pub component: String,

    pub meta: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfigNamespace {
    pub description: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    pub schema: Option<String>,
    #[serde(default)]
    pub properties: HashMap<String, SchemaProperty>,
}

impl ConfigNamespace {
    pub fn defaults(&self) -> serde_json::Value {
        let mut obj = serde_json::Map::new();
        for (key, prop) in &self.properties {
            if let Some(val) = prop.default_value() {
                obj.insert(key.clone(), val);
            }
        }
        serde_json::Value::Object(obj)
    }

    pub fn to_json_schema(&self) -> serde_json::Value {
        let mut schema = serde_json::Map::new();
        schema.insert("type".into(), "object".into());
        if let Some(ref desc) = self.description {
            schema.insert("description".into(), desc.clone().into());
        }
        if !self.properties.is_empty() {
            let mut props = serde_json::Map::new();
            for (name, prop) in &self.properties {
                props.insert(name.clone(), prop.to_json_schema());
            }
            schema.insert("properties".into(), props.into());
        }
        schema.into()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SchemaProperty {
    #[serde(rename = "type")]
    pub schema_type: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub default: Option<serde_json::Value>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
    pub pattern: Option<String>,
    pub format: Option<String>,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<serde_json::Value>>,
    pub items: Option<Box<SchemaProperty>>,
    #[serde(default)]
    pub properties: HashMap<String, SchemaProperty>,
    pub required: Option<Vec<String>>,
    pub additional_properties: Option<bool>,
    pub step: Option<f64>,
    pub precision: Option<u32>,
    pub unit: Option<String>,
    pub span: Option<u8>,
}

impl SchemaProperty {
    pub fn default_value(&self) -> Option<serde_json::Value> {
        if let Some(ref d) = self.default {
            return Some(d.clone());
        }
        if self.schema_type == "object" && !self.properties.is_empty() {
            let mut obj = serde_json::Map::new();
            for (key, prop) in &self.properties {
                if let Some(val) = prop.default_value() {
                    obj.insert(key.clone(), val);
                }
            }
            if !obj.is_empty() {
                return Some(serde_json::Value::Object(obj));
            }
        }
        None
    }

    pub fn to_json_schema(&self) -> serde_json::Value {
        let mut schema = serde_json::Map::new();
        schema.insert("type".into(), self.schema_type.clone().into());

        if let Some(ref v) = self.title {
            schema.insert("title".into(), v.clone().into());
        }
        if let Some(ref v) = self.description {
            schema.insert("description".into(), v.clone().into());
        }
        if let Some(ref v) = self.default {
            schema.insert("default".into(), v.clone());
        }
        if let Some(v) = self.min {
            schema.insert("minimum".into(), v.into());
        }
        if let Some(v) = self.max {
            schema.insert("maximum".into(), v.into());
        }
        if let Some(v) = self.min_length {
            schema.insert("minLength".into(), v.into());
        }
        if let Some(v) = self.max_length {
            schema.insert("maxLength".into(), v.into());
        }
        if let Some(ref v) = self.pattern {
            schema.insert("pattern".into(), v.clone().into());
        }
        if let Some(ref v) = self.format {
            schema.insert("format".into(), v.clone().into());
        }
        if let Some(ref v) = self.enum_values {
            schema.insert("enum".into(), v.clone().into());
        }
        if let Some(ref v) = self.items {
            schema.insert("items".into(), v.to_json_schema());
        }
        if !self.properties.is_empty() {
            let mut props = serde_json::Map::new();
            for (name, prop) in &self.properties {
                props.insert(name.clone(), prop.to_json_schema());
            }
            schema.insert("properties".into(), props.into());
        }
        if let Some(ref v) = self.required {
            schema.insert("required".into(), v.clone().into());
        }
        if let Some(v) = self.additional_properties {
            schema.insert("additionalProperties".into(), v.into());
        }
        if let Some(v) = self.step {
            schema.insert("multipleOf".into(), v.into());
        }
        if let Some(v) = self.precision {
            schema.insert("x-precision".into(), v.into());
        }
        if let Some(ref v) = self.unit {
            schema.insert("x-unit".into(), v.clone().into());
        }
        if let Some(v) = self.span {
            schema.insert("x-span".into(), v.into());
        }

        schema.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_manifest_with_inline_config_schema() {
        let toml_str = r#"
            name = "test-plugin"
            version = "1.0.0"

            [config.my-ns]
            description = "Test config"
            scopes = ["plugin", "problem"]

            [config.my-ns.properties.timeout]
            type = "number"
            title = "Timeout"
            default = 30.0
            min = 0.0
            max = 300.0
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.config.len(), 1);

        let ns = &manifest.config["my-ns"];
        assert_eq!(ns.description.as_deref(), Some("Test config"));
        assert_eq!(ns.scopes, vec!["plugin", "problem"]);
        assert_eq!(ns.properties.len(), 1);

        let prop = &ns.properties["timeout"];
        assert_eq!(prop.schema_type, "number");
        assert_eq!(prop.title.as_deref(), Some("Timeout"));
        assert_eq!(prop.default, Some(json!(30.0)));
        assert_eq!(prop.min, Some(0.0));
        assert_eq!(prop.max, Some(300.0));
    }

    #[test]
    fn parse_manifest_without_config() {
        let toml_str = r#"
            name = "test-plugin"
            version = "1.0.0"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert!(manifest.config.is_empty());
    }

    #[test]
    fn schema_property_to_json_schema_basic() {
        let prop = SchemaProperty {
            schema_type: "string".into(),
            title: Some("Name".into()),
            description: Some("A name field".into()),
            default: Some(json!("hello")),
            min: None,
            max: None,
            min_length: Some(1),
            max_length: Some(100),
            pattern: Some("^[a-z]+$".into()),
            format: None,
            enum_values: None,
            items: None,
            properties: HashMap::new(),
            required: None,
            additional_properties: None,
            step: None,
            precision: None,
            unit: None,
            span: None,
        };

        let schema = prop.to_json_schema();
        assert_eq!(schema["type"], "string");
        assert_eq!(schema["title"], "Name");
        assert_eq!(schema["description"], "A name field");
        assert_eq!(schema["default"], "hello");
        assert_eq!(schema["minLength"], 1);
        assert_eq!(schema["maxLength"], 100);
        assert_eq!(schema["pattern"], "^[a-z]+$");
    }

    #[test]
    fn schema_property_to_json_schema_nested_object() {
        let toml_str = r#"
            name = "test-plugin"
            version = "1.0.0"

            [config.settings]
            description = "Settings"
            scopes = ["plugin"]

            [config.settings.properties.compiler]
            type = "object"
            title = "Compiler"
            required = ["path"]

            [config.settings.properties.compiler.properties.path]
            type = "string"
            default = "/usr/bin/gcc"

            [config.settings.properties.compiler.properties.flags]
            type = "array"
            default = ["-O2"]
            items = { type = "string" }
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let ns = &manifest.config["settings"];
        let schema = ns.to_json_schema();

        assert_eq!(schema["type"], "object");
        let compiler = &schema["properties"]["compiler"];
        assert_eq!(compiler["type"], "object");
        assert_eq!(compiler["required"], json!(["path"]));

        let path_prop = &compiler["properties"]["path"];
        assert_eq!(path_prop["type"], "string");
        assert_eq!(path_prop["default"], "/usr/bin/gcc");

        let flags_prop = &compiler["properties"]["flags"];
        assert_eq!(flags_prop["type"], "array");
        assert_eq!(flags_prop["default"], json!(["-O2"]));
        assert_eq!(flags_prop["items"]["type"], "string");
    }

    #[test]
    fn schema_property_to_json_schema_enum_values() {
        let prop = SchemaProperty {
            schema_type: "string".into(),
            title: Some("Mode".into()),
            description: None,
            default: Some(json!("fast")),
            min: None,
            max: None,
            min_length: None,
            max_length: None,
            pattern: None,
            format: None,
            enum_values: Some(vec![json!("fast"), json!("slow"), json!("balanced")]),
            items: None,
            properties: HashMap::new(),
            required: None,
            additional_properties: None,
            step: None,
            precision: None,
            unit: None,
            span: None,
        };

        let schema = prop.to_json_schema();
        assert_eq!(schema["enum"], json!(["fast", "slow", "balanced"]));
    }

    #[test]
    fn schema_property_minimal_produces_minimal_schema() {
        let toml_str = r#"
            type = "string"
        "#;

        let prop: SchemaProperty = toml::from_str(toml_str).unwrap();
        let schema = prop.to_json_schema();

        let obj = schema.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert_eq!(obj["type"], "string");
    }

    #[test]
    fn config_namespace_defaults_collects_property_defaults() {
        let toml_str = r#"
            name = "test"
            version = "1.0.0"

            [config.cooldown]
            scopes = ["problem"]

            [config.cooldown.properties.cooldown_seconds]
            type = "integer"
            default = 60

            [config.cooldown.properties.enabled]
            type = "boolean"
            default = true

            [config.cooldown.properties.label]
            type = "string"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let defaults = manifest.config["cooldown"].defaults();

        assert_eq!(defaults["cooldown_seconds"], 60);
        assert_eq!(defaults["enabled"], true);
        assert!(defaults.get("label").is_none());
    }

    #[test]
    fn config_namespace_defaults_recurses_into_nested_objects() {
        let toml_str = r#"
            name = "test"
            version = "1.0.0"

            [config.testlib]
            scopes = ["plugin"]

            [config.testlib.properties.compile_time_limit_s]
            type = "number"
            default = 10.0

            [config.testlib.properties.cpp]
            type = "object"

            [config.testlib.properties.cpp.properties.compiler]
            type = "string"
            default = "/usr/bin/g++"

            [config.testlib.properties.cpp.properties.flags]
            type = "array"
            default = ["-O2", "-std=c++17"]
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let defaults = manifest.config["testlib"].defaults();

        assert_eq!(defaults["compile_time_limit_s"], 10.0);
        assert_eq!(defaults["cpp"]["compiler"], "/usr/bin/g++");
        assert_eq!(defaults["cpp"]["flags"], json!(["-O2", "-std=c++17"]));
    }

    #[test]
    fn config_namespace_defaults_empty_when_no_defaults() {
        let toml_str = r#"
            name = "test"
            version = "1.0.0"

            [config.bare]
            scopes = ["plugin"]

            [config.bare.properties.name]
            type = "string"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let defaults = manifest.config["bare"].defaults();
        assert_eq!(defaults, json!({}));
    }

    #[test]
    fn schema_property_default_value_prefers_explicit_over_recursive() {
        let toml_str = r#"
            type = "object"
            default = { key = "explicit" }

            [properties.key]
            type = "string"
            default = "recursive"
        "#;

        let prop: SchemaProperty = toml::from_str(toml_str).unwrap();
        assert_eq!(prop.default_value(), Some(json!({"key": "explicit"})));
    }

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
            worker: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::from([(
                "settings".into(),
                ConfigNamespace {
                    description: Some("External".into()),
                    scopes: vec!["plugin".into()],
                    schema: Some("config/settings.schema.toml".into()),
                    properties: HashMap::new(),
                },
            )]),
        };

        manifest.resolve_schema_includes(dir.path()).unwrap();

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
            worker: None,
            web: None,
            translations: HashMap::new(),
            config: HashMap::from([(
                "evil".into(),
                ConfigNamespace {
                    description: None,
                    scopes: vec!["plugin".into()],
                    schema: Some("../secret.toml".into()),
                    properties: HashMap::new(),
                },
            )]),
        };

        let err = manifest.resolve_schema_includes(&plugin_dir).unwrap_err();
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
        assert_eq!(testlib.scopes, vec!["plugin"]);
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

    #[test]
    fn server_hooks_section_parses_topic_function_and_scope() {
        let toml_str = r#"
            name = "cooldown"
            version = "0.1.0"

            [server]
            entry = "cooldown.wasm"
            permissions = ["sql", "logger"]

            [[server.hooks]]
            topic = "before_submission"
            function = "check_cooldown"
            scope = "resource"

            [[server.hooks]]
            topic = "before_submission"
            function = "global_audit"
            scope = "global"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let server = manifest.server.unwrap();
        assert_eq!(server.hooks.len(), 2);

        assert_eq!(server.hooks[0].topic, "before_submission");
        assert_eq!(server.hooks[0].function, "check_cooldown");
        assert_eq!(server.hooks[0].scope, HookScope::Resource);
        assert_eq!(server.hooks[0].mode, HookMode::Blocking);

        assert_eq!(server.hooks[1].topic, "before_submission");
        assert_eq!(server.hooks[1].function, "global_audit");
        assert_eq!(server.hooks[1].scope, HookScope::Global);
        assert_eq!(server.hooks[1].mode, HookMode::Blocking);
    }

    #[test]
    fn hook_scope_defaults_to_resource_when_omitted() {
        let toml_str = r#"
            name = "test"
            version = "1.0.0"

            [server]
            entry = "test.wasm"

            [[server.hooks]]
            topic = "before_submission"
            function = "my_hook"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let server = manifest.server.unwrap();
        assert_eq!(server.hooks.len(), 1);
        assert_eq!(server.hooks[0].scope, HookScope::Resource);
        assert_eq!(server.hooks[0].mode, HookMode::Blocking);
    }

    #[test]
    fn manifest_without_hooks_section_has_empty_hooks_list() {
        let toml_str = r#"
            name = "test"
            version = "1.0.0"

            [server]
            entry = "test.wasm"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let server = manifest.server.unwrap();
        assert!(server.hooks.is_empty());
    }

    #[test]
    fn hook_mode_parses_notify_and_blocking_values() {
        let toml_str = r#"
            name = "audit"
            version = "0.1.0"

            [server]
            entry = "audit.wasm"

            [[server.hooks]]
            topic = "after_submission"
            function = "log_submission"
            scope = "resource"
            mode = "notify"

            [[server.hooks]]
            topic = "before_submission"
            function = "check_access"
            mode = "blocking"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let server = manifest.server.unwrap();
        assert_eq!(server.hooks.len(), 2);

        assert_eq!(server.hooks[0].topic, "after_submission");
        assert_eq!(server.hooks[0].mode, HookMode::Notify);
        assert_eq!(server.hooks[0].scope, HookScope::Resource);

        assert_eq!(server.hooks[1].topic, "before_submission");
        assert_eq!(server.hooks[1].mode, HookMode::Blocking);
    }

    #[test]
    fn hook_mode_defaults_to_blocking_when_omitted() {
        let toml_str = r#"
            name = "test"
            version = "1.0.0"

            [server]
            entry = "test.wasm"

            [[server.hooks]]
            topic = "after_judging"
            function = "on_judged"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        let server = manifest.server.unwrap();
        assert_eq!(server.hooks[0].mode, HookMode::Blocking);
    }

    #[test]
    fn parses_server_queries_section() {
        let toml = r#"
name = "t"
version = "0.1.0"

[server]
entry = "t.wasm"
permissions = ["logger"]

[[server.queries]]
topic = "visibility"
function = "decide_visibility"
"#;
        let manifest: PluginManifest = toml::from_str(toml).expect("manifest parses");
        let queries = &manifest.server.as_ref().unwrap().queries;
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].topic, "visibility");
        assert_eq!(queries[0].function, "decide_visibility");
    }

    #[test]
    fn manifest_without_queries_defaults_to_empty() {
        let toml = r#"
name = "t"
version = "0.1.0"

[server]
entry = "t.wasm"
permissions = ["logger"]
"#;
        let manifest: PluginManifest = toml::from_str(toml).expect("manifest parses");
        assert!(manifest.server.as_ref().unwrap().queries.is_empty());
    }

    #[test]
    fn parses_server_timers_section() {
        let toml = r#"
name = "p"
version = "0.1.0"

[server]
entry = "p.wasm"

[[server.timers]]
function = "on_timer"
"#;
        let manifest: PluginManifest = toml::from_str(toml).expect("manifest parses");
        let timers = &manifest.server.as_ref().unwrap().timers;
        assert_eq!(timers.len(), 1);
        assert_eq!(timers[0].function, "on_timer");
    }

    #[test]
    fn manifest_without_timers_defaults_to_empty() {
        let toml = r#"
name = "p"
version = "0.1.0"

[server]
entry = "p.wasm"
"#;
        let manifest: PluginManifest = toml::from_str(toml).expect("manifest parses");
        assert!(manifest.server.as_ref().unwrap().timers.is_empty());
    }
}
