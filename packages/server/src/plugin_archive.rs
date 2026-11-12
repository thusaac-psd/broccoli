//! Plugin archives: the `.tar.gz` an admin uploads to publish a plugin.
//!
//! An archive holds exactly one top-level directory, named after the plugin
//! id, with `plugin.toml` directly inside it. [`PluginArchive::parse`] checks
//! everything that can be checked without loading the plugin; every replica
//! runs it again on the bytes it fetches from the blob store before
//! unpacking them, so a stored archive is never trusted more than an
//! uploaded one.
//!
//! Parsing and unpacking walk the archive through the same function, so the
//! two can never disagree about which entries are allowed.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use common::storage::ContentHash;
use plugin_core::manifest::PluginManifest;
use plugin_core::registry::PluginEntry;

const MIB: usize = 1024 * 1024;

/// Largest compressed archive accepted.
pub const MAX_ARCHIVE_BYTES: usize = 256 * MIB;
/// Largest total size of the files in an archive, once decompressed.
pub const MAX_UNPACKED_BYTES: u64 = 512 * MIB as u64;
/// Most entries (files and directories) an archive may hold.
pub const MAX_ENTRIES: usize = 10_000;

/// Why an archive was rejected. The message is shown to the admin.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ArchiveError(String);

fn reject(message: impl Into<String>) -> ArchiveError {
    ArchiveError(message.into())
}

