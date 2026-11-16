//! Where a server finds a plugin's files.
//!
//! A plugin comes from one of two [`PluginSource`]s:
//!
//! - the bundle: `plugin.plugins_dir/<id>/`, shipped on every node and only
//!   ever read, so it may be mounted read-only;
//! - an upload: a plugin archive (see [`crate::plugin_archive`]) stored in the
//!   shared blob store under its content hash, which the plugin row records
//!   in `archive_hash`.
//!
//! The plugin row is the one record of which source the fleet runs: while it
//! names an archive, that upload is what every server loads, even where the
//! bundle has a copy of the same plugin. Removing the upload clears the
//! column and the bundled copy, if any, takes over again.
//!
//! Each server unpacks the archives it needs into its own
//! `plugin.cache_dir/<archive hash>/<id>/`. A cache directory is only ever
//! created whole (unpacked beside it, then renamed into place), so one that
//! exists is complete; and an archive fetched from the blob store is checked
//! against its hash and validated again before it is unpacked. Cache
//! changes happen under the lifecycle lock (`crate::plugin_lifecycle`), so
//! unpacking and [`retain`] never run concurrently on one server.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use common::storage::ContentHash;
use plugin_core::error::PluginError;

use crate::plugin_archive::{MAX_ARCHIVE_BYTES, PluginArchive};
use crate::plugin_id::PluginId;
use crate::state::AppState;

/// Where the fleet takes a plugin's files from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// This node's bundled copy.
    Bundle,
    /// An uploaded archive, by the hex content hash the plugin row records.
    /// Parsed only when the archive is fetched, so a malformed value fails
    /// that plugin's load like any other unusable archive.
    Upload(String),
}

impl PluginSource {
    /// The source a plugin row's `archive_hash` column names.
    pub fn of(archive_hash: Option<&str>) -> Self {
        match archive_hash {
            Some(hash) => PluginSource::Upload(hash.to_string()),
            None => PluginSource::Bundle,
        }
    }

    /// The value for the `archive_hash` column.
    pub fn archive_hash(&self) -> Option<&str> {
        match self {
            PluginSource::Bundle => None,
            PluginSource::Upload(hash) => Some(hash),
        }
    }
}

/// Where a plugin's files come from, as the admin API reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginSourceKind {
    /// The copy shipped in each node's plugins directory.
    Bundle,
    /// An archive uploaded through the admin API, served from the shared
    /// blob store. Takes precedence over a bundled copy.
    Upload,
}

impl PluginSource {
    pub fn kind(&self) -> PluginSourceKind {
        match self {
            PluginSource::Bundle => PluginSourceKind::Bundle,
            PluginSource::Upload(_) => PluginSourceKind::Upload,
        }
    }
}

/// The plugin's directory in this node's bundle, if the bundle has it.
pub fn bundled_root(state: &AppState, plugin_id: &PluginId) -> Option<PathBuf> {
    let root = state.config.plugin.plugins_dir.join(plugin_id.as_str());
    root.join("plugin.toml").is_file().then_some(root)
}

/// The directory to load `plugin_id` from on this server, fetching and
/// unpacking an uploaded archive first if it is not cached here yet.
pub async fn locate(
    state: &AppState,
    plugin_id: &PluginId,
    source: &PluginSource,
) -> Result<PathBuf, PluginError> {
    match source {
        PluginSource::Bundle => bundled_root(state, plugin_id)
            .ok_or_else(|| PluginError::NotFound(plugin_id.to_string())),
        PluginSource::Upload(hash) => {
            if !state.config.plugin.uploads_enabled {
                return Err(PluginError::LoadFailed(
                    "the published version is an upload, and plugin uploads are disabled on \
                     this server (plugin.uploads_enabled)"
                        .into(),
                ));
            }
            materialize(state, plugin_id, hash).await
        }
    }
}

fn cached_root(state: &AppState, hash: &ContentHash, plugin_id: &PluginId) -> PathBuf {
    state
        .config
        .plugin
        .cache_dir
        .join(hash.to_hex())
        .join(plugin_id.as_str())
}

fn load_failed(message: impl std::fmt::Display) -> PluginError {
    PluginError::LoadFailed(message.to_string())
}

