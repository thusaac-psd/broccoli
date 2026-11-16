//! The `plugin.toml` manifest format, shared by the host that loads plugins
//! and the developer tooling that builds and packages them.

use std::collections::HashMap;
use std::fmt::Display;

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: Option<String>,

    pub server: Option<ServerConfig>,

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

    pub fn has_web(&self) -> bool {
        self.web.is_some()
    }

    pub fn has_translations(&self) -> bool {
        !self.translations.is_empty()
    }

    /// Paths, relative to the plugin directory, of the files and
    /// directories the host loads for this plugin besides `plugin.toml`: the
    /// server entry point, the web root, translation files and
    /// included config schemas. Together they identify what a replica would
    /// actually run, without sweeping in build output or sources that happen
    /// to sit in the same directory.
    pub fn loaded_paths(&self) -> Vec<&str> {
        let mut paths = Vec::new();
        if let Some(server) = &self.server {
            paths.push(server.entry.as_str());
        }
        if let Some(web) = &self.web {
            paths.push(web.root.as_str());
        }
        paths.extend(self.translations.values().map(String::as_str));
        paths.extend(self.config.values().filter_map(|ns| ns.schema.as_deref()));
        paths.sort_unstable();
        paths.dedup();
        paths
    }

    /// The permissions this plugin's routes require. Together with the host
    /// catalog ([`crate::permissions::ALL`]) they are every permission a role
    /// grant can mean something for.
    pub fn declared_permissions(&self) -> impl Iterator<Item = &str> {
        self.server
            .iter()
            .flat_map(|server| &server.routes)
            .filter_map(|route| route.permission.as_deref())
    }

    pub fn is_hollow(&self) -> bool {
        !self.has_server() && !self.has_web() && !self.has_translations()
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

    /// `[server.sql]`: what the plugin's raw SQL may read beyond the default.
    #[serde(default)]
    pub sql: SqlConfig,
}

/// Host content columns, as `table.column`, that a plugin's SQL reads only
/// when its manifest declares them under `[server.sql] read`: contestant
/// source, test data, program output and message bodies. The other columns
/// of a plugin-readable host table are readable to every plugin with the
/// `sql` permission. Credentials are in no table a plugin can read and are
/// never grantable.
pub const SQL_READ_SCOPES: &[&str] = &[
    "clarification.content",
    "clarification_reply.content",
    "code_run.compile_output",
    "code_run.custom_test_cases",
    "code_run.files",
    "code_run_result.checker_output",
    "code_run_result.stderr",
    "code_run_result.stdout",
    "problem.content",
    "submission.compile_output",
    "submission.files",
    "submission_judgement.compile_output",
    "test_case.expected_output",
    "test_case.expected_output_blob_hash",
    "test_case.expected_output_preview",
    "test_case.input",
    "test_case.input_blob_hash",
    "test_case.input_preview",
    "test_case_result.checker_output",
    "test_case_result.stderr",
    "test_case_result.stdout",
];

/// `[server.sql]`.
#[derive(Debug, Default, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SqlConfig {
    /// Content columns the plugin reads, each one of [`SQL_READ_SCOPES`].
    #[serde(default)]
    pub read: Vec<SqlReadScope>,
}

/// One entry of [`SQL_READ_SCOPES`]; any other string fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct SqlReadScope(String);

