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

    /// Set on a language pack: the plugin that provides the host's own text
    /// in one locale.
    pub language_pack: Option<LanguagePack>,

    #[serde(default)]
    pub config: HashMap<String, ConfigNamespace>,
}

/// What a language pack declares: `[language_pack] locale = "<locale>"`. Its
/// translation file for that locale holds the host's text in the locale;
/// one enabled plugin at a time provides a locale's host text. A pack
/// declares translations only: no `[server]`, `[web]` or `[config]`.
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LanguagePack {
    pub locale: String,
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

    /// The locale whose host text this plugin provides, when it is a
    /// language pack ([`LanguagePack`]). Only its translation file for that
    /// locale may carry keys outside the plugin's own.
    pub fn host_locale(&self) -> Option<&str> {
        self.language_pack.as_ref().map(|pack| pack.locale.as_str())
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

    /// The checker format, one this plugin registers, that a problem gets
    /// when it names none. The host uses it while this plugin is the one
    /// loaded plugin that declares a default; otherwise a new problem must
    /// name its checker format.
    #[serde(default)]
    pub default_checker_format: Option<String>,
}

/// Host content columns, as `table.column`, that a plugin's SQL reads only
/// when its manifest declares them under `[server.sql] read`: contestant
/// source, test data, program output, message bodies and the diagnostics of
/// a failed judgement or run (`error_message`, which the host shows only to
/// staff with `system:view`). The other columns
/// of a plugin-readable host table are readable to every plugin with the
/// `sql` permission. Credentials are in no table a plugin can read and are
/// never grantable.
pub const SQL_READ_SCOPES: &[&str] = &[
    "clarification.content",
    "clarification_reply.content",
    "code_run.compile_output",
    "code_run.custom_test_cases",
    "code_run.error_message",
    "code_run.files",
    "code_run_result.checker_output",
    "code_run_result.stderr",
    "code_run_result.stdout",
    "problem.content",
    "submission.compile_output",
    "submission.error_message",
    "submission.files",
    "submission_judgement.compile_output",
    "submission_judgement.error_message",
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

#[derive(Debug, Deserialize, Serialize, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
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
    /// renders this slot entry on the pages of a contest of this type. When
    /// unset, the slot is rendered everywhere.
    #[serde(default)]
    pub contest_type: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct WebRouteConfig {
    pub path: String,

    pub component: String,

    /// Translation key of the page's name, shown in the browser tab.
    pub title: String,
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
    /// A short name for the namespace, which the settings form labels its
    /// tab by (the namespace's id when absent); `description` says more.
    pub title: Option<String>,
    pub description: Option<String>,
    /// Where this namespace can be configured. The host refuses a config for
    /// it anywhere else.
    #[serde(default)]
    pub scopes: Vec<ConfigScope>,
    pub schema: Option<String>,
    #[serde(default)]
    pub properties: HashMap<String, SchemaProperty>,
    /// Whether this namespace holds a problem's checker source files: a
    /// problem-scoped namespace with a `files` array of `{ filename, content }`
    /// objects. The host stores and serves it through its checker-source
    /// endpoints for every problem whose checker format this plugin provides,
    /// and nowhere else; a plugin declares at most one.
    #[serde(default)]
    pub checker_source: bool,
    /// Whether judging reads this namespace: a change to one of its stored
    /// configs changes what a problem is judged with, so it counts in the
    /// inputs version of every problem the config reaches (a contest-scope
    /// config each problem of the contest), and a plugin-scope one in the
    /// plugin's own inputs epoch, which every judgement it judges records. A
    /// `checker_source` namespace is one whether it says so or
    /// not ([`ConfigNamespace::is_judging_input`]).
    #[serde(default)]
    pub judging_input: bool,
    /// For a `checker_source` namespace: the checker formats whose checker
    /// compiles the stored source. Work on a problem of one of these formats
    /// needs the language its source compiles in ([`Self::checker_languages`]);
    /// a format not listed compiles nothing, whatever its source holds.
    #[serde(default)]
    pub compiles: Vec<String>,
    /// For a `checker_source` namespace: the languages its checker source
    /// compiles in, each with the file extensions (without the dot) of a
    /// primary file in that language. The primary file is the first stored
    /// file whose extension, compared case-insensitively, is listed here
    /// ([`checker_language`]); the host reads a problem's checker language
    /// from it, and the plugin compiles the source by the same rule.
    #[serde(default)]
    pub checker_languages: std::collections::BTreeMap<String, Vec<String>>,
    /// The plugin function that checks an admin's write to this namespace
    /// beyond its schema (see `types::ConfigValidateInput`). The host stores
    /// nothing it refuses, and lists stored configs it now refuses on the
    /// problem's config issues. Requires a `[server]` entry.
    #[serde(default)]
    pub validate: Option<String>,
}

/// A value that holds no secret: absent, `null` or the empty string.
fn unset(value: &serde_json::Value) -> bool {
    value.is_null() || value.as_str() == Some("")
}

/// What a write sends for a secret to clear it: `{"$clear": true}`.
pub const CLEAR_SECRET: &str = "$clear";

/// What a read shows for a secret that holds a value, and what a write sends
/// back to keep it: `{"$secret": "<ref>"}`, the opaque ref the host minted
/// when it stored that value ([`SecretRef`]).
pub const SECRET_REF: &str = "$secret";

/// Whether `value` is the marker that clears a secret ([`CLEAR_SECRET`]).
fn clears(value: &serde_json::Value) -> bool {
    value.as_object().is_some_and(|marker| {
        marker.len() == 1 && marker.get(CLEAR_SECRET) == Some(&serde_json::Value::Bool(true))
    })
}

/// The ref `value` echoes, when it is the marker a read shows a set secret
/// as ([`SECRET_REF`]).
fn echoed(value: &serde_json::Value) -> Option<&str> {
    let marker = value.as_object()?;
    (marker.len() == 1)
        .then(|| marker.get(SECRET_REF)?.as_str())
        .flatten()
}

/// The marker a read shows a secret holding ref `id` as ([`SECRET_REF`]).
fn ref_marker(id: &str) -> serde_json::Value {
    serde_json::json!({ SECRET_REF: id })
}

/// One step to a secret by identity: a field, or the item of an array whose
/// key field (the array's `key`) holds `value`. A secret belongs to the item
/// it was set on, never to a position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretStep {
    Field(String),
    Item {
        key: String,
        value: serde_json::Value,
    },
}

/// A secret field that holds a value: `path` names it in the config as it
/// is (`stations[0].token`), `identity` by its items' keys.
#[derive(Debug, Clone, PartialEq)]
pub struct SetSecret {
    pub path: String,
    pub identity: Vec<SecretStep>,
}

/// The opaque ref of one stored secret: minted when a write stores a new
/// value at `identity`, kept while the value stays. A read shows the ref in
/// place of the value, and a write keeps the stored value only where it
/// sends the ref back, so a secret is kept only for an item the writer
/// read. A bundle carries the refs and never the values: an import keeps
/// what the target stores only where the target's ref is the bundle's, the
/// same stored secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretRef {
    pub identity: Vec<SecretStep>,
    #[serde(rename = "ref")]
    pub id: String,
}

