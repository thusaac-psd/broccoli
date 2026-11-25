//! Plugin registry listings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LanguageRegistryItem {
    #[schema(example = "cpp")]
    pub id: String,
    #[schema(example = "C++")]
    pub name: String,
    #[schema(example = "solution.cpp")]
    pub default_filename: String,
    #[schema(example = json!(["cpp", "cc", "cxx"]))]
    pub extensions: Vec<String>,
    #[schema(
        example = "#include <iostream>\nusing namespace std;\n\nint main() {\n    return 0;\n}\n"
    )]
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegistriesResponse {
    #[schema(example = json!(["batch", "interactive"]))]
    pub problem_types: Vec<String>,

    #[schema(example = json!(["exact", "lines", "tokens"]))]
    pub checker_formats: Vec<String>,

    /// The checker format a problem gets when it names none, for a form to
    /// preselect: the one a loaded checker plugin declares, or none when no
    /// single plugin declares one (a problem then names its format).
    #[schema(example = "lines")]
    pub default_checker_format: Option<String>,

    #[schema(example = json!(["my-contest-type"]))]
    pub contest_types: Vec<String>,

    pub languages: Vec<LanguageRegistryItem>,

    pub evaluators: Vec<EvaluatorEntry>,

    pub contest_type_handlers: Vec<ContestTypeEntry>,

    pub hooks: Vec<HookEntryInfo>,
}

/// A single evaluator registration entry.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EvaluatorEntry {
    /// Problem type this evaluator handles (e.g. "batch", "communication").
    #[schema(example = "communication")]
    pub problem_type: String,
    /// Plugin that registered this evaluator.
    #[schema(example = "communication-evaluator")]
    pub plugin_id: String,
    /// Plugin function invoked to evaluate a test case, or, for a staged
    /// evaluator, to prepare its operations.
    #[schema(example = "evaluate_communication")]
    pub function_name: String,
    /// For a staged evaluator, the plugin function that turns finished
    /// operations into verdicts.
    #[schema(example = "on_operation_results")]
    pub interpret_function_name: Option<String>,
}

/// A single contest type registration entry. Submission visibility is not
/// part of it: plugins govern visibility through their `visibility` queries.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestTypeEntry {
    /// Contest type identifier, as the plugin that registered it names it.
    #[schema(example = "my-contest-type")]
    pub contest_type: String,
    /// Plugin that registered this contest type.
    #[schema(example = "my-plugin")]
    pub plugin_id: String,
    /// Function name invoked on submission.
    #[schema(example = "on_submission")]
    pub submission_fn: String,
    /// Function name invoked for code-run / sample-run dispatch.
    #[schema(example = "on_code_run")]
    pub code_run_fn: String,
}

/// A single hook registration entry.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HookEntryInfo {
    /// Event topic the hook is bound to (e.g. "submission.created").
    #[schema(example = "submission.created")]
    pub topic: String,
    /// Plugin that registered the hook.
    #[schema(example = "submission-limit")]
    pub plugin_id: String,
    /// Hook scope ("global", "contest", "problem", "user", ...).
    #[schema(example = "global")]
    pub scope: String,
    /// Hook execution mode ("async", "sync", ...).
    #[schema(example = "async")]
    pub mode: String,
}

/// Where a plugin's files come from, as the admin API reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginSourceKind {
    /// The copy shipped in each node's plugins directory.
    Bundle,
    /// An archive uploaded through the admin API, served from the shared
    /// blob store. Takes precedence over a bundled copy.
    Upload,
}

/// What the fleet currently publishes for a plugin id.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PublishedPluginResponse {
    pub source: PluginSourceKind,
    /// Hash of the uploaded archive, when `source` is `upload`. An upload
    /// that replaces this plugin sends it (or `bundle`) as `replaces`.
    pub archive_hash: Option<String>,
    /// Published manifest version, once one has been published.
    #[schema(example = "1.1.0")]
    pub version: Option<String>,
    pub enabled: bool,
}