impl SqlReadScope {
    pub fn parse(scope: &str) -> Result<Self, String> {
        if SQL_READ_SCOPES.contains(&scope) {
            Ok(Self(scope.to_string()))
        } else {
            Err(format!(
                "unknown SQL read scope '{scope}'; the grantable scopes are: {}",
                SQL_READ_SCOPES.join(", ")
            ))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The host table the scope belongs to.
    pub fn table(&self) -> &str {
        self.0.split_once('.').map_or("", |(table, _)| table)
    }

    /// The column it grants.
    pub fn column(&self) -> &str {
        self.0.split_once('.').map_or("", |(_, column)| column)
    }
}

impl<'de> Deserialize<'de> for SqlReadScope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
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

#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HookScope {
    /// Runs where the plugin applies: for a problem, contest, or contest
    /// problem whose plugin config enables it, and always for submissions to
    /// a contest whose type this plugin registered. See
    /// `server::hooks::resource_enablements`.
    #[default]
    Resource,
    /// Runs for every event on the server.
    Global,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum HookMode {
    #[default]
    Blocking,
    Notify,
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

/// A `[[server.routes]]` entry, as written in `plugin.toml`. The host checks
/// who may call the route and reads the resources it addresses before the
/// plugin runs.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(deny_unknown_fields)]
pub struct ServerRouteConfig {
    pub method: String,

    pub path: String,

    pub handler: String,

    /// Who may call the route. Defaults to `user`.
    #[serde(default)]
    pub access: RouteAccessLevel,

    /// A permission the caller must hold.
    #[serde(default)]
    pub permission: Option<String>,

    /// The path parameter naming the contest the route acts on, as
    /// `"{contest_id}"`. The caller must be able to read the contest.
    #[serde(default)]
    pub contest: Option<String>,

    /// The path parameter naming the problem the route acts on, as
    /// `"{problem_id}"`: a problem of the route's `contest` when it declares
    /// one, else a standalone problem. The caller must be able to read the
    /// problem there.
    #[serde(default)]
    pub problem: Option<String>,

    /// The contest type the route serves. A contest of another type is not
    /// found.
    #[serde(default)]
    pub contest_type: Option<String>,
}

/// The `access` key of a route.
#[derive(Debug, Default, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum RouteAccessLevel {
    /// Anyone, signed in or not. The route addresses no contest or problem
    /// and needs no permission: core resources are never readable
    /// anonymously.
    Public,
    /// A signed-in caller.
    #[default]
    User,
    /// A signed-in participant of the route's contest, or a caller holding
    /// `contest:manage`.
    Participant,
}

/// Who may call a route and what it addresses, checked by the host before
/// the plugin runs. Built by [`ServerRouteConfig::access`], which refuses
/// every combination of keys the host cannot enforce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteAccess {
    /// Anyone. A bearer token, when sent, must still be valid.
    Public,
    /// A signed-in caller with a fresh token.
    SignedIn {
        permission: Option<String>,
        target: RouteTarget,
    },
}

/// The resource a signed-in route addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteTarget {
    /// No core resource.
    Nothing,
    /// A standalone problem, named by the path parameter `problem`.
    Problem { problem: String },
    /// A contest, and optionally one of its problems.
    Contest(ContestTarget),
}

/// A contest a route addresses; see [`RouteTarget::Contest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContestTarget {
    /// The path parameter naming the contest.
    pub contest: String,
    /// The path parameter naming a problem of the contest.
    pub problem: Option<String>,
    /// The only contest type the route serves.
    pub contest_type: Option<String>,
    /// The caller must be registered in the contest or hold
    /// `contest:manage`.
    pub participant: bool,
}