/// The ref `refs` holds for the secret at `identity`.
fn ref_at<'a>(refs: &'a [SecretRef], identity: &[SecretStep]) -> Option<&'a str> {
    refs.iter()
        .find(|stored| stored.identity == identity)
        .map(|stored| stored.id.as_str())
}

/// What a [`each_secret`] visitor gets for each secret field: its path, its
/// identity, the object that holds it, its key there, and whether that
/// object requires it.
type SecretVisit<'a> = dyn FnMut(&str, &[SecretStep], &mut serde_json::Map<String, serde_json::Value>, &str, bool)
    + 'a;

/// Calls `visit` with each secret field of `value` that `properties`
/// declare, inside nested objects and array items too, with its path
/// (`stations[0].token`) and its identity, each item named by its array's
/// key field.
fn each_secret(
    properties: &HashMap<String, SchemaProperty>,
    required: &[String],
    value: &mut serde_json::Value,
    path: &str,
    identity: &mut Vec<SecretStep>,
    visit: &mut SecretVisit<'_>,
) {
    let serde_json::Value::Object(fields) = value else {
        return;
    };
    let mut keys: Vec<&String> = properties.keys().collect();
    keys.sort();
    for key in keys {
        let property = &properties[key];
        let at = if path.is_empty() {
            key.clone()
        } else {
            format!("{path}.{key}")
        };
        identity.push(SecretStep::Field(key.clone()));
        if property.secret {
            visit(&at, identity, fields, key, required.contains(key));
        } else if let Some(child) = fields.get_mut(key) {
            each_secret(
                &property.properties,
                property.required.as_deref().unwrap_or_default(),
                child,
                &at,
                identity,
                visit,
            );
            if let (Some(items), serde_json::Value::Array(elements)) = (&property.items, child) {
                for (i, element) in elements.iter_mut().enumerate() {
                    let item = SecretStep::Item {
                        key: property.key.clone().unwrap_or_default(),
                        value: property
                            .key
                            .as_ref()
                            .and_then(|k| element.get(k))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null),
                    };
                    identity.push(item);
                    each_secret(
                        &items.properties,
                        items.required.as_deref().unwrap_or_default(),
                        element,
                        &format!("{at}[{i}]"),
                        identity,
                        visit,
                    );
                    identity.pop();
                }
            }
        }
        identity.pop();
    }
}

