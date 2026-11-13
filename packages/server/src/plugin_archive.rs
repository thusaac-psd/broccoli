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

use axum::body::Bytes;
use common::storage::ContentHash;

use crate::plugin_id::PluginId;
use plugin_core::manifest::{PluginManifest, parse_manifest};
use plugin_core::registry::PluginEntry;

const MIB: usize = 1024 * 1024;

/// Largest compressed archive accepted.
pub const MAX_ARCHIVE_BYTES: usize = 256 * MIB;
/// Largest total size of the files in an archive, once decompressed.
pub const MAX_UNPACKED_BYTES: u64 = 512 * MIB as u64;
/// Most entries (files and directories) an archive may hold.
pub const MAX_ENTRIES: usize = 10_000;
/// Largest GNU long-name or long-link record and largest PAX extension
/// record. The tar reader holds each one in memory whole before it hands
/// over the entry it describes.
const MAX_EXTENSION_RECORD_BYTES: u64 = 64 * 1024;
/// Decompressed bytes allowed beyond [`MAX_UNPACKED_BYTES`]: the 512-byte
/// headers, block padding and extension records.
const MAX_FRAMING_BYTES: u64 = 64 * MIB as u64;
/// Most tar records (entries plus the extension records attached to them).
const MAX_RECORDS: usize = 3 * MAX_ENTRIES;

/// Why an archive was rejected. The message is shown to the admin.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ArchiveError(String);

fn reject(message: impl Into<String>) -> ArchiveError {
    ArchiveError(message.into())
}

/// A validated plugin archive.
pub struct PluginArchive {
    id: PluginId,
    manifest: PluginManifest,
    hash: ContentHash,
    bytes: Bytes,
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
    /// plugin root and its server entry point is in the archive.
    pub fn parse(bytes: impl Into<Bytes>) -> Result<Self, ArchiveError> {
        let bytes: Bytes = bytes.into();
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
            parse_manifest(&manifest_source).map_err(|e| reject(e.to_string()))?;
        // Route and timer declarations are checked without touching disk.
        PluginEntry::new(id.to_string(), PathBuf::new(), manifest.clone())
            .map_err(|e| reject(e.to_string()))?;
        for path in manifest.loaded_paths() {
            if !stays_inside(Path::new(path)) {
                return Err(reject(format!(
                    "plugin.toml names '{path}', which is outside the plugin directory"
                )));
            }
        }
        if let Some(server) = &manifest.server
            && !files.contains(Path::new(&server.entry))
        {
            return Err(reject(format!(
                "plugin.toml names entry point '{}', which is not in the archive",
                server.entry
            )));
        }
        Ok(Self {
            id,
            manifest,
            hash: ContentHash::compute(&bytes),
            bytes,
        })
    }

    pub fn id(&self) -> &PluginId {
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

    /// Unpack into `dest`, which must not exist yet, as `dest/<id>/...`,
    /// each file flushed to disk. Returns the plugin directory. Blocking.
    pub fn unpack(&self, dest: &Path) -> Result<PathBuf, ArchiveError> {
        let root = dest.join(self.id.as_str());
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
                    out.sync_all().map_err(io)
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
    if e.kind() == std::io::ErrorKind::FileTooLarge {
        return too_large();
    }
    reject(format!("Not a valid gzip-compressed tar archive: {e}"))
}

fn too_large() -> ArchiveError {
    reject(format!(
        "Archive unpacks to more than {} MiB",
        MAX_UNPACKED_BYTES / MIB as u64
    ))
}

/// A reader that fails with [`std::io::ErrorKind::FileTooLarge`] once more
/// than `left` bytes have come through it.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self
            .left
            .checked_sub(n as u64)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::FileTooLarge))?;
        Ok(n)
    }
}

/// The archive in `bytes`, decompressed through a cap on every byte the tar
/// reader can pull: file contents, headers and extension records alike.
fn open(bytes: &[u8]) -> tar::Archive<Capped<flate2::read::GzDecoder<&[u8]>>> {
    tar::Archive::new(Capped {
        inner: flate2::read::GzDecoder::new(bytes),
        left: MAX_UNPACKED_BYTES + MAX_FRAMING_BYTES,
    })
}

/// Check every raw tar record before [`walk`] reads the archive the usual
/// way. In raw mode the tar reader buffers nothing, whereas the usual mode
/// reads extension records (and GNU sparse maps) into memory before handing
/// over an entry, so their size must be known to be small first.
fn check_records(bytes: &[u8]) -> Result<(), ArchiveError> {
    let mut archive = open(bytes);
    let entries = archive.entries().map_err(unreadable)?.raw(true);
    let mut declared: u64 = 0;
    for (count, entry) in entries.enumerate() {
        let entry = entry.map_err(unreadable)?;
        if count >= MAX_RECORDS {
            return Err(reject(format!(
                "Archive has more than {MAX_ENTRIES} entries"
            )));
        }
        match entry.header().entry_type() {
            tar::EntryType::Regular => {
                declared = declared.saturating_add(entry.size());
                if declared > MAX_UNPACKED_BYTES {
                    return Err(too_large());
                }
            }
            tar::EntryType::Directory => {}
            tar::EntryType::GNULongName
            | tar::EntryType::GNULongLink
            | tar::EntryType::XHeader
            | tar::EntryType::XGlobalHeader => {
                if entry.size() > MAX_EXTENSION_RECORD_BYTES {
                    return Err(reject(format!(
                        "Archive has a long-name or PAX extension record over {} KiB",
                        MAX_EXTENSION_RECORD_BYTES / 1024
                    )));
                }
            }
            _ => return Err(only_files_and_dirs()),
        }
    }
    Ok(())
}