impl ServerRouteConfig {
    /// The checked form of this route's access keys, or why they do not make
    /// a rule the host can enforce.
    pub fn access(&self) -> Result<RouteAccess, String> {
        let param = |key: &str, value: &Option<String>| -> Result<Option<String>, String> {
            let Some(value) = value else { return Ok(None) };
            let name = value
                .strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
                .filter(|name| !name.is_empty())
                .ok_or_else(|| format!("{key} must name a path parameter as \"{{name}}\""))?;
            if !self.path.split('/').any(|segment| segment == value) {
                return Err(format!("{key} names {value}, which is not in the path"));
            }
            Ok(Some(name.to_string()))
        };
        let contest = param("contest", &self.contest)?;
        let problem = param("problem", &self.problem)?;
        if self.contest_type.is_some() && contest.is_none() {
            return Err("contest_type needs contest".into());
        }
        match self.access {
            RouteAccessLevel::Public => {
                if contest.is_some() || problem.is_some() || self.permission.is_some() {
                    return Err(
                        "a public route cannot name a contest, a problem or a permission".into(),
                    );
                }
                Ok(RouteAccess::Public)
            }
            RouteAccessLevel::User | RouteAccessLevel::Participant => {
                let participant = self.access == RouteAccessLevel::Participant;
                let target = match (contest, problem) {
                    (Some(contest), problem) => RouteTarget::Contest(ContestTarget {
                        contest,
                        problem,
                        contest_type: self.contest_type.clone(),
                        participant,
                    }),
                    (None, _) if participant => {
                        return Err("participant access needs contest".into());
                    }
                    (None, Some(problem)) => RouteTarget::Problem { problem },
                    (None, None) => RouteTarget::Nothing,
                };
                Ok(RouteAccess::SignedIn {
                    permission: self.permission.clone(),
                    target,
                })
            }
        }
    }
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

#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "utoipa", schema(example = json!({
    "MyComponent": "MyComponent",
    "MyPage": "MyPage"
})))]
pub struct ComponentMap(HashMap<String, String>);

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "lowercase")]
pub enum WebSlotPosition {
    Append,
    Prepend,
    Replace,
    Before,
    After,
    Wrap,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
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

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebRouteConfig {
    pub path: String,

    pub component: String,

    pub meta: Option<HashMap<String, String>>,
}

/// What a stored plugin config belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub enum ConfigScope {
    /// The plugin as a whole (its global settings).
    Plugin,
    Problem,
    Contest,
    /// One problem inside one contest.
    ContestProblem,
}

impl ConfigScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plugin => "plugin",
            Self::Problem => "problem",
            Self::Contest => "contest",
            Self::ContestProblem => "contest_problem",
        }
    }

    pub fn parse(scope: &str) -> Option<Self> {
        [
            Self::Plugin,
            Self::Problem,
            Self::Contest,
            Self::ContestProblem,
        ]
        .into_iter()
        .find(|s| s.as_str() == scope)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ConfigNamespace {
    pub description: Option<String>,
    /// Where this namespace can be configured. The host refuses a config for
    /// it anywhere else.
    #[serde(default)]
    pub scopes: Vec<ConfigScope>,
    pub schema: Option<String>,
    #[serde(default)]
    pub properties: HashMap<String, SchemaProperty>,
}

impl ConfigNamespace {
    /// The config with nothing stored: every default the schema declares.
    pub fn defaults(&self) -> serde_json::Value {
        self.resolve(None)
    }

    /// The config a plugin reads: `stored` with every default the schema
    /// declares filled in where `stored` leaves the field out, inside nested
    /// objects and array items too.
    pub fn resolve(&self, stored: Option<&serde_json::Value>) -> serde_json::Value {
        let mut fields = match stored {
            Some(serde_json::Value::Object(fields)) => fields.clone(),
            Some(other) => return other.clone(),
            None => serde_json::Map::new(),
        };
        fill_defaults(&self.properties, &mut fields);
        serde_json::Value::Object(fields)
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
            schema.insert("additionalProperties".into(), false.into());
        }
        schema.into()
    }
}

fn fill_defaults(
    properties: &HashMap<String, SchemaProperty>,
    fields: &mut serde_json::Map<String, serde_json::Value>,
) {
    for (name, prop) in properties {
        match fields.get_mut(name) {
            Some(value) => fill_value(prop, value),
            None => {
                if let Some(mut default) = prop.default_value() {
                    fill_value(prop, &mut default);
                    fields.insert(name.clone(), default);
                }
            }
        }
    }
}

