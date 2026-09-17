use plugin_core::manifest::{ComponentMap, ServerRouteConfig, WebRouteConfig, WebSlotConfig};
use plugin_core::registry::{PluginInfo, PluginStatus};
use serde::Serialize;

#[derive(Serialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct RegistriesResponse {
    #[schema(example = json!(["batch", "interactive"]))]
    pub problem_types: Vec<String>,

    #[schema(example = json!(["exact", "tokens"]))]
    pub checker_formats: Vec<String>,

    #[schema(example = json!(["icpc", "ioi"]))]
    pub contest_types: Vec<String>,

    pub languages: Vec<LanguageRegistryItem>,

    pub evaluators: Vec<EvaluatorEntry>,

    pub contest_type_handlers: Vec<ContestTypeEntry>,

    pub hooks: Vec<HookEntryInfo>,
}

/// A single evaluator registration entry.
#[derive(Serialize, utoipa::ToSchema)]
pub struct EvaluatorEntry {
    /// Problem type this evaluator handles (e.g. "batch", "communication").
    #[schema(example = "communication")]
    pub problem_type: String,
    /// Plugin that registered this evaluator.
    #[schema(example = "communication-evaluator")]
    pub plugin_id: String,
    /// Plugin function invoked to evaluate test cases for this problem type.
    #[schema(example = "evaluate_communication")]
    pub function_name: String,
}