fn only_files_and_dirs() -> ArchiveError {
    reject("Archive may contain only regular files and directories")
}

struct Walked {
    id: PluginId,
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
    check_records(bytes)?;
    let mut archive = open(bytes);
    let entries = archive.entries().map_err(unreadable)?;
    let mut id: Option<PluginId> = None;
    let mut files = BTreeSet::new();
    // Every directory the entries name or imply, so that no path is used as
    // both a file and a directory: unpacking would fail on it.
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let both = |path: &Path| {
        reject(format!(
            "Archive has '{}' as both a file and a directory",
            path.display()
        ))
    };
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
            _ => return Err(only_files_and_dirs()),
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
            Some(existing) if existing.as_str() != top.as_ref() => {
                return Err(reject(
                    "Archive must contain exactly one top-level directory",
                ));
            }
            Some(_) => {}
            None => id = Some(PluginId::parse(top).map_err(|e| reject(e.to_string()))?),
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
        // A file implies its parents; a directory, itself and its parents.
        let implied: Vec<PathBuf> = path
            .ancestors()
            .skip(usize::from(is_file))
            .filter(|a| !a.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .collect();
        if let Some(clash) = implied.iter().find(|d| files.contains(*d)) {
            return Err(both(clash));
        }
        dirs.extend(implied);
        if !is_file {
            visit(&path, None)?;
            continue;
        }
        if dirs.contains(&path) {
            return Err(both(&path));
        }
        if !files.insert(path.clone()) {
            return Err(reject(format!(
                "Archive contains '{}' more than once",
                raw.display()
            )));
        }
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
        assert_eq!(archive.id().as_str(), "demo");
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
    fn rejects_a_path_that_is_both_a_file_and_a_directory() {
        for items in [
            vec![
                Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
                Item::File("demo/demo.wasm", b"x"),
                Item::File("demo/a", b"x"),
                Item::File("demo/a/b", b"x"),
            ],
            vec![
                Item::File("demo/demo.wasm", b"x"),
                Item::File("demo/a/b", b"x"),
                Item::File("demo/a", b"x"),
                Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            ],
            vec![
                Item::Dir("demo/plugin.toml/"),
                Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            ],
            vec![
                Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
                Item::Dir("demo/plugin.toml/"),
            ],
        ] {
            let message = error(archive(&items));
            assert!(message.contains("both a file and a directory"), "{message}");
        }
        // A directory listed explicitly and implied by its files is fine.
        let bytes = archive(&[
            Item::Dir("demo/"),
            Item::Dir("demo/web/"),
            Item::File("demo/web/index.js", b"x"),
            Item::File("demo/plugin.toml", MANIFEST.as_bytes()),
            Item::File("demo/demo.wasm", b"x"),
        ]);
        let archive = PluginArchive::parse(bytes).unwrap();
        let dir = tempfile::tempdir().unwrap();
        archive.unpack(&dir.path().join("x")).unwrap();
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

    /// One raw tar record: a header of `kind` for `path`, then `data`.
    fn record(out: &mut Vec<u8>, kind: tar::EntryType, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(kind);
        header.set_size(data.len() as u64);
        set_raw_path(&mut header, path);
        header.set_cksum();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }

    fn gzip(tar: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        encoder.write_all(tar).unwrap();
        encoder.write_all(&[0; 1024]).unwrap();
        encoder.finish().unwrap()
    }

    /// Extension records compress to almost nothing: a few KiB on the wire
    /// describing megabytes that the tar reader would hold in memory.
    #[test]
    fn rejects_oversized_extension_records_before_reading_them() {
        for kind in [
            tar::EntryType::XHeader,
            tar::EntryType::GNULongName,
            tar::EntryType::GNULongLink,
        ] {
            let mut tar = Vec::new();
            let huge = vec![b'a'; 8 * MIB];
            record(&mut tar, kind, "././@LongLink", &huge);
            record(
                &mut tar,
                tar::EntryType::Regular,
                "demo/plugin.toml",
                MANIFEST.as_bytes(),
            );
            let bytes = gzip(&tar);
            assert!(bytes.len() < 64 * 1024, "{kind:?}: {}", bytes.len());
            let message = error(bytes);
            assert!(
                message.contains("extension record over 64 KiB"),
                "{kind:?}: {message}"
            );
        }
    }

    #[test]
    fn rejects_sparse_files_before_reading_their_maps() {
        let mut tar = Vec::new();
        record(&mut tar, tar::EntryType::GNUSparse, "demo/sparse", &[]);
        assert!(error(gzip(&tar)).contains("only regular files and directories"));
    }

    #[test]
    fn the_decompression_cap_counts_every_byte() {
        let mut capped = Capped {
            inner: std::io::repeat(0),
            left: 10_000,
        };
        let err = std::io::copy(&mut capped, &mut std::io::sink()).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::FileTooLarge);
        assert!(unreadable(err).to_string().contains("unpacks to more than"));
    }
}