fn fill_value(prop: &SchemaProperty, value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => fill_defaults(&prop.properties, fields),
        serde_json::Value::Array(items) => {
            if let Some(item) = &prop.items {
                for v in items {
                    fill_value(item, v);
                }
            }
        }
        _ => {}
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
        // The host rejects undeclared fields of an object that declares its
        // fields, unless it opts into additional ones.
        match self.additional_properties {
            Some(v) => {
                schema.insert("additionalProperties".into(), v.into());
            }
            None if !self.properties.is_empty() => {
                schema.insert("additionalProperties".into(), false.into());
            }
            None => {}
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

    fn route(extra: &str) -> Result<RouteAccess, String> {
        let toml_str = format!(
            "method = \"GET\"\npath = \"/contests/{{contest_id}}/problems/{{problem_id}}\"\nhandler = \"h\"\n{extra}"
        );
        let route: ServerRouteConfig = toml::from_str(&toml_str).map_err(|e| e.to_string())?;
        route.access()
    }

    #[test]
    fn route_without_access_keys_needs_a_signed_in_caller() {
        assert_eq!(
            route(""),
            Ok(RouteAccess::SignedIn {
                permission: None,
                target: RouteTarget::Nothing
            })
        );
    }

    #[test]
    fn route_names_its_contest_and_problem_by_path_parameter() {
        assert_eq!(
            route(
                "access = \"participant\"\ncontest = \"{contest_id}\"\nproblem = \"{problem_id}\"\ncontest_type = \"ioi\""
            ),
            Ok(RouteAccess::SignedIn {
                permission: None,
                target: RouteTarget::Contest(ContestTarget {
                    contest: "contest_id".into(),
                    problem: Some("problem_id".into()),
                    contest_type: Some("ioi".into()),
                    participant: true,
                }),
            })
        );
        assert_eq!(
            route("problem = \"{problem_id}\""),
            Ok(RouteAccess::SignedIn {
                permission: None,
                target: RouteTarget::Problem {
                    problem: "problem_id".into()
                },
            })
        );
    }

    #[test]
    fn route_keys_the_host_cannot_enforce_are_refused() {
        for extra in [
            "contest = \"contest_id\"",
            "contest = \"{cid}\"",
            "contest_type = \"ioi\"",
            "access = \"participant\"",
            "access = \"public\"\ncontest = \"{contest_id}\"",
            "access = \"public\"\nproblem = \"{problem_id}\"",
            "access = \"public\"\npermission = \"contest:manage\"",
            "acess = \"public\"",
            "access = \"anyone\"",
        ] {
            assert!(route(extra).is_err(), "accepted: {extra}");
        }
        assert_eq!(route("access = \"public\""), Ok(RouteAccess::Public));
    }

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
        assert_eq!(ns.scopes, vec![ConfigScope::Plugin, ConfigScope::Problem]);
        assert_eq!(ns.properties.len(), 1);

        let prop = &ns.properties["timeout"];
        assert_eq!(prop.schema_type, "number");
        assert_eq!(prop.title.as_deref(), Some("Timeout"));
        assert_eq!(prop.default, Some(json!(30.0)));
        assert_eq!(prop.min, Some(0.0));
        assert_eq!(prop.max, Some(300.0));
    }

    /// `[bundle]` is read only by release packaging (release/lib/plugins.sh).
    /// The host must keep accepting it, or every plugin that declares how it
    /// ships would fail to load.
    #[test]
    fn parse_manifest_with_bundle_section() {
        let toml_str = r#"
            name = "test-plugin"
            version = "1.0.0"

            [bundle]
            mode = "optional"
        "#;

        let manifest: PluginManifest = toml::from_str(toml_str).unwrap();
        assert_eq!(manifest.name, "test-plugin");
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
    fn declared_permissions_are_the_permissions_routes_require() {
        let manifest: PluginManifest = toml::from_str(
            r#"
            name = "print"
            version = "0.1.0"

            [server]
            entry = "print.wasm"

            [[server.routes]]
            method = "GET"
            path = "/queue"
            handler = "queue"
            permission = "print:operate"

            [[server.routes]]
            method = "GET"
            path = "/mine"
            handler = "mine"
        "#,
        )
        .unwrap();
        assert_eq!(
            manifest.declared_permissions().collect::<Vec<_>>(),
            ["print:operate"]
        );
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