/// The value `identity` names in `value`: an item step takes the one item
/// whose key field holds the step's value, and no item when none or several
/// do.
fn at_identity<'a>(
    value: &'a serde_json::Value,
    identity: &[SecretStep],
) -> Option<&'a serde_json::Value> {
    identity.iter().try_fold(value, |value, step| match step {
        SecretStep::Field(name) => value.get(name),
        SecretStep::Item { key, value: wanted } => {
            if wanted.is_null() {
                return None;
            }
            let mut found = value
                .as_array()?
                .iter()
                .filter(|item| item.get(key) == Some(wanted));
            let item = found.next()?;
            found.next().is_none().then_some(item)
        }
    })
}

/// [`at_identity`], for writing.
fn at_identity_mut<'a>(
    value: &'a mut serde_json::Value,
    identity: &[SecretStep],
) -> Option<&'a mut serde_json::Value> {
    identity.iter().try_fold(value, |value, step| match step {
        SecretStep::Field(name) => value.get_mut(name),
        SecretStep::Item { key, value: wanted } => {
            if wanted.is_null() {
                return None;
            }
            let items = value.as_array_mut()?;
            let found: Vec<usize> = (0..items.len())
                .filter(|&i| items[i].get(key) == Some(wanted))
                .collect();
            match found[..] {
                [index] => items.get_mut(index),
                _ => None,
            }
        }
    })
}

/// A stored config and the refs of the secrets it holds.
#[derive(Debug, Clone, Copy, Default)]
pub struct StoredSecrets<'a> {
    pub config: Option<&'a serde_json::Value>,
    pub refs: &'a [SecretRef],
}

impl StoredSecrets<'_> {
    /// The value and ref stored for the secret at `identity`, when it holds
    /// a value.
    fn at(&self, identity: &[SecretStep]) -> Option<(&serde_json::Value, Option<&str>)> {
        let value = self
            .config
            .and_then(|stored| at_identity(stored, identity))
            .filter(|value| !unset(value))?;
        Some((value, ref_at(self.refs, identity)))
    }
}

/// Why a write's secret field is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretRefusal {
    /// It keeps a secret it never read: it sends no secret (`null`, the
    /// empty string, or nothing) where one is stored, or a ref the stored
    /// secret at the same item does not hold.
    NotStored(String),
    /// It sends no secret, or clears the one stored, where its object
    /// requires one.
    Required(String),
}

