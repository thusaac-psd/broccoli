use crate::plugin_distribution::PluginSourceKind;
use crate::plugin_fleet::{FleetView, PluginFleetResponse, PluginSourceResponse};
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
    /// Where the fleet takes the plugin's files from.
    pub source: PluginSourceResponse,

    #[schema(example = "An Awesome Plugin")]
    pub name: String,
    #[schema(example = "1.0.0")]
    pub version: String,
    #[schema(example = "This plugin does awesome things!")]
    pub description: Option<String>,

    #[schema(example = true)]
    pub has_server: bool,
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
    pub scopes: Vec<plugin_core::manifest::ConfigScope>,
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
    /// Where the fleet takes the plugin's files from.
    pub source: PluginSourceResponse,

    #[schema(example = "An Awesome Plugin")]
    pub name: String,
    #[schema(example = "1.0.0")]
    pub version: String,
    #[schema(example = "This plugin does awesome things!")]
    pub description: Option<String>,

    #[schema(example = true)]
    pub has_server: bool,
    #[schema(example = true)]
    pub has_web: bool,

    pub server: Option<ServerDetailResponse>,
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
    /// Content columns, as `table.column`, the plugin's SQL may read
    /// (`[server.sql] read`).
    #[schema(example = json!(["submission.files"]))]
    pub sql_read: Vec<String>,
    pub routes: Vec<ServerRouteConfig>,
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

impl ActivePluginResponse {
    /// The frontend description of a plugin, or `None` when it has no web
    /// component. Asset URLs carry the file's modification time so browsers
    /// refetch them after an update.
    pub async fn for_web_plugin(info: PluginInfo) -> Option<Self> {
        let web = info.manifest.web.as_ref()?;
        let web_root = info.root_dir.join(&web.root);
        let entry = versioned_asset_url(&info.id, &web_root, &web.entry).await;
        let mut css = Vec::with_capacity(web.css.len());
        for css_file in &web.css {
            css.push(versioned_asset_url(&info.id, &web_root, css_file).await);
        }
        Some(Self {
            id: info.id.clone(),
            entry,
            components: web.components.clone(),
            slots: web.slots.clone(),
            routes: web.routes.clone(),
            css,
            name: info.manifest.name,
        })
    }
}

/// `/assets/<plugin>/<file>?t=<mtime seconds>`, with `t=0` when the file's
/// modification time cannot be read.
async fn versioned_asset_url(plugin_id: &str, web_root: &std::path::Path, file: &str) -> String {
    let mtime = tokio::fs::metadata(web_root.join(file))
        .await
        .and_then(|m| m.modified())
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        })
        .unwrap_or(0);
    format!("/assets/{plugin_id}/{file}?t={mtime}")
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
    pub fn without_manifest(id: &str, fleet: &FleetView, bundled: bool) -> Self {
        Self {
            id: id.to_string(),
            status: PluginStatusResponse::Unloaded,
            enabled: fleet.is_enabled(id),
            fleet: fleet.plugin(id),
            source: fleet.source(id, bundled),
            name: id.to_string(),
            version: String::new(),
            description: None,
            has_server: false,
            has_web: false,
            config_schemas: Vec::new(),
        }
    }

    /// `fleet` supplies the desired state and the per-replica load states;
    /// `bundled` is whether this replica's bundle has a copy.
    pub fn new(info: PluginInfo, fleet: &FleetView, bundled: bool) -> Self {
        let enabled = fleet.is_enabled(&info.id);
        let source = fleet.source(&info.id, bundled);
        let fleet = fleet.plugin(&info.id);
        let has_server = info.manifest.has_server();
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
            source,
            name: info.manifest.name,
            version: info.manifest.version,
            description: info.manifest.description,
            has_server,
            has_web,
            config_schemas,
        }
    }
}

impl PluginFullDetailResponse {
    /// See [`PluginDetailResponse::without_manifest`].
    pub fn without_manifest(id: &str, fleet: &FleetView, bundled: bool) -> Self {
        Self {
            id: id.to_string(),
            status: PluginStatusResponse::Unloaded,
            enabled: fleet.is_enabled(id),
            fleet: fleet.plugin(id),
            source: fleet.source(id, bundled),
            name: id.to_string(),
            version: String::new(),
            description: None,
            has_server: false,
            has_web: false,
            server: None,
            web: None,
            translations: Vec::new(),
            config_schemas: Vec::new(),
        }
    }

    /// `fleet` supplies the desired state and the per-replica load states;
    /// `bundled` is whether this replica's bundle has a copy.
    pub fn new(info: PluginInfo, fleet: &FleetView, bundled: bool) -> Self {
        let enabled = fleet.is_enabled(&info.id);
        let source = fleet.source(&info.id, bundled);
        let fleet = fleet.plugin(&info.id);
        let has_server = info.manifest.has_server();
        let has_web = info.manifest.has_web();

        let server = info.manifest.server.as_ref().map(|s| ServerDetailResponse {
            permissions: s.permissions.clone(),
            sql_read: sql_read(s),
            routes: s.routes.clone(),
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
            source,
            name: info.manifest.name,
            version: info.manifest.version,
            description: info.manifest.description,
            has_server,
            has_web,
            server,
            web,
            translations,
            config_schemas,
        }
    }
}

/// The multipart form the plugin upload endpoints read. Documentation only:
/// the handlers read the field themselves, after the permission and
/// upload-policy checks, so a refused upload is answered before its body is
/// read.
#[derive(utoipa::ToSchema)]
pub struct PluginArchiveForm {
    /// A tar.gz archive with a single top-level directory, named after the
    /// plugin id, containing `plugin.toml`.
    #[schema(value_type = String, format = Binary)]
    pub plugin: Vec<u8>,
    /// Upload only: what the archive replaces, as inspecting it reported:
    /// `new` (no such plugin yet), `bundle`, or the hash of the uploaded
    /// archive now published. A mismatch means another change landed since,
    /// and the upload is refused with `409 CONFLICT`.
    #[schema(example = "bundle")]
    pub replaces: Option<String>,
}

/// What a plugin archive would publish, for the admin to confirm.
#[derive(Serialize, utoipa::ToSchema)]
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
}

/// What the fleet currently publishes for a plugin id.
#[derive(Serialize, utoipa::ToSchema)]
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

#[derive(Serialize, utoipa::ToSchema)]
pub struct PluginUploadResponse {
    #[schema(example = "my-plugin")]
    pub plugin_id: String,
    #[schema(example = "1.2.0")]
    pub version: String,
    pub archive_hash: String,
    pub message: String,
}

/// Whether this server accepts plugin uploads.
#[derive(Serialize, utoipa::ToSchema)]
pub struct PluginUploadPolicy {
    /// `false` when the operator switched uploads off (`plugin.uploads_enabled`).
    pub enabled: bool,
    /// Largest archive accepted, in bytes.
    pub max_archive_bytes: u64,
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RemoveUploadQuery {
    /// The outcome the admin confirmed: `bundle_restored` or `removed`.
    pub expect: crate::plugin_lifecycle::UploadRemoval,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct RemoveUploadResponse {
    pub outcome: crate::plugin_lifecycle::UploadRemoval,
    pub message: String,
}

/// The `[server.sql] read` scopes of a manifest's server section.
pub fn sql_read(server: &plugin_core::manifest::ServerConfig) -> Vec<String> {
    server
        .sql
        .read
        .iter()
        .map(|scope| scope.as_str().to_string())
        .collect()
}
