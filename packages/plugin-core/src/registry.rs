use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use matchit::Router;
use serde::Serialize;

use crate::error::{AssetError, PluginError};
use crate::manifest::{PluginManifest, resolve_schema_includes};
use crate::pool::RecyclingPool;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum PluginStatus {
    Unloaded,
    Loaded,
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct RouteMatchInfo {
    pub methods: HashMap<String, RouteMetaData>,
}

#[derive(Debug, Clone)]
pub struct RouteMetaData {
    pub handler: String,
    pub permission: Option<String>,
}

pub struct PluginEntry {
    pub id: String,
    pub root_dir: PathBuf,
    pub manifest: PluginManifest,
    pub status: PluginStatus,
    pub runtime: Option<RecyclingPool>,
    pub router: Router<RouteMatchInfo>,
}

pub type PluginRegistry = Arc<RwLock<HashMap<String, PluginEntry>>>;

#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub id: String,
    #[serde(skip)]
    pub root_dir: PathBuf,
    #[serde(flatten)]
    pub manifest: PluginManifest,
    pub status: PluginStatus,
}

impl From<&PluginEntry> for PluginInfo {
    fn from(entry: &PluginEntry) -> Self {
        Self {
            id: entry.id.clone(),
            root_dir: entry.root_dir.clone(),
            manifest: entry.manifest.clone(),
            status: entry.status.clone(),
        }
    }
}

impl PluginEntry {
    pub fn new(
        id: String,
        root_dir: PathBuf,
        manifest: PluginManifest,
    ) -> Result<Self, PluginError> {
        let mut router = Router::new();

        if let Some(server_config) = &manifest.server {
            if server_config.timers.len() > 1 {
                return Err(PluginError::LoadFailed(format!(
                    "Plugin '{id}' declares more than one server timer callback"
                )));
            }
            let mut path_map: HashMap<String, RouteMatchInfo> = HashMap::new();
            for route in &server_config.routes {
                let info = path_map
                    .entry(route.path.clone())
                    .or_insert(RouteMatchInfo {
                        methods: HashMap::new(),
                    });
                info.methods.insert(
                    route.method.to_uppercase(),
                    RouteMetaData {
                        handler: route.handler.clone(),
                        permission: route.permission.clone(),
                    },
                );
            }
            for (path, info) in path_map {
                router.insert(&path, info).map_err(|e| {
                    PluginError::LoadFailed(format!(
                        "Invalid route '{}' in plugin '{}': {}",
                        path, id, e
                    ))
                })?;
            }
        }

        Ok(Self {
            id,
            root_dir,
            manifest,
            status: PluginStatus::Unloaded,
            runtime: None,
            router,
        })
    }

    pub fn from_dir(plugin_dir: &Path) -> Result<Self, PluginError> {
        if !plugin_dir.exists() || !plugin_dir.is_dir() {
            return Err(PluginError::LoadFailed(format!(
                "Plugin directory '{}' does not exist or is not a directory",
                plugin_dir.display()
            )));
        }

        let id = plugin_dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| PluginError::LoadFailed("Invalid plugin directory name".into()))?
            .to_string();

        let toml_path = plugin_dir.join("plugin.toml");
        let toml_content = std::fs::read_to_string(&toml_path)
            .map_err(|e| PluginError::LoadFailed(format!("Failed to read manifest: {}", e)))?;

        let mut manifest: PluginManifest = toml::from_str(&toml_content)
            .map_err(|e| PluginError::LoadFailed(format!("Invalid manifest syntax: {}", e)))?;

        resolve_schema_includes(&mut manifest, plugin_dir)?;

        Self::new(id, plugin_dir.to_path_buf(), manifest)
    }

    pub fn resolve_web_asset(&self, relative_path: &str) -> Result<PathBuf, AssetError> {
        let web_config = self.manifest.web.as_ref().ok_or(AssetError::NoWebConfig)?;

        let web_root = self.root_dir.join(&web_config.root);
        let asset_path = web_root.join(relative_path);

        // A declared web root that was never built holds no assets: that is a
        // missing asset, not a host fault.
        let canonical_web_root = web_root.canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => AssetError::NotFound,
            _ => AssetError::Io(e),
        })?;
        let canonical_asset_path = asset_path
            .canonicalize()
            .map_err(|_| AssetError::NotFound)?;

        if !canonical_asset_path.starts_with(&canonical_web_root) {
            return Err(AssetError::PathTraversal);
        }

        if !canonical_asset_path.exists() || !canonical_asset_path.is_file() {
            return Err(AssetError::NotFound);
        }

        Ok(canonical_asset_path)
    }
}

#[cfg(test)]
mod tests {
    use super::PluginEntry;
    use crate::manifest::PluginManifest;

    #[test]
    fn rejects_multiple_timer_callbacks() {
        let manifest: PluginManifest = toml::from_str(
            r#"
name = "test"
version = "0.1.0"

[server]
entry = "test.wasm"

[[server.timers]]
function = "first"

[[server.timers]]
function = "second"
"#,
        )
        .unwrap();

        let error = PluginEntry::new("test".into(), ".".into(), manifest)
            .err()
            .expect("multiple callbacks must be rejected");
        assert!(
            error
                .to_string()
                .contains("more than one server timer callback")
        );
    }

    fn web_plugin(root: &std::path::Path) -> PluginEntry {
        let manifest: PluginManifest = toml::from_str(
            r#"
name = "web"
version = "0.1.0"

[web]
root = "frontend/dist"
entry = "index.js"
"#,
        )
        .unwrap();
        PluginEntry::new("web".into(), root.to_path_buf(), manifest).unwrap()
    }

    #[test]
    fn an_unbuilt_web_root_is_a_missing_asset() {
        let dir = tempfile::tempdir().unwrap();
        let err = web_plugin(dir.path())
            .resolve_web_asset("index.js")
            .expect_err("nothing was built");
        assert!(matches!(err, crate::error::AssetError::NotFound), "{err:?}");
    }

    #[test]
    fn a_built_web_root_serves_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let dist = dir.path().join("frontend/dist");
        std::fs::create_dir_all(&dist).unwrap();
        std::fs::write(dist.join("index.js"), "export {}").unwrap();
        let path = web_plugin(dir.path())
            .resolve_web_asset("index.js")
            .unwrap();
        assert!(path.ends_with("frontend/dist/index.js"));
    }
}