/// What a plugin archive would publish, for the admin to confirm.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PluginArchiveSummary {
    #[schema(example = "my-plugin")]
    pub id: String,
    #[schema(example = "My Plugin")]
    pub name: String,
    #[schema(example = "1.2.0")]
    pub version: String,
    pub description: Option<String>,
    /// Host permissions the plugin's server component requests.
    #[schema(example = json!(["storage", "sql"]))]
    pub permissions: Vec<String>,
    /// Content columns, as `table.column`, the plugin's SQL would read
    /// (`[server.sql] read`).
    #[schema(example = json!(["submission.files"]))]
    pub sql_read: Vec<String>,
    pub has_server: bool,
    pub has_web: bool,
    /// Content hash of the archive: its key in the blob store.
    pub archive_hash: String,
    pub size_bytes: u64,
    /// What the fleet runs under this id now; `None` for a new plugin.
    pub current: Option<PublishedPluginResponse>,
    /// Stored configs of this plugin that the archive's config schemas
    /// refuse. Publishing it leaves them in place, and the plugin reports an
    /// error wherever it reads one, so fix or delete them.
    pub incompatible_configs: Vec<IncompatibleConfig>,
    /// The enabled language pack that already provides the locale this
    /// archive's language pack declares. One enabled pack provides a locale,
    /// so publishing it is refused (`LANGUAGE_PACK_TAKEN`) until that pack
    /// is disabled.
    pub language_pack_taken: Option<LanguagePackHolder>,
}

/// An enabled language pack and the locale it provides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LanguagePackHolder {
    #[schema(example = "en")]
    pub locale: String,
    #[schema(example = "broccoli-en")]
    pub plugin_id: String,
}

impl PluginArchiveSummary {
    /// What an upload of this archive replaces, as the upload form's
    /// `replaces` field states it.
    pub fn replaces(&self) -> PluginReplaces {
        match &self.current {
            None => PluginReplaces::New,
            Some(current) => match (current.source, &current.archive_hash) {
                (PluginSourceKind::Upload, Some(hash)) => PluginReplaces::Upload(hash.clone()),
                _ => PluginReplaces::Bundle,
            },
        }
    }
}

/// The `replaces` field of a plugin upload: what the upload expects the fleet
/// to publish under the plugin's id when it lands. The server refuses the
/// upload with `409 CONFLICT` when that is no longer true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginReplaces {
    /// No plugin with this id exists yet.
    New,
    /// The bundled copy is published.
    Bundle,
    /// The uploaded archive with this hex content hash is published.
    Upload(String),
}

impl PluginReplaces {
    /// The field's value on the wire: `new`, `bundle` or the archive hash.
    pub fn form_value(&self) -> &str {
        match self {
            Self::New => "new",
            Self::Bundle => "bundle",
            Self::Upload(hash) => hash,
        }
    }