/// What a write's secret field comes to: its ref, none, or a refusal.
enum Kept {
    Ref(String),
    Unset,
    NotStored,
    Required,
}

/// The rule for one secret field of a write, at `key` of `fields`:
///
/// - a string sets the secret; it keeps the stored ref when it is the
///   stored value, else gets a new one from `mint`;
/// - the ref a read showed (`{"$secret": "<ref>"}`) keeps the stored value,
///   only at the item the read showed it on and while that item still holds
///   it;
/// - `{"$clear": true}` clears it;
/// - no secret (`null`, `""`, or the field left out) is refused where a
///   secret is stored, since the writer never read it, and leaves the field
///   unset elsewhere.
///
/// A secret the object requires (`required`) can be set or kept, never
/// cleared or left without one.
fn keep(
    fields: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    identity: &[SecretStep],
    stored: StoredSecrets<'_>,
    required: bool,
    mint: &mut dyn FnMut() -> String,
) -> Kept {
    let at = stored.at(identity);
    match fields.get(key) {
        Some(marker) if clears(marker) => {
            if required {
                return Kept::Required;
            }
            fields.remove(key);
            Kept::Unset
        }
        Some(marker) if echoed(marker).is_some() => {
            let wanted = echoed(marker).map(str::to_string);
            match at {
                Some((value, Some(id))) if wanted.as_deref() == Some(id) => {
                    let (value, id) = (value.clone(), id.to_string());
                    fields.insert(key.to_string(), value);
                    Kept::Ref(id)
                }
                _ => Kept::NotStored,
            }
        }
        Some(value) if !unset(value) => match at {
            Some((stored, Some(id))) if stored == value => Kept::Ref(id.to_string()),
            _ => Kept::Ref(mint()),
        },
        _ => {
            fields.remove(key);
            if required {
                Kept::Required
            } else if at.is_some() {
                Kept::NotStored
            } else {
                Kept::Unset
            }
        }
    }
}

/// Restores, in a config whose secrets were withheld where the schema is
/// not at hand (a bundle's, the identities
/// [`ConfigNamespace::withhold_secrets`] returned), each secret whose ref in
/// `refs` (the bundle's) is the one `stored` (the target's) holds at the
/// same item: the same stored secret. Every other one is left unset and its
/// ref dropped from `refs`. Returns how many were left unset.
pub fn keep_withheld(
    value: &mut serde_json::Value,
    refs: &mut Vec<SecretRef>,
    stored: StoredSecrets<'_>,
    identities: &[Vec<SecretStep>],
) -> usize {
    let mut unset_count = 0;
    for identity in identities {
        let Some((SecretStep::Field(key), parent)) = identity.split_last() else {
            continue;
        };
        let restored = match (stored.at(identity), ref_at(refs, identity)) {
            (Some((value, Some(target))), Some(bundled)) if target == bundled => {
                Some(value.clone())
            }
            _ => None,
        };
        let fields = match at_identity_mut(value, parent) {
            Some(serde_json::Value::Object(fields)) => Some(fields),
            _ => None,
        };
        match (restored, fields) {
            (Some(restored), Some(fields)) => {
                fields.insert(key.clone(), restored);
            }
            (_, fields) => {
                if let Some(fields) = fields {
                    fields.remove(key);
                }
                refs.retain(|bundled| &bundled.identity != identity);
                unset_count += 1;
            }
        }
    }
    unset_count
}