async fn materialize(
    state: &AppState,
    plugin_id: &PluginId,
    hash: &str,
) -> Result<PathBuf, PluginError> {
    let hash = ContentHash::from_hex(hash)
        .map_err(|e| load_failed(format!("invalid published archive hash: {e}")))?;
    let root = cached_root(state, &hash, plugin_id);
    if tokio::fs::try_exists(&root).await.unwrap_or(false) {
        return Ok(root);
    }
    let fetch_failed = |e: common::storage::StorageError| {
        load_failed(format!("cannot fetch the published archive {hash}: {e}"))
    };
    let size = state.blob_store.size(&hash).await.map_err(fetch_failed)?;
    if size > MAX_ARCHIVE_BYTES as u64 {
        return Err(load_failed(format!(
            "the published archive {hash} is larger than an upload may be"
        )));
    }
    let bytes = state.blob_store.get(&hash).await.map_err(fetch_failed)?;
    let actual = ContentHash::compute(&bytes);
    if actual != hash {
        return Err(load_failed(format!(
            "the blob store returned content hashing to {actual} for the published archive {hash}"
        )));
    }
    let archive = PluginArchive::parse(bytes)
        .map_err(|e| load_failed(format!("the published archive {hash} is invalid: {e}")))?;
    if archive.id() != plugin_id {
        return Err(load_failed(format!(
            "the published archive {hash} holds plugin '{}', not '{plugin_id}'",
            archive.id()
        )));
    }
    store(state, archive).await
}

/// Unpack `archive` into this server's cache, unless it is there already.
/// Returns the plugin directory.
pub async fn store(state: &AppState, archive: PluginArchive) -> Result<PathBuf, PluginError> {
    let cache_dir = state.config.plugin.cache_dir.clone();
    let root = cached_root(state, &archive.hash(), archive.id());
    tokio::task::spawn_blocking(move || {
        if root.exists() {
            return Ok(root);
        }
        let io = |e: std::io::Error| load_failed(format!("plugin cache: {e}"));
        std::fs::create_dir_all(&cache_dir).map_err(io)?;
        let hash_dir = cache_dir.join(archive.hash().to_hex());
        // A crash mid-unpack leaves only a `.partial` directory, which the
        // next `retain` removes.
        let partial = cache_dir.join(format!(".partial-{}", archive.hash().to_hex()));
        if partial.exists() {
            std::fs::remove_dir_all(&partial).map_err(io)?;
        }
        archive.unpack(&partial).map_err(load_failed)?;
        // Durable before it is visible: a cache entry that exists is
        // trusted to be complete, even after a power loss.
        sync_dirs(&partial).map_err(io)?;
        std::fs::rename(&partial, &hash_dir).map_err(io)?;
        sync_dir(&cache_dir).map_err(io)?;
        Ok(root)
    })
    .await
    .map_err(|e| PluginError::Internal(format!("plugin cache task failed: {e}")))?
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

/// Flush `dir` and every directory below it (the files are flushed as they
/// are written).
fn sync_dirs(dir: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_dirs(&entry.path())?;
        }
    }
    sync_dir(dir)
}

/// Drop this server's unpacked copy of an uploaded archive, so the next
/// [`locate`] fetches and verifies it again. Best-effort.
pub async fn evict(state: &AppState, source: &PluginSource) {
    let Some(entry) = source.archive_hash().and_then(cache_entry_for) else {
        return;
    };
    let dir = state.config.plugin.cache_dir.join(entry);
    if let Err(e) = tokio::fs::remove_dir_all(&dir).await
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(path = %dir.display(), error = %e, "Failed to drop a plugin cache entry");
    }
}

/// Delete every cache entry whose archive is not in `keep`: neither
/// published for any plugin nor loaded on this server. The compiled-module
/// entry ([`plugin_core::config::COMPILED_MODULES_ENTRY`]) always stays.
/// Best-effort.
pub async fn retain(state: &AppState, mut keep: HashSet<String>) {
    keep.insert(plugin_core::config::COMPILED_MODULES_ENTRY.to_string());
    let cache_dir = state.config.plugin.cache_dir.clone();
    let result = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let entries = match std::fs::read_dir(&cache_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                tracing::info!(entry = %name, "Removing an unused plugin cache entry");
                std::fs::remove_dir_all(entry.path())?;
            }
        }
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "Failed to clean the plugin cache"),
        Err(e) => tracing::warn!(error = %e, "Plugin cache cleanup task failed"),
    }
}

/// The cache entry name (archive hash) of `root`, if it is a cached upload.
pub fn cache_entry_of(state: &AppState, root: &Path) -> Option<String> {
    let relative = root.strip_prefix(&state.config.plugin.cache_dir).ok()?;
    Some(
        relative
            .components()
            .next()?
            .as_os_str()
            .to_string_lossy()
            .into_owned(),
    )
}

/// The cache entry name a published `archive_hash` column value maps to.
pub fn cache_entry_for(archive_hash: &str) -> Option<String> {
    ContentHash::from_hex(archive_hash).ok().map(|h| h.to_hex())
}
