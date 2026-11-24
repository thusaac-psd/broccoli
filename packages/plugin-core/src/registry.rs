use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use matchit::Router;
use serde::Serialize;

use crate::error::{AssetError, PluginError};
use crate::i18n::TranslationMap;
use crate::manifest::{
    PluginManifest, RouteAccess, dir_reader, parse_manifest, resolve_schema_includes_with,
};
use crate::pool::{InstancePool, Liveness};

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
    /// Who may call the route and what it addresses; the host enforces it
    /// before the handler runs.
    pub access: RouteAccess,
}

pub struct PluginEntry {
    pub id: String,
    pub root_dir: PathBuf,
    pub manifest: PluginManifest,
    pub status: PluginStatus,
    pub runtime: Option<InstancePool>,
    pub router: Router<RouteMatchInfo>,
    /// The manifest's translation files, read when the entry is built: what
    /// the host serves for this plugin never depends on its files staying on
    /// disk.
    pub translations: Vec<(String, TranslationMap)>,
    /// The pools of the versions this load replaced ([`Self::succeed`]):
    /// calls that started on them still run there, and the plugin's unload
    /// retires them with its own pool.
    superseded: Vec<Liveness>,
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
        crate::config_schema::check_manifest(&manifest)
            .map_err(|e| PluginError::LoadFailed(format!("Plugin '{id}': {e}")))?;
        if let Some(locale) = manifest.host_locale()
            && !manifest.translations.contains_key(locale)
        {
            return Err(PluginError::LoadFailed(format!(
                "Plugin '{id}' is a language pack for '{locale}' but declares no \
                 translation file for it"
            )));
        }
        // A pack's catalog for its locale is the host's text, unconfined to
        // the pack's id; a pack that also ran code or took settings would do
        // so under that text.
        if manifest.host_locale().is_some()
            && (manifest.has_server() || manifest.has_web() || !manifest.config.is_empty())
        {
            return Err(PluginError::LoadFailed(format!(
                "Plugin '{id}' is a language pack, which declares translations only: \
                 it may not declare [server], [web] or [config]"
            )));
        }
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
                let access = route.access().map_err(|e| {
                    PluginError::LoadFailed(format!(
                        "Invalid route {} {} in plugin '{}': {}",
                        route.method, route.path, id, e
                    ))
                })?;
                // A second declaration of a route would silently replace the
                // first one's handler and access, so it fails the load.
                let method = route.method.to_uppercase();
                if info.methods.contains_key(&method) {
                    return Err(PluginError::LoadFailed(format!(
                        "Plugin '{id}' declares route {method} {} more than once",
                        route.path
                    )));
                }
                info.methods.insert(
                    method,
                    RouteMetaData {
                        handler: route.handler.clone(),
                        access,
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
            translations: Vec::new(),
            superseded: Vec::new(),
        })
    }

    /// Take over from `previous`, the version of this plugin a reload
    /// replaced: its pool, and the pools it took over in turn, stay this
    /// load's, so calls still running on them are retired when the plugin
    /// is unloaded ([`Self::retire`]). Pools no call runs on any more are
    /// dropped: each is checked where `previous` holds it, before it is
    /// cloned, since the clone would itself count as a holder.
    pub fn succeed(&mut self, previous: &PluginEntry) {
        self.superseded = previous
            .superseded
            .iter()
            .filter(|pool| pool.in_use())
            .cloned()
            .chain(
                previous
                    .runtime
                    .as_ref()
                    .filter(|pool| pool.has_calls())
                    .map(InstancePool::liveness),
            )
            .collect();
    }

    /// How many replaced pools this load keeps.
    #[cfg(test)]
    pub(crate) fn superseded_count(&self) -> usize {
        self.superseded.len()
    }

    /// Unload: retire this version's pool and every pool it took over, so
    /// no call of the plugin, whichever version it started on, runs another
    /// host function.
    pub fn retire(&mut self) {
        if let Some(pool) = self.runtime.take() {
            pool.retire();
        }
        for pool in self.superseded.drain(..) {
            pool.retire();
        }
    }

    /// Drop this version alone, a reload that did not take: its pool
    /// retires, and the pools it took over stay with the version put back.
    pub fn discard(mut self) {
        if let Some(pool) = self.runtime.take() {
            pool.retire();
        }
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

        Self::read(id, plugin_dir.to_path_buf(), dir_reader(plugin_dir)?)
    }

    /// Build plugin `id`'s entry from its files, each read through `read`
    /// (given its path relative to the plugin directory, as the manifest
    /// names it): `plugin.toml`, the config schemas it includes and its
    /// translation files. Loading a plugin directory and checking an
    /// uploaded archive both build the entry here, so an archive uploads
    /// exactly when it would load. An id the one plugin id rule refuses
    /// ([`broccoli_api_types::plugin_id::check_plugin_id`]) is refused here:
    /// one naming a namespace of the host's messages would own the host's
    /// keys.
    pub fn read(
        id: String,
        root_dir: PathBuf,
        mut read: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<Self, PluginError> {
        broccoli_api_types::plugin_id::check_plugin_id(&id)
            .map_err(|e| PluginError::LoadFailed(e.to_string()))?;
        let toml_content = read("plugin.toml")
            .map_err(|e| PluginError::LoadFailed(format!("Failed to read manifest: {e}")))?;
        let mut manifest = parse_manifest(&toml_content)?;
        resolve_schema_includes_with(&mut manifest, &mut read)?;
        let mut entry = Self::new(id, root_dir, manifest)?;
        entry.translations = read_translations(&entry.id, &entry.manifest, read)?;
        Ok(entry)
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

/// Every translation file `manifest` declares, read through `read`. A file
/// with a key the plugin may not provide (see [`crate::i18n::foreign_keys`])
/// fails the load, naming the keys.
fn read_translations(
    plugin_id: &str,
    manifest: &PluginManifest,
    mut read: impl FnMut(&str) -> Result<String, String>,
) -> Result<Vec<(String, TranslationMap)>, PluginError> {
    manifest
        .translations
        .iter()
        .map(|(lang, path)| {
            let content = read(path).map_err(|e| {
                PluginError::LoadFailed(format!("Failed to read translation file '{path}': {e}"))
            })?;
            let map = toml::from_str(&content).map_err(|e| {
                PluginError::LoadFailed(format!("Invalid translation file syntax in '{path}': {e}"))
            })?;
            let foreign = crate::i18n::foreign_keys(plugin_id, manifest, lang, &map);
            if !foreign.is_empty() {
                return Err(PluginError::LoadFailed(format!(
                    "Translation file '{path}' has keys outside plugin '{plugin_id}': {}. \
                     A plugin's keys start with '{plugin_id}.', or are its manifest text \
                     ('plugin.{plugin_id}.', 'config.{plugin_id}.') or the name of an \
                     identifier it registers ('identifier.<contestType|problemType|checker>.')",
                    foreign.join(", ")
                )));
            }
            Ok((lang.clone(), map))
        })
        .collect()
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

    /// A language pack declares translations only: a pack with a server,
    /// a web bundle or config would run code or take settings under the
    /// host's text, so it does not load.
    #[test]
    fn a_language_pack_declares_only_translations() {
        let pack = |extra: &str| -> PluginManifest {
            toml::from_str(&format!(
                "name = \"p\"\nversion = \"0.1.0\"\n\n[language_pack]\nlocale = \"en\"\n\n\
                 [translations]\nen = \"en.toml\"\n\n{extra}"
            ))
            .unwrap()
        };
        assert!(PluginEntry::new("p".into(), ".".into(), pack("")).is_ok());
        for extra in [
            "[server]\nentry = \"p.wasm\"\n",
            "[web]\nroot = \"dist\"\nentry = \"index.js\"\n",
            "[config.settings]\nscopes = [\"plugin\"]\n",
        ] {
            let error = PluginEntry::new("p".into(), ".".into(), pack(extra))
                .err()
                .unwrap_or_else(|| panic!("a pack with {extra} must not load"))
                .to_string();
            assert!(error.contains("translations only"), "{error}");
        }
    }

    #[test]
    fn a_route_declared_twice_fails_the_load() {
        let manifest: PluginManifest = toml::from_str(
            r#"
name = "test"
version = "0.1.0"

[server]
entry = "test.wasm"

[[server.routes]]
method = "GET"
path = "/contests/{contest_id}/secret"
access = "participant"
contest = "{contest_id}"
handler = "secret"

[[server.routes]]
method = "get"
path = "/contests/{contest_id}/secret"
access = "public"
handler = "secret"
"#,
        )
        .unwrap();

        let error = PluginEntry::new("test".into(), ".".into(), manifest)
            .err()
            .expect("a duplicate route must be refused");
        assert!(
            error
                .to_string()
                .contains("GET /contests/{contest_id}/secret more than once"),
            "{error}"
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
    fn a_translation_key_outside_the_plugin_fails_the_load() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cooldown");
        std::fs::create_dir_all(root.join("i18n")).unwrap();
        std::fs::write(
            root.join("plugin.toml"),
            "name = \"cooldown\"\nversion = \"0.1.0\"\n\n[web]\nroot = \"dist\"\nentry = \"index.js\"\n\n[translations]\nen = \"i18n/en.toml\"\n",
        )
        .unwrap();
        std::fs::write(
            root.join("i18n/en.toml"),
            "'cooldown.ready' = 'Ready'\n'apiError.TOKEN_INVALID' = 'Signed out'\n",
        )
        .unwrap();
        let error = PluginEntry::from_dir(&root)
            .err()
            .expect("a key outside the plugin must fail the load")
            .to_string();
        assert!(error.contains("apiError.TOKEN_INVALID"), "{error}");
        assert!(!error.contains("cooldown.ready"), "{error}");

        std::fs::write(root.join("i18n/en.toml"), "'cooldown.ready' = 'Ready'\n").unwrap();
        let entry = PluginEntry::from_dir(&root).unwrap();
        assert_eq!(entry.translations.len(), 1);
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