/// The language a checker source of `filenames` (in stored order) compiles
/// in, by `languages` (language to the extensions of a primary file, without
/// the dot): the language of the first file whose extension (the text after
/// its last `.`) equals one of them lists, ASCII letters compared
/// case-insensitively; when several languages list it, the first of them in
/// `languages`' order, bytewise by name. `None` when no file's does. The one
/// rule for a problem's checker language: the host derives it in SQL
/// (`providers::checker_language_sql`) and the checker plugin compiles by it,
/// both pinned to this function.
pub fn checker_language<'a>(
    languages: &'a std::collections::BTreeMap<String, Vec<String>>,
    filenames: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    filenames.into_iter().find_map(|filename| {
        let (_, extension) = filename.rsplit_once('.')?;
        languages
            .iter()
            .find(|(_, extensions)| {
                extensions.iter().any(|listed| {
                    listed
                        .trim_start_matches('.')
                        .eq_ignore_ascii_case(extension)
                })
            })
            .map(|(language, _)| language.as_str())
    })
}

impl ConfigNamespace {
    /// Whether judging reads this namespace (see [`Self::judging_input`]).
    pub fn is_judging_input(&self) -> bool {
        self.judging_input || self.checker_source
    }

    /// Replaces each secret field of `value` that holds a value with `null`,
    /// and returns those fields: a bundle's config, which carries no secret.
    pub fn withhold_secrets(&self, value: &mut serde_json::Value) -> Vec<SetSecret> {
        let mut set = Vec::new();
        each_secret(
            &self.properties,
            &[],
            value,
            "",
            &mut Vec::new(),
            &mut |path, identity, fields, key, _| {
                if let Some(secret) = fields.get_mut(key)
                    && !unset(secret)
                {
                    *secret = serde_json::Value::Null;
                    set.push(SetSecret {
                        path: path.to_string(),
                        identity: identity.to_vec(),
                    });
                }
            },
        );
        set
    }

    /// Replaces each secret field of `value` that holds a value with the
    /// marker of its ref in `refs` ([`SECRET_REF`]): what a read shows of a
    /// secret, that it is set, and what a write sends back to keep it. A
    /// secret with no ref shows an empty one, which keeps nothing.
    pub fn show_secrets(&self, value: &mut serde_json::Value, refs: &[SecretRef]) {
        each_secret(
            &self.properties,
            &[],
            value,
            "",
            &mut Vec::new(),
            &mut |_, identity, fields, key, _| {
                if let Some(secret) = fields.get_mut(key)
                    && !unset(secret)
                {
                    *secret = ref_marker(ref_at(refs, identity).unwrap_or_default());
                }
            },
        );
    }

    /// Applies the secret rule to each secret field of a value a client
    /// writes, against what is `stored`, and returns the refs of the
    /// secrets the value holds once written, or every field refused. An
    /// item is matched by its key field, never its position, so a client
    /// that read the config writes it back unchanged whatever items it
    /// moved; a new item, or one renamed, holds no secret a read showed it.
    ///
    /// A string sets a secret (keeping the stored ref when it is the stored
    /// value, else a new one from `mint`); the ref a read showed
    /// (`{"$secret": "<ref>"}`, [`SECRET_REF`]) keeps the stored value at
    /// that item while the item still holds it; `{"$clear": true}`
    /// ([`CLEAR_SECRET`]) clears it; no secret (`null`, `""`, or the field
    /// left out) is refused where one is stored and leaves the field unset
    /// elsewhere. A secret its object requires is never cleared or left
    /// without one.
    pub fn keep_secrets(
        &self,
        value: &mut serde_json::Value,
        stored: StoredSecrets<'_>,
        mint: &mut dyn FnMut() -> String,
    ) -> Result<Vec<SecretRef>, Vec<SecretRefusal>> {
        let mut refused = Vec::new();
        let mut refs = Vec::new();
        each_secret(
            &self.properties,
            &[],
            value,
            "",
            &mut Vec::new(),
            &mut |path, identity, fields, key, required| match keep(
                fields, key, identity, stored, required, mint,
            ) {
                Kept::Ref(id) => refs.push(SecretRef {
                    identity: identity.to_vec(),
                    id,
                }),
                Kept::Unset => {}
                Kept::NotStored => refused.push(SecretRefusal::NotStored(path.to_string())),
                Kept::Required => refused.push(SecretRefusal::Required(path.to_string())),
            },
        );
        if refused.is_empty() {
            Ok(refs)
        } else {
            Err(refused)
        }
    }

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
        if let Some(ref title) = self.title {
            schema.insert("title".into(), title.clone().into());
        }
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
    /// What each `enum` value reads as, in its order: the option's title in
    /// the settings form, which the plugin's pack translates. The JSON
    /// schema carries it as `x-enum-titles`.
    #[serde(default)]
    pub enum_titles: Option<Vec<String>>,
    pub items: Option<Box<SchemaProperty>>,
    #[serde(default)]
    pub properties: HashMap<String, SchemaProperty>,
    pub required: Option<Vec<String>>,
    pub additional_properties: Option<bool>,
    pub step: Option<f64>,
    pub precision: Option<u32>,
    pub unit: Option<String>,
    pub span: Option<u8>,
    /// The field holds a secret, such as a credential: the host never sends
    /// its value back on any read, only whether it is set, and keeps the
    /// stored value when a write sends `null` for it (see
    /// [`ConfigNamespace::withhold_secrets`]). The JSON schema marks it
    /// `writeOnly`.
    #[serde(default)]
    pub secret: bool,
    /// For an array of objects: the field of its items that names each one
    /// (a station's `name`). An array whose items hold a secret declares it,
    /// so a secret belongs to the item it was set on whatever its position.
    /// The JSON schema carries it as `x-key`.
    pub key: Option<String>,
}