/// The canonical plugin-id rule, shared by the archive check and the
/// plugin-config handlers. A plugin id becomes a filesystem path segment, so
/// it is strict: 1 to 128 characters, starting with an ASCII letter or digit,
/// otherwise only ASCII letters, digits, `_` and `-`.
pub fn is_valid_plugin_id(id: &str) -> bool {
    id.len() <= 128
        && id.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A validated plugin archive.
pub struct PluginArchive {
    id: String,
    manifest: PluginManifest,
    hash: ContentHash,
    bytes: Vec<u8>,
}

impl std::fmt::Debug for PluginArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginArchive")
            .field("id", &self.id)
            .field("hash", &self.hash)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

impl PluginArchive {
    /// Validate `bytes` as a plugin archive: the size limits, the entry
    /// types (regular files and directories only), the layout, that every
    /// path stays inside the plugin root, that `plugin.toml` parses and
    /// declares valid routes, and that every path it names is inside the
    /// plugin root and its entry points are in the archive.
    pub fn parse(bytes: Vec<u8>) -> Result<Self, ArchiveError> {
        if bytes.len() > MAX_ARCHIVE_BYTES {
            return Err(reject(format!(
                "Archive exceeds the maximum size of {} MiB",
                MAX_ARCHIVE_BYTES / MIB
            )));
        }
        let mut manifest_source = None;
        let Walked { id, files } = walk(&bytes, |path, file| {
            if let Some(reader) = file
                && path == Path::new("plugin.toml")
            {
                let mut source = String::new();
                reader.read_to_string(&mut source).map_err(|e| {
                    if e.kind() == std::io::ErrorKind::InvalidData {
                        reject("plugin.toml is not valid UTF-8 text")
                    } else {
                        unreadable(e)
                    }
                })?;
                manifest_source = Some(source);
            }
            Ok(())
        })?;
        let manifest_source =
            manifest_source.ok_or_else(|| reject(format!("Archive has no {id}/plugin.toml")))?;
        let manifest =
            PluginManifest::parse(&manifest_source).map_err(|e| reject(e.to_string()))?;
        // Route and timer declarations are checked without touching disk.
        PluginEntry::new(id.clone(), PathBuf::new(), manifest.clone())
            .map_err(|e| reject(e.to_string()))?;
        for path in manifest.loaded_paths() {
            if !stays_inside(Path::new(path)) {
                return Err(reject(format!(
                    "plugin.toml names '{path}', which is outside the plugin directory"
                )));
            }
        }
        let entries = [
            manifest.server.as_ref().map(|s| s.entry.as_str()),
            manifest.worker.as_ref().map(|w| w.entry.as_str()),
        ];
        for entry in entries.into_iter().flatten() {
            if !files.contains(Path::new(entry)) {
                return Err(reject(format!(
                    "plugin.toml names entry point '{entry}', which is not in the archive"
                )));
            }
        }
        Ok(Self {
            id,
            manifest,
            hash: ContentHash::compute(&bytes),
            bytes,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// The archive's content hash: its key in the blob store.
    pub fn hash(&self) -> ContentHash {
        self.hash
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Unpack into `dest`, which must not exist yet, as `dest/<id>/...`.
    /// Returns the plugin directory. Blocking.
    pub fn unpack(&self, dest: &Path) -> Result<PathBuf, ArchiveError> {
        let root = dest.join(&self.id);
        let io = |e: std::io::Error| reject(format!("Failed to unpack the archive: {e}"));
        std::fs::create_dir_all(&root).map_err(io)?;
        walk(&self.bytes, |path, file| {
            let target = root.join(path);
            match file {
                None => std::fs::create_dir_all(&target).map_err(io),
                Some(reader) => {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent).map_err(io)?;
                    }
                    let mut out = std::fs::File::create(&target).map_err(io)?;
                    std::io::copy(reader, &mut out).map_err(io)?;
                    Ok(())
                }
            }
        })?;
        Ok(root)
    }
}

/// Whether a manifest-relative path stays inside the plugin directory.
fn stays_inside(path: &Path) -> bool {
    path.components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

fn unreadable(e: std::io::Error) -> ArchiveError {
    reject(format!("Not a valid gzip-compressed tar archive: {e}"))
}

struct Walked {
    id: String,
    files: BTreeSet<PathBuf>,
}

/// Visit every entry of a gzip'd tar archive with its path relative to the
/// plugin directory: `Some(reader)` for a file, `None` for a directory.
/// Enforces the entry count, the unpacked size, the entry types and the
/// single-plugin layout, and rejects duplicate files.
fn walk(
    bytes: &[u8],
    mut visit: impl FnMut(&Path, Option<&mut dyn Read>) -> Result<(), ArchiveError>,
) -> Result<Walked, ArchiveError> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let entries = archive.entries().map_err(unreadable)?;
    let mut id: Option<String> = None;
    let mut files = BTreeSet::new();
    let mut count = 0usize;
    let mut budget = MAX_UNPACKED_BYTES;
    for entry in entries {
        let mut entry = entry.map_err(unreadable)?;
        let kind = entry.header().entry_type();
        if matches!(
            kind,
            tar::EntryType::XGlobalHeader | tar::EntryType::XHeader
        ) {
            continue;
        }
        let is_file = match kind {
            tar::EntryType::Regular => true,
            tar::EntryType::Directory => false,
            _ => {
                return Err(reject(
                    "Archive may contain only regular files and directories",
                ));
            }
        };
        count += 1;
        if count > MAX_ENTRIES {
            return Err(reject(format!(
                "Archive has more than {MAX_ENTRIES} entries"
            )));
        }
        let raw = entry
            .path()
            .map_err(|e| reject(format!("Invalid path in archive: {e}")))?
            .into_owned();
        let mut components = Vec::new();
        for component in raw.components() {
            match component {
                Component::Normal(part) => components.push(part.to_owned()),
                Component::CurDir => {}
                _ => {
                    return Err(reject(format!(
                        "Archive path '{}' leaves the plugin directory",
                        raw.display()
                    )));
                }
            }
        }
        let Some((top, rest)) = components.split_first() else {
            continue;
        };
        let top = top.to_string_lossy();
        match &id {
            Some(existing) if *existing != top => {
                return Err(reject(
                    "Archive must contain exactly one top-level directory",
                ));
            }
            Some(_) => {}
            None if !is_valid_plugin_id(&top) => {
                return Err(reject(format!(
                    "Invalid plugin ID '{top}'. Must match [a-zA-Z0-9][a-zA-Z0-9_-]*"
                )));
            }
            None => id = Some(top.into_owned()),
        }
        let path: PathBuf = rest.iter().collect();
        if path.as_os_str().is_empty() {
            if is_file {
                return Err(reject(
                    "Archive must contain exactly one top-level directory",
                ));
            }
            continue;
        }
        if !is_file {
            visit(&path, None)?;
            continue;
        }
        if !files.insert(path.clone()) {
            return Err(reject(format!(
                "Archive contains '{}' more than once",
                raw.display()
            )));
        }
        let too_large = || {
            reject(format!(
                "Archive unpacks to more than {} MiB",
                MAX_UNPACKED_BYTES / MIB as u64
            ))
        };
        if entry.size() > budget {
            return Err(too_large());
        }
        // Cap what is read at the remaining budget, whatever the header
        // claims, and drain what the visitor left so the cap sees it all.
        let mut limited = (&mut entry).take(budget + 1);
        visit(&path, Some(&mut limited))?;
        std::io::copy(&mut limited, &mut std::io::sink()).map_err(unreadable)?;
        let used = budget + 1 - limited.limit();
        if used > budget {
            return Err(too_large());
        }
        budget -= used;
    }
    let id = id.ok_or_else(|| reject("Archive is empty"))?;
    Ok(Walked { id, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"
name = "demo"
version = "1.2.0"

[server]
entry = "demo.wasm"
permissions = ["storage"]
"#;

    enum Item<'a> {
        File(&'a str, &'a [u8]),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
    }

    fn archive(items: &[Item]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for item in items {
            let mut header = tar::Header::new_gnu();
            match item {
                Item::File(path, data) => {
                    header.set_entry_type(tar::EntryType::Regular);
                    header.set_size(data.len() as u64);
                    header.set_mode(0o644);
                    set_raw_path(&mut header, path);
                    header.set_cksum();
                    builder.append(&header, *data).unwrap();
                }
                Item::Dir(path) => {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(0o755);
                    set_raw_path(&mut header, path);
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
                Item::Symlink(path, target) => {
                    header.set_entry_type(tar::EntryType::Symlink);
                    header.set_size(0);
                    set_raw_path(&mut header, path);
                    header.set_link_name(target).unwrap();
                    header.set_cksum();
                    builder.append(&header, std::io::empty()).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// Write the path bytes as given: `Header::set_path` refuses `..`,
    /// which is exactly what some tests need to smuggle in.
    fn set_raw_path(header: &mut tar::Header, path: &str) {
        let name = &mut header.as_old_mut().name;
        name.fill(0);
        name[..path.len()].copy_from_slice(path.as_bytes());
    }

    fn valid() -> Vec<u8> {
        archive(&[
            Item::Dir("demo/"),
            Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            Item::File("demo/demo.wasm", b"\0asm"),
        ])
    }

    fn error(bytes: Vec<u8>) -> String {
        PluginArchive::parse(bytes)
            .expect_err("the archive must be rejected")
            .to_string()
    }

    #[test]
    fn a_valid_archive_parses_and_unpacks_under_its_id() {
        let bytes = valid();
        let archive = PluginArchive::parse(bytes.clone()).unwrap();
        assert_eq!(archive.id(), "demo");
        assert_eq!(archive.manifest().version, "1.2.0");
        assert_eq!(archive.hash(), ContentHash::compute(&bytes));

        let dir = tempfile::tempdir().unwrap();
        let root = archive.unpack(&dir.path().join("x")).unwrap();
        assert_eq!(root, dir.path().join("x/demo"));
        assert_eq!(std::fs::read(root.join("demo.wasm")).unwrap(), b"\0asm");
        PluginEntry::from_dir(&root).unwrap();
    }

    #[test]
    fn rejects_bytes_that_are_not_an_archive() {
        assert!(error(b"not gzip".to_vec()).contains("Not a valid gzip-compressed tar"));
        let truncated = valid()[..100].to_vec();
        assert!(error(truncated).contains("Not a valid gzip-compressed tar"));
    }

    #[test]
    fn rejects_paths_that_leave_the_plugin_directory() {
        let bytes = archive(&[
            Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            Item::File("demo/../escape", b"x"),
        ]);
        assert!(error(bytes).contains("leaves the plugin directory"));
        let bytes = archive(&[Item::File("/demo/plugin.toml", MANIFEST.as_bytes())]);
        assert!(error(bytes).contains("leaves the plugin directory"));
    }

    #[test]
    fn rejects_links() {
        let bytes = archive(&[
            Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            Item::Symlink("demo/demo.wasm", "/etc/passwd"),
        ]);
        assert!(error(bytes).contains("only regular files and directories"));
    }

    #[test]
    fn rejects_a_second_top_level_entry() {
        let bytes = archive(&[
            Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            Item::File("other/plugin.toml", MANIFEST.as_bytes()),
        ]);
        assert!(error(bytes).contains("exactly one top-level directory"));
        // A file, not a directory, at the top.
        let bytes = archive(&[Item::File("demo", MANIFEST.as_bytes())]);
        assert!(error(bytes).contains("exactly one top-level directory"));
    }

    #[test]
    fn rejects_an_invalid_plugin_id() {
        let bytes = archive(&[Item::File("-demo/plugin.toml", MANIFEST.as_bytes())]);
        assert!(error(bytes).contains("Invalid plugin ID"));
    }

    #[test]
    fn rejects_a_missing_or_broken_manifest() {
        let bytes = archive(&[Item::File("demo/demo.wasm", b"\0asm")]);
        assert!(error(bytes).contains("no demo/plugin.toml"));
        let bytes = archive(&[Item::File("demo/plugin.toml", b"name = ")]);
        assert!(error(bytes).contains("Invalid manifest syntax"));
    }

    #[test]
    fn rejects_a_manifest_naming_files_the_archive_lacks_or_cannot_hold() {
        let bytes = archive(&[Item::File("demo/plugin.toml", MANIFEST.as_bytes())]);
        assert!(error(bytes).contains("entry point 'demo.wasm'"));
        let manifest = MANIFEST.replace("demo.wasm", "../demo.wasm");
        let bytes = archive(&[Item::File("demo/plugin.toml", manifest.as_bytes())]);
        assert!(error(bytes).contains("outside the plugin directory"));
    }

    #[test]
    fn rejects_duplicate_files() {
        let bytes = archive(&[
            Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            Item::File("demo/demo.wasm", b"a"),
            Item::File("demo/./demo.wasm", b"b"),
        ]);
        assert!(error(bytes).contains("more than once"));
    }

    #[test]
    fn rejects_an_archive_that_unpacks_too_large() {
        // A header that claims more than the budget is refused before any
        // of the content is read.
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(MAX_UNPACKED_BYTES + 1);
        header.set_path("demo/big").unwrap();
        header.set_cksum();
        builder.append(&header, std::io::empty()).ok();
        let bytes = builder.into_inner().unwrap().finish().unwrap();
        assert!(error(bytes).contains("unpacks to more than"));
    }

    #[test]
    fn plugin_id_rule() {
        assert!(is_valid_plugin_id("a"));
        assert!(is_valid_plugin_id("Plugin_1-x"));
        assert!(!is_valid_plugin_id(""));
        assert!(!is_valid_plugin_id("_a"));
        assert!(!is_valid_plugin_id("a.b"));
        assert!(!is_valid_plugin_id(&"a".repeat(129)));
        assert!(is_valid_plugin_id(&"a".repeat(128)));
    }
}
