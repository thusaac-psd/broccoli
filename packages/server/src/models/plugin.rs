use crate::plugin_fleet::{FleetView, PluginFleetResponse};
pub use broccoli_api_types::plugin::{
    ContestTypeEntry, EvaluatorEntry, HookEntryInfo, LanguageRegistryItem, RegistriesResponse,
};
use plugin_core::manifest::{ComponentMap, ServerRouteConfig, WebRouteConfig, WebSlotConfig};
use plugin_core::registry::{PluginInfo, PluginStatus};
use serde::Serialize;

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
    /// Load state on the replica that served this request only. Prefer
    /// `enabled` and `fleet`, which cover every server replica.
    #[schema(example = "Loaded")]
    pub status: PluginStatusResponse,
    /// Whether the plugin is enabled (the fleet-wide desired state).
    #[schema(example = true)]
    pub enabled: bool,
    /// The plugin's load state on every live server replica.
    pub fleet: PluginFleetResponse,

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
    /// Load state on the replica that served this request only. Prefer
    /// `enabled` and `fleet`, which cover every server replica.
    #[schema(example = "Loaded")]
    pub status: PluginStatusResponse,
    /// Whether the plugin is enabled (the fleet-wide desired state).
    #[schema(example = true)]
    pub enabled: bool,
    /// The plugin's load state on every live server replica.
    pub fleet: PluginFleetResponse,

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

impl PluginDetailResponse {
    /// A plugin the fleet knows about but whose files are not on the replica
    /// serving the request, so there is no manifest to describe it: only its
    /// id, desired state and per-replica states are known.
    pub fn without_manifest(id: &str, fleet: &FleetView) -> Self {
        Self {
            id: id.to_string(),
            status: PluginStatusResponse::Unloaded,
            enabled: fleet.is_enabled(id),
            fleet: fleet.plugin(id),
            name: id.to_string(),
            version: String::new(),
            description: None,
            has_server: false,
            has_worker: false,
            has_web: false,
            config_schemas: Vec::new(),
        }
    }

    /// `fleet` supplies the desired state and the per-replica load states.
    pub fn new(info: PluginInfo, fleet: &FleetView) -> Self {
        let enabled = fleet.is_enabled(&info.id);
        let fleet = fleet.plugin(&info.id);
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
            enabled,
            fleet,
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

impl PluginFullDetailResponse {
    /// See [`PluginDetailResponse::without_manifest`].
    pub fn without_manifest(id: &str, fleet: &FleetView) -> Self {
        Self {
            id: id.to_string(),
            status: PluginStatusResponse::Unloaded,
            enabled: fleet.is_enabled(id),
            fleet: fleet.plugin(id),
            name: id.to_string(),
            version: String::new(),
            description: None,
            has_server: false,
            has_worker: false,
            has_web: false,
            server: None,
            worker: None,
            web: None,
            translations: Vec::new(),
            config_schemas: Vec::new(),
        }
    }

    /// `fleet` supplies the desired state and the per-replica load states.
    pub fn new(info: PluginInfo, fleet: &FleetView) -> Self {
        let enabled = fleet.is_enabled(&info.id);
        let fleet = fleet.plugin(&info.id);
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
            enabled,
            fleet,
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