/// A single contest type registration entry.
///
/// Used to carry a fourth field, `filter_submission_fn`: the function a
/// contest-type plugin registered to filter/redact a submission response
/// body before it left the server. Retired entirely (not replaced by
/// anything in this struct) once the VisibilityKernel took over submission
/// visibility end to end - `Decision`/`FieldMask` plus each plugin's
/// `topic = "visibility"` query function now own that job, driven by
/// `packages/server/src/visibility/`, not by a per-contest-type hook listed
/// here. See `git show dc8112d8` ("retire filter_submission_fn plugin hook")
/// for the removal and SDD 2026-09-15-visibility-kernel Tasks 13-16 for what
/// replaced it.
#[derive(Serialize, utoipa::ToSchema)]
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
#[derive(Serialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct ActivePluginResponse {
    #[schema(example = "plugin-123")]
    pub id: String,

    #[schema(example = "An Awesome Plugin")]
    pub name: String,

    #[schema(example = "/assets/plugin-123/index.js")]
    pub entry: String,

    pub components: ComponentMap,

    #[schema(example = json!([
        {
            "name": "sidebar.footer",
            "position": "append",
            "component": "MyComponent",
            "priority": 10
        }
    ]))]
    pub slots: Vec<WebSlotConfig>,

    #[schema(example = json!([
        {
            "path": "/problems/{id}/export",
            "component": "MyPage"
        }
    ]))]
    pub routes: Vec<WebRouteConfig>,

    #[schema(example = json!(["style.css"]))]
    pub css: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PluginDetailResponse {
    #[schema(example = "plugin-123")]
    pub id: String,
    #[schema(example = "Loaded")]
    pub status: PluginStatusResponse,

    #[schema(example = "An Awesome Plugin")]
    pub name: String,
    #[schema(example = "1.0.0")]
    pub version: String,
    #[schema(example = "This plugin does awesome things!")]
    pub description: Option<String>,

    #[schema(example = true)]
    pub has_server: bool,
    #[schema(example = false)]
    pub has_worker: bool,
    #[schema(example = true)]
    pub has_web: bool,

    pub config_schemas: Vec<ConfigSchemaResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ConfigSchemaResponse {
    #[schema(example = "testlib")]
    pub namespace: String,
    #[schema(example = "Testlib checker compiler settings")]
    pub description: Option<String>,
    pub scopes: Vec<String>,
    pub json_schema: serde_json::Value,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct PluginFullDetailResponse {
    #[schema(example = "plugin-123")]
    pub id: String,
    #[schema(example = "Loaded")]
    pub status: PluginStatusResponse,

    #[schema(example = "An Awesome Plugin")]
    pub name: String,
    #[schema(example = "1.0.0")]
    pub version: String,
    #[schema(example = "This plugin does awesome things!")]
    pub description: Option<String>,

    #[schema(example = true)]
    pub has_server: bool,
    #[schema(example = false)]
    pub has_worker: bool,
    #[schema(example = true)]
    pub has_web: bool,

    pub server: Option<ServerDetailResponse>,
    pub worker: Option<WorkerDetailResponse>,
    pub web: Option<WebDetailResponse>,
    pub translations: Vec<String>,

    /// Config schemas declared by this plugin (from `[server.config_schemas.<key>]`
    /// sections in plugin.toml). Used by the admin UI to render config forms and
    /// by the server for validation.
    pub config_schemas: Vec<ConfigSchemaResponse>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ServerDetailResponse {
    pub permissions: Vec<String>,
    pub routes: Vec<ServerRouteConfig>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WorkerDetailResponse {
    pub permissions: Vec<String>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct WebDetailResponse {
    pub components: ComponentMap,
    pub slots: Vec<WebSlotConfig>,
    pub routes: Vec<WebRouteConfig>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub enum PluginStatusResponse {
    Unloaded,
    Loaded,
    Failed,
}

impl From<PluginInfo> for ActivePluginResponse {
    fn from(info: PluginInfo) -> Self {
        let web_manifest = info.manifest.web.as_ref().expect(
            "PluginInfo should only be converted to ActivePluginResponse if it has a web component",
        );

        let entry = {
            let base = format!("/assets/{}/{}", info.id, web_manifest.entry);
            let entry_path = info
                .root_dir
                .join(&web_manifest.root)
                .join(&web_manifest.entry);
            let mtime = std::fs::metadata(&entry_path)
                .and_then(|m| m.modified())
                .map(|t| {
                    t.duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                })
                .unwrap_or(0);
            format!("{}?t={}", base, mtime)
        };

        let css: Vec<String> = web_manifest
            .css
            .iter()
            .map(|css_file| {
                let base = format!("/assets/{}/{}", info.id, css_file);
                let css_path = info.root_dir.join(&web_manifest.root).join(css_file);
                let mtime = std::fs::metadata(&css_path)
                    .and_then(|m| m.modified())
                    .map(|t| {
                        t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                    })
                    .unwrap_or(0);
                format!("{}?t={}", base, mtime)
            })
            .collect();

        Self {
            id: info.id.clone(),
            name: info.manifest.name,
            entry,
            components: web_manifest.components.clone(),
            slots: web_manifest.slots.clone(),
            routes: web_manifest.routes.clone(),
            css,
        }
    }
}

impl From<PluginStatus> for PluginStatusResponse {
    fn from(status: PluginStatus) -> Self {
        match status {
            PluginStatus::Unloaded => Self::Unloaded,
            PluginStatus::Loaded => Self::Loaded,
            PluginStatus::Failed(_) => Self::Failed,
        }
    }
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ReloadAllResponse {
    pub reloaded: Vec<String>,
    pub new: Vec<String>,
    pub failed: Vec<ReloadFailure>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ReloadFailure {
    pub id: String,
    pub error: String,
}

impl From<PluginInfo> for PluginDetailResponse {
    fn from(info: PluginInfo) -> Self {
        let has_server = info.manifest.has_server();
        let has_worker = info.manifest.has_worker();
        let has_web = info.manifest.has_web();

        let mut config_schemas: Vec<_> = info
            .manifest
            .config
            .iter()
            .map(|(ns, entry)| ConfigSchemaResponse {
                namespace: ns.clone(),
                description: entry.description.clone(),
                scopes: entry.scopes.clone(),
                json_schema: entry.to_json_schema(),
            })
            .collect();
        config_schemas.sort_by(|a, b| a.namespace.cmp(&b.namespace));

        Self {
            id: info.id,
            status: info.status.into(),
            name: info.manifest.name,
            version: info.manifest.version,
            description: info.manifest.description,
            has_server,
            has_worker,
            has_web,
            config_schemas,
        }
    }
}

impl From<PluginInfo> for PluginFullDetailResponse {
    fn from(info: PluginInfo) -> Self {
        let has_server = info.manifest.has_server();
        let has_worker = info.manifest.has_worker();
        let has_web = info.manifest.has_web();

        let server = info.manifest.server.as_ref().map(|s| ServerDetailResponse {
            permissions: s.permissions.clone(),
            routes: s.routes.clone(),
        });

        let worker = info.manifest.worker.as_ref().map(|w| WorkerDetailResponse {
            permissions: w.permissions.clone(),
        });

        let web = info.manifest.web.as_ref().map(|w| WebDetailResponse {
            components: w.components.clone(),
            slots: w.slots.clone(),
            routes: w.routes.clone(),
        });

        let translations: Vec<String> = info.manifest.translations.keys().cloned().collect();

        let mut config_schemas: Vec<_> = info
            .manifest
            .config
            .iter()
            .map(|(ns, entry)| ConfigSchemaResponse {
                namespace: ns.clone(),
                description: entry.description.clone(),
                scopes: entry.scopes.clone(),
                json_schema: entry.to_json_schema(),
            })
            .collect();
        config_schemas.sort_by(|a, b| a.namespace.cmp(&b.namespace));

        Self {
            id: info.id,
            status: info.status.into(),
            name: info.manifest.name,
            version: info.manifest.version,
            description: info.manifest.description,
            has_server,
            has_worker,
            has_web,
            server,
            worker,
            web,
            translations,
            config_schemas,
        }
    }
}