    /// Read the field's value. Any value other than `new` and `bundle` names
    /// an archive hash; the server checks that it is one.
    pub fn parse(value: &str) -> Self {
        match value.trim() {
            "new" => Self::New,
            "bundle" => Self::Bundle,
            hash => Self::Upload(hash.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PluginUploadResponse {
    #[schema(example = "my-plugin")]
    pub plugin_id: String,
    #[schema(example = "1.2.0")]
    pub version: String,
    pub archive_hash: String,
    pub message: String,
    /// Stored configs of this plugin that the published version's config
    /// schemas refuse, as inspecting the archive reports them.
    pub incompatible_configs: Vec<IncompatibleConfig>,
}

/// One way a config value breaks its namespace's schema: where, what (`kind`
/// and its parameters, for a client to say in its own language), and the same
/// in English (`message`, for logs).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FieldError {
    /// Where the value is, as `field.nested[2].leaf`; empty for the config
    /// object itself.
    pub path: String,
    #[schema(example = "must be at most 10")]
    pub message: String,
    #[serde(flatten)]
    pub kind: FieldErrorKind,
}

impl FieldError {
    /// The error `kind` at `path`, with its English message.
    pub fn new(path: impl Into<String>, kind: FieldErrorKind) -> Self {
        Self {
            path: path.into(),
            message: kind.to_string(),
            kind,
        }
    }
}

/// What is wrong with a config value, with the schema's numbers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FieldErrorKind {
    /// The value is of another JSON type than the schema declares.
    Type { expected: String, got: String },
    /// The schema declares no such field.
    UnknownField,
    /// A required field is absent.
    Missing,
    /// The value is none of the schema's allowed values.
    NotAllowed { allowed: Vec<serde_json::Value> },
    /// A number below the schema's minimum.
    Min { limit: f64 },
    /// A number above the schema's maximum.
    Max { limit: f64 },
    /// A string shorter than the schema's minimum length.
    MinLength { limit: u64 },
    /// A string longer than the schema's maximum length.
    MaxLength { limit: u64 },
    /// A string that does not match the schema's pattern.
    Pattern { pattern: String },
    /// The plugin no longer declares the config's namespace at its scope.
    NamespaceUndeclared { scope: String },
    /// A write keeps (sends `null` for) a secret that no stored item it
    /// names holds: a new item, or one whose key field changed. Its secret
    /// is entered again.
    SecretNotStored,
    /// A write clears (`{"$clear": true}`) a secret its item requires. The
    /// secret is replaced with a new one instead.
    SecretRequired,
    /// Another item of the array already has this value in the array's key
    /// field, which names one item.
    DuplicateKey { value: serde_json::Value },
    /// The plugin's own `validate` function refuses the value. `key`, when
    /// present, is a key of that plugin's language pack and `params` the
    /// values its message names; without it, `message` is all there is.
    Plugin {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        params: BTreeMap<String, serde_json::Value>,
    },
}

impl std::fmt::Display for FieldErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Type { expected, got } => write!(f, "expected {expected}, got {got}"),
            Self::UnknownField => f.write_str("unknown field"),
            Self::Missing => f.write_str("required field is missing"),
            Self::NotAllowed { allowed } => {
                let allowed: Vec<String> = allowed.iter().map(ToString::to_string).collect();
                write!(f, "must be one of {}", allowed.join(", "))
            }
            Self::Min { limit } => write!(f, "must be at least {limit}"),
            Self::Max { limit } => write!(f, "must be at most {limit}"),
            Self::MinLength { limit } => write!(f, "must be at least {limit} characters"),
            Self::MaxLength { limit } => write!(f, "must be at most {limit} characters"),
            Self::Pattern { pattern } => write!(f, "must match the pattern {pattern}"),
            Self::DuplicateKey { value } => {
                write!(f, "another item already has the key {value}")
            }
            Self::SecretRequired => {
                f.write_str("this secret is required: replace it instead of clearing it")
            }
            Self::SecretNotStored => {
                f.write_str("no secret is stored for this item: enter it again")
            }
            Self::NamespaceUndeclared { scope } => write!(
                f,
                "the plugin no longer declares this namespace at the {scope} scope"
            ),
            Self::Plugin { key, .. } => match key {
                Some(key) => write!(f, "refused by the plugin ({key})"),
                None => f.write_str("refused by the plugin"),
            },
        }
    }
}

/// What a config write sends in place of a secret field's value to clear
/// it: `{"$clear": true}`. Sending `null`, or leaving the field out, keeps
/// the stored secret; a string replaces it. Clients build the marker from
/// this schema, so its shape has one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClearSecret {
    #[serde(rename = "$clear")]
    pub clear: bool,
}

