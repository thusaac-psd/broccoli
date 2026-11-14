//! Plugin registry listings.

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
    /// preselect.
    #[schema(example = "lines")]
    pub default_checker_format: String,

    #[schema(example = json!(["icpc", "ioi"]))]
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
    /// Contest type identifier (e.g. "icpc", "ioi").
    #[schema(example = "icpc")]
    pub contest_type: String,
    /// Plugin that registered this contest type.
    #[schema(example = "icpc")]
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

/// One way a config value breaks its namespace's schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct FieldError {
    /// Where the value is, as `field.nested[2].leaf`; empty for the config
    /// object itself.
    pub path: String,
    pub message: String,
}

/// A stored config that a plugin manifest would no longer accept.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct IncompatibleConfig {
    pub scope: broccoli_types::manifest::ConfigScope,
    /// What the config belongs to: the problem id, contest id or
    /// `contest_id:problem_id`; the plugin id at the plugin scope.
    #[schema(example = "12")]
    pub ref_id: String,
    #[schema(example = "contest")]
    pub namespace: String,
    /// Why the manifest refuses it. A namespace the manifest no longer
    /// declares at this scope has one error with an empty path.
    pub errors: Vec<FieldError>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