impl SchemaProperty {
    /// Whether this property, or anything inside it, is a secret field.
    pub fn holds_secret(&self) -> bool {
        self.secret
            || self.properties.values().any(SchemaProperty::holds_secret)
            || self
                .items
                .as_ref()
                .is_some_and(|items| items.holds_secret())
    }

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
        if self.secret {
            schema.insert("writeOnly".into(), true.into());
        }
        if let Some(key) = &self.key {
            schema.insert("x-key".into(), key.clone().into());
        }
        if let Some(titles) = &self.enum_titles {
            schema.insert("x-enum-titles".into(), titles.clone().into());
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

    /// A namespace like print's: stations, each with a secret token.
    fn stations() -> ConfigNamespace {
        toml::from_str(
            r#"
            scopes = ["plugin"]
            [properties.banner]
            type = "string"
            [properties.stations]
            type = "array"
            key = "name"
            [properties.stations.items]
            type = "object"
            [properties.stations.items.properties.name]
            type = "string"
            [properties.stations.items.properties.token]
            type = "string"
            secret = true
            "#,
        )
        .expect("namespace")
    }

    #[test]
    fn a_bundle_withholds_each_set_secret_and_names_it() {
        let mut value = json!({
            "banner": "b",
            "stations": [{"name": "a", "token": "s1"}, {"name": "b", "token": ""}],
        });
        let set = stations().withhold_secrets(&mut value);
        assert_eq!(
            set,
            [SetSecret {
                path: "stations[0].token".into(),
                identity: identity("a"),
            }]
        );
        assert_eq!(
            value,
            json!({
                "banner": "b",
                "stations": [{"name": "a", "token": null}, {"name": "b", "token": ""}],
            })
        );
    }

    /// The identity of station `name`'s token.
    fn identity(name: &str) -> Vec<SecretStep> {
        vec![
            SecretStep::Field("stations".into()),
            SecretStep::Item {
                key: "name".into(),
                value: json!(name),
            },
            SecretStep::Field("token".into()),
        ]
    }

    fn secret_ref(name: &str, id: &str) -> SecretRef {
        SecretRef {
            identity: identity(name),
            id: id.into(),
        }
    }

    /// Two stations stored, alpha with token A (ref ra) and beta with token
    /// B (ref rb).
    fn stored_pair() -> (serde_json::Value, Vec<SecretRef>) {
        (
            json!({"stations": [{"name": "alpha", "token": "A"}, {"name": "beta", "token": "B"}]}),
            vec![secret_ref("alpha", "ra"), secret_ref("beta", "rb")],
        )
    }

    fn write_to(
        ns: &ConfigNamespace,
        write: serde_json::Value,
        stored: Option<&(serde_json::Value, Vec<SecretRef>)>,
    ) -> Result<(serde_json::Value, Vec<SecretRef>), Vec<SecretRefusal>> {
        let mut write = write;
        let stored = stored.map_or_else(StoredSecrets::default, |(config, refs)| StoredSecrets {
            config: Some(config),
            refs,
        });
        let mut minted = 0;
        let mut mint = || {
            minted += 1;
            format!("new{minted}")
        };
        ns.keep_secrets(&mut write, stored, &mut mint)
            .map(|refs| (write, refs))
    }

    fn kept(
        write: serde_json::Value,
    ) -> Result<(serde_json::Value, Vec<SecretRef>), Vec<SecretRefusal>> {
        write_to(&stations(), write, Some(&stored_pair()))
    }

    #[test]
    fn a_read_shows_each_set_secret_as_its_ref() {
        let (mut value, refs) = stored_pair();
        value["stations"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "gamma", "token": "G"}));
        stations().show_secrets(&mut value, &refs);
        assert_eq!(
            value,
            json!({"stations": [
                {"name": "alpha", "token": {"$secret": "ra"}},
                {"name": "beta", "token": {"$secret": "rb"}},
                {"name": "gamma", "token": {"$secret": ""}},
            ]}),
            "a secret with no ref shows one that keeps nothing"
        );
    }

    #[test]
    fn a_write_that_echoes_a_ref_keeps_that_items_secret_wherever_it_moved() {
        assert_eq!(
            kept(json!({"stations": [
                {"name": "new", "token": "N"},
                {"name": "beta", "token": {"$secret": "rb"}},
                {"name": "alpha", "token": {"$secret": "ra"}},
            ]})),
            Ok((
                json!({"stations": [
                    {"name": "new", "token": "N"},
                    {"name": "beta", "token": "B"},
                    {"name": "alpha", "token": "A"},
                ]}),
                vec![
                    secret_ref("new", "new1"),
                    secret_ref("beta", "rb"),
                    secret_ref("alpha", "ra"),
                ]
            ))
        );
    }

    #[test]
    fn a_deleted_item_takes_its_secret_with_it() {
        assert_eq!(
            kept(json!({"stations": [{"name": "beta", "token": {"$secret": "rb"}}]})),
            Ok((
                json!({"stations": [{"name": "beta", "token": "B"}]}),
                vec![secret_ref("beta", "rb")]
            ))
        );
    }

    #[test]
    fn a_write_that_never_read_a_stored_secret_keeps_none() {
        for (what, token) in [
            ("null", Some(json!(null))),
            ("the empty string", Some(json!(""))),
            ("left out", None),
            ("another item's ref", Some(json!({"$secret": "rb"}))),
            ("an unknown ref", Some(json!({"$secret": "forged"}))),
        ] {
            let mut alpha = json!({"name": "alpha"});
            if let Some(token) = token {
                alpha["token"] = token;
            }
            assert_eq!(
                kept(json!({"stations": [alpha]})),
                Err(vec![SecretRefusal::NotStored("stations[0].token".into())]),
                "{what}: a station deleted and added again by name has no secret to keep"
            );
        }
    }

    #[test]
    fn a_ref_keeps_nothing_on_an_item_renamed_since_the_read() {
        assert_eq!(
            kept(json!({"stations": [{"name": "renamed", "token": {"$secret": "ra"}}]})),
            Err(vec![SecretRefusal::NotStored("stations[0].token".into())])
        );
    }

    #[test]
    fn no_secret_where_none_is_stored_leaves_the_field_unset() {
        assert_eq!(
            kept(json!({"stations": [{"name": "new", "token": null}, {"name": "other"}]})),
            Ok((
                json!({"stations": [{"name": "new"}, {"name": "other"}]}),
                vec![]
            ))
        );
    }

    #[test]
    fn the_stored_value_written_again_keeps_its_ref_and_another_gets_a_new_one() {
        assert_eq!(
            kept(json!({"stations": [
                {"name": "alpha", "token": "A"},
                {"name": "beta", "token": "B2"},
            ]})),
            Ok((
                json!({"stations": [
                    {"name": "alpha", "token": "A"},
                    {"name": "beta", "token": "B2"},
                ]}),
                vec![secret_ref("alpha", "ra"), secret_ref("beta", "new1")]
            ))
        );
    }

    #[test]
    fn the_clear_marker_clears_a_secret() {
        assert_eq!(
            kept(json!({"stations": [
                {"name": "alpha", "token": {"$clear": true}},
                {"name": "beta", "token": {"$secret": "rb"}},
            ]})),
            Ok((
                json!({"stations": [{"name": "alpha"}, {"name": "beta", "token": "B"}]}),
                vec![secret_ref("beta", "rb")]
            ))
        );
        let (extra, _) = kept(json!({"stations": [
            {"name": "alpha", "token": {"$clear": true, "x": 1}},
        ]}))
        .expect("only the exact marker clears; the schema check refuses the rest");
        assert_eq!(
            extra["stations"][0]["token"],
            json!({"$clear": true, "x": 1})
        );
    }

    #[test]
    fn a_secret_its_item_requires_is_never_cleared_or_left_out() {
        let mut ns = stations();
        ns.properties
            .get_mut("stations")
            .and_then(|stations| stations.items.as_mut())
            .expect("items")
            .required = Some(vec!["name".into(), "token".into()]);
        for (what, token) in [
            ("cleared", Some(json!({"$clear": true}))),
            ("null", Some(json!(null))),
            ("left out", None),
        ] {
            for stored in [Some(&stored_pair()), None] {
                let mut alpha = json!({"name": "alpha"});
                if let Some(token) = token.clone() {
                    alpha["token"] = token;
                }
                assert_eq!(
                    write_to(&ns, json!({"stations": [alpha]}), stored),
                    Err(vec![SecretRefusal::Required("stations[0].token".into())]),
                    "{what}, stored: {}",
                    stored.is_some()
                );
            }
        }
    }

    #[test]
    fn a_bundle_keeps_the_targets_secret_only_where_it_holds_the_bundles_ref() {
        let mut exported = json!({"stations": [
            {"name": "alpha", "token": "source-A"},
            {"name": "beta", "token": "source-B"},
            {"name": "gamma", "token": "source-G"},
        ]});
        let identities: Vec<_> = stations()
            .withhold_secrets(&mut exported)
            .into_iter()
            .map(|set| set.identity)
            .collect();
        let mut bundled = vec![
            secret_ref("alpha", "ra"),
            secret_ref("beta", "rotated"),
            secret_ref("gamma", "rg"),
        ];
        let (target, target_refs) = stored_pair();
        let unset = keep_withheld(
            &mut exported,
            &mut bundled,
            StoredSecrets {
                config: Some(&target),
                refs: &target_refs,
            },
            &identities,
        );
        assert_eq!(
            exported,
            json!({"stations": [
                {"name": "alpha", "token": "A"},
                {"name": "beta"},
                {"name": "gamma"},
            ]}),
            "alpha's ref is the target's; beta's secret was set again since; \
             the target holds no gamma"
        );
        assert_eq!(bundled, vec![secret_ref("alpha", "ra")]);
        assert_eq!(unset, 2);
    }

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
            enum_titles: None,
            items: None,
            properties: HashMap::new(),
            required: None,
            additional_properties: None,
            step: None,
            precision: None,
            unit: None,
            span: None,
            secret: false,
            key: None,
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
            enum_titles: None,
            items: None,
            properties: HashMap::new(),
            required: None,
            additional_properties: None,
            step: None,
            precision: None,
            unit: None,
            span: None,
            secret: false,
            key: None,
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