/// A config its plugin finds something wrong with: a stored config it would
/// no longer accept, or the defaults a namespace's `validate` function
/// checks where no config is stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IncompatibleConfig {
    #[schema(example = "my-plugin")]
    pub plugin_id: String,
    pub scope: broccoli_types::manifest::ConfigScope,
    /// What the config belongs to: the problem id, contest id or
    /// `contest_id:problem_id`; the plugin id at the plugin scope.
    #[schema(example = "12")]
    pub ref_id: String,
    /// The contest the config belongs to, at the contest and contest
    /// problem scopes.
    pub contest_id: Option<i32>,
    /// The problem the config belongs to, at the problem and contest
    /// problem scopes.
    pub problem_id: Option<i32>,
    /// The problem's label in its contest (`A`), at the contest problem
    /// scope.
    #[schema(example = "A")]
    pub label: Option<String>,
    #[schema(example = "contest")]
    pub namespace: String,
    /// What the plugin finds: what its schema or its namespace's validate
    /// function refuses in the config itself, then what the target's data
    /// does not meet yet. A namespace the plugin no longer declares at this
    /// scope has one error with an empty path.
    pub errors: Vec<FieldError>,
}

impl IncompatibleConfig {
    /// The issue of the config stored at `scope` and `ref_id`, with the ids
    /// `ref_id` names. The label is the server's to fill in.
    pub fn new(
        plugin_id: String,
        scope: broccoli_types::manifest::ConfigScope,
        ref_id: String,
        namespace: String,
        errors: Vec<FieldError>,
    ) -> Self {
        use broccoli_types::manifest::ConfigScope;
        let id = |part: Option<&str>| part.and_then(|part| part.parse().ok());
        let (contest_id, problem_id) = match scope {
            ConfigScope::Contest => (id(Some(&ref_id)), None),
            ConfigScope::Problem => (None, id(Some(&ref_id))),
            ConfigScope::ContestProblem => {
                let mut parts = ref_id.splitn(2, ':');
                (id(parts.next()), id(parts.next()))
            }
            ConfigScope::Plugin => (None, None),
        };
        Self {
            plugin_id,
            scope,
            ref_id,
            contest_id,
            problem_id,
            label: None,
            namespace,
            errors,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clear_marker_schema_is_the_marker_the_host_reads() {
        assert_eq!(
            serde_json::to_value(ClearSecret { clear: true }).unwrap(),
            serde_json::json!({ broccoli_types::manifest::CLEAR_SECRET: true })
        );
    }

    fn summary(current: Option<PublishedPluginResponse>) -> PluginArchiveSummary {
        PluginArchiveSummary {
            id: "demo".into(),
            name: "Demo".into(),
            version: "0.1.0".into(),
            description: None,
            permissions: vec![],
            sql_read: vec![],
            has_server: true,
            has_web: false,
            archive_hash: "ab".into(),
            size_bytes: 1,
            current,
            incompatible_configs: vec![],
            language_pack_taken: None,
        }
    }

    fn published(source: PluginSourceKind, archive_hash: Option<&str>) -> PublishedPluginResponse {
        PublishedPluginResponse {
            source,
            archive_hash: archive_hash.map(String::from),
            version: None,
            enabled: true,
        }
    }

    #[test]
    fn an_upload_replaces_what_the_inspection_reports_as_published() {
        assert_eq!(summary(None).replaces(), PluginReplaces::New);
        assert_eq!(
            summary(Some(published(PluginSourceKind::Bundle, None))).replaces(),
            PluginReplaces::Bundle
        );
        assert_eq!(
            summary(Some(published(PluginSourceKind::Upload, Some("cafe")))).replaces(),
            PluginReplaces::Upload("cafe".into())
        );
    }

    #[test]
    fn the_replaces_field_reads_back_what_it_writes() {
        for r in [
            PluginReplaces::New,
            PluginReplaces::Bundle,
            PluginReplaces::Upload("cafe".into()),
        ] {
            assert_eq!(PluginReplaces::parse(r.form_value()), r);
        }
    }
}
