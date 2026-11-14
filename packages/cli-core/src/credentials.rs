//! `credentials.json`: the access/refresh token pair saved per server, keyed
//! by the server's normalised [`ServerUrl`]. The first entry is the active
//! server: the one `broccoli login` signed in to last, which commands use when
//! no server is given.
//!
//! A pair is always written whole: there is no way to store an access token
//! without the refresh token issued with it. Several CLI processes (for
//! example `broccoli watch` and `broccoli submit`) share the file, so every
//! write goes through [`CredentialsLock`], an exclusive lock on a sidecar file.
//! Holding the lock is the only way to obtain a writer, which makes an unlocked
//! read-modify-write impossible to express. Writes replace the file
//! atomically, so an unlocked reader never sees a partial file.
//!
//! A damaged or foreign file never makes the CLI unusable: entries that do not
//! parse are treated as absent (see [`CredentialsFile::damage`] for the
//! warning), and the next login rewrites the file from the entries that do.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::server::ServerUrl;

/// An access token and the refresh token issued together with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokens {
    pub token: String,
    pub refresh_token: String,
}

#[derive(Serialize, Default)]
struct FileContents {
    entries: Vec<Entry>,
}

/// The file as stored, before each entry is checked on its own.
#[derive(Deserialize)]
struct RawContents {
    entries: Vec<serde_json::Value>,
}

/// The entries that parsed, and what was wrong with the rest of the file.
struct Loaded {
    contents: FileContents,
    damage: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    server: String,
    #[serde(flatten)]
    tokens: StoredTokens,
}

/// Location of a credentials file and its lock.
#[derive(Debug, Clone)]
pub struct CredentialsFile {
    path: PathBuf,
}

impl CredentialsFile {
    /// `~/.config/broccoli/credentials.json`.
    pub fn default_location() -> Self {
        Self::at(crate::config::config_dir().join("credentials.json"))
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The pair saved for `server`.
    pub fn read(&self, server: &ServerUrl) -> Option<StoredTokens> {
        self.load()
            .contents
            .entries
            .into_iter()
            .find(|e| e.server == server.as_str())
            .map(|e| e.tokens)
    }

    /// The active server (the last one logged in to) and its pair.
    pub fn active(&self) -> Option<(ServerUrl, StoredTokens)> {
        self.load()
            .contents
            .entries
            .into_iter()
            .next()
            .and_then(|e| Some((e.server.parse().ok()?, e.tokens)))
    }

    /// Why part or all of the file is being ignored, if it is.
    pub fn damage(&self) -> Option<String> {
        self.load().damage
    }

    /// Block until no other process holds the lock, then take it.
    ///
    /// Some home directories (NFS without a lock manager) cannot lock at all.
    /// Refusing to log in or refresh there would make the CLI unusable, so the
    /// lock degrades to unlocked access with a one-time warning. The cost is
    /// the race the lock prevents: two processes refreshing at the same moment
    /// can each spend the refresh token, and the slower one's session ends
    /// (the server allows a short replay grace, so this needs near-simultaneous
    /// refreshes).
    pub fn lock(&self) -> anyhow::Result<CredentialsLock> {
        let lock = self.open_lock()?;
        let held = match lock.lock() {
            Ok(()) => Some(lock),
            Err(e) if locking_unsupported(&e) => {
                warn_unlocked(&e);
                None
            }
            Err(e) => return Err(e).context("Failed to lock credentials"),
        };
        Ok(CredentialsLock {
            file: self.clone(),
            _lock: held,
        })
    }

    /// Check, without waiting, whether this file system supports the lock,
    /// so the one-time warning appears before any long-running command starts.
    pub fn check_locking(&self) {
        if let Ok(lock) = self.open_lock()
            && let Err(std::fs::TryLockError::Error(e)) = lock.try_lock()
            && locking_unsupported(&e)
        {
            warn_unlocked(&e);
        }
    }

    fn open_lock(&self) -> anyhow::Result<File> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir).context("Failed to create config directory")?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.lock_path())
            .context("Failed to open credentials lock")
    }

    fn lock_path(&self) -> PathBuf {
        let mut name = self.path.file_name().unwrap_or_default().to_os_string();
        name.push(".lock");
        self.path.with_file_name(name)
    }

    fn load(&self) -> Loaded {
        let path = self.path.display();
        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded {
                    contents: FileContents::default(),
                    damage: None,
                };
            }
            Err(e) => {
                return Loaded {
                    contents: FileContents::default(),
                    damage: Some(format!("cannot read {path}: {e}")),
                };
            }
        };
        let raw: RawContents = match serde_json::from_str(&text) {
            Ok(raw) => raw,
            Err(e) => {
                return Loaded {
                    contents: FileContents::default(),
                    damage: Some(format!("{path} is not a credentials file ({e})")),
                };
            }
        };
        let total = raw.entries.len();
        // An entry must name its server in normalised form, or no lookup
        // could ever find it.
        let entries: Vec<Entry> = raw
            .entries
            .into_iter()
            .filter_map(|value| serde_json::from_value::<Entry>(value).ok())
            .filter(|e| {
                e.server
                    .parse::<ServerUrl>()
                    .is_ok_and(|url| url.as_str() == e.server)
            })
            .collect();
        let skipped = total - entries.len();
        Loaded {
            contents: FileContents { entries },
            damage: (skipped > 0).then(|| format!("{skipped} entry(ies) in {path} are invalid")),
        }
    }
}

/// Exclusive access to a credentials file (where the file system supports
/// locking); released on drop.
pub struct CredentialsLock {
    file: CredentialsFile,
    _lock: Option<File>,
}

fn locking_unsupported(e: &std::io::Error) -> bool {
    #[cfg(unix)]
    if e.raw_os_error() == Some(libc::ENOLCK) {
        return true;
    }
    e.kind() == std::io::ErrorKind::Unsupported
}

fn warn_unlocked(e: &std::io::Error) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "Warning: this file system cannot lock the credentials file ({e}); \
             running several broccoli commands at once may end the session."
        );
    });
}

impl CredentialsLock {
    pub fn read(&self, server: &ServerUrl) -> Option<StoredTokens> {
        self.file.read(server)
    }

    /// Save the pair a login obtained for `server` and make it the active
    /// server. Other servers' valid entries are kept; anything unreadable is
    /// replaced.
    pub fn save_login(&self, server: &ServerUrl, tokens: &StoredTokens) -> anyhow::Result<()> {
        let mut contents = self.file.load().contents;
        contents.entries.retain(|e| e.server != server.as_str());
        contents.entries.insert(0, Entry::new(server, tokens));
        self.replace(&contents)
    }

    /// Save the pair a refresh rotated `server`'s session to. The active
    /// server stays what it was: refreshing a session is not logging in.
    pub fn save_rotation(&self, server: &ServerUrl, tokens: &StoredTokens) -> anyhow::Result<()> {
        let mut contents = self.file.load().contents;
        match contents
            .entries
            .iter_mut()
            .find(|e| e.server == server.as_str())
        {
            Some(entry) => entry.tokens = tokens.clone(),
            None => contents.entries.push(Entry::new(server, tokens)),
        }
        self.replace(&contents)
    }

    /// Forget `server`'s pair. Returns whether one was saved. The next saved
    /// server, if any, becomes the active one.
    pub fn remove(&self, server: &ServerUrl) -> anyhow::Result<bool> {
        let mut contents = self.file.load().contents;
        let before = contents.entries.len();
        contents.entries.retain(|e| e.server != server.as_str());
        if contents.entries.len() == before {
            return Ok(false);
        }
        self.replace(&contents)?;
        Ok(true)
    }

    fn replace(&self, contents: &FileContents) -> anyhow::Result<()> {
        let text = serde_json::to_string_pretty(contents)?;
        let tmp = self.file.path.with_extension("json.tmp");
        write_private(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &self.file.path).context("Failed to replace credentials file")
    }
}

impl Entry {
    fn new(server: &ServerUrl, tokens: &StoredTokens) -> Self {
        Self {
            server: server.to_string(),
            tokens: tokens.clone(),
        }
    }
}

/// Create `path` readable by the owner only from the start, so the tokens are
/// never briefly world-readable.
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    // A leftover file from an interrupted write keeps its old mode, and the
    // mode below applies only to a newly created file, so start from none.
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("Failed to remove a stale credentials file"),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .context("Failed to open credentials file")?;
    file.write_all(bytes)
        .context("Failed to write credentials file")?;
    file.sync_all().context("Failed to write credentials file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(token: &str, refresh: &str) -> StoredTokens {
        StoredTokens {
            token: token.into(),
            refresh_token: refresh.into(),
        }
    }

    fn url(s: &str) -> ServerUrl {
        s.parse().unwrap()
    }

    #[test]
    fn a_login_replaces_one_server_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        let lock = file.lock().unwrap();
        lock.save_login(&url("https://a"), &pair("a1", "ra1"))
            .unwrap();
        lock.save_login(&url("https://b"), &pair("b1", "rb1"))
            .unwrap();
        lock.save_login(&url("https://a"), &pair("a2", "ra2"))
            .unwrap();
        drop(lock);

        assert_eq!(file.read(&url("https://a")), Some(pair("a2", "ra2")));
        assert_eq!(file.read(&url("https://b/")), Some(pair("b1", "rb1")));
        let (server, _) = file.active().unwrap();
        assert_eq!(server, url("https://a"));
    }

    #[test]
    fn a_refresh_keeps_the_active_server() {
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        let lock = file.lock().unwrap();
        lock.save_login(&url("https://a"), &pair("a1", "ra1"))
            .unwrap();
        lock.save_login(&url("https://b"), &pair("b1", "rb1"))
            .unwrap();
        lock.save_rotation(&url("https://a"), &pair("a2", "ra2"))
            .unwrap();
        drop(lock);

        assert_eq!(file.active().unwrap().0, url("https://b"));
        assert_eq!(file.read(&url("https://a")), Some(pair("a2", "ra2")));
    }

    #[test]
    fn removing_the_active_server_activates_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        let lock = file.lock().unwrap();
        lock.save_login(&url("https://a"), &pair("a", "ra"))
            .unwrap();
        lock.save_login(&url("https://b"), &pair("b", "rb"))
            .unwrap();
        assert!(lock.remove(&url("https://b")).unwrap());
        assert!(!lock.remove(&url("https://b")).unwrap());
        drop(lock);

        assert_eq!(file.active().unwrap().0, url("https://a"));
        assert_eq!(file.read(&url("https://b")), None);
    }

    #[test]
    fn an_invalid_entry_is_skipped_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(
            &path,
            r#"{"entries":[{"server":"https://x","token":"t"},
                {"server":"https://y","token":"t","refresh_token":"r"}]}"#,
        )
        .unwrap();
        let file = CredentialsFile::at(path);
        assert_eq!(file.read(&url("https://x")), None);
        assert_eq!(file.read(&url("https://y")), Some(pair("t", "r")));
        assert!(file.damage().unwrap().contains("1 entry"));
    }

    #[test]
    fn a_login_replaces_a_file_that_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "not json at all").unwrap();
        let file = CredentialsFile::at(&path);
        assert_eq!(file.active(), None);
        assert!(file.damage().is_some());

        file.lock()
            .unwrap()
            .save_login(&url("https://a"), &pair("a", "r"))
            .unwrap();
        assert_eq!(file.read(&url("https://a")), Some(pair("a", "r")));
        assert_eq!(file.damage(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        file.lock()
            .unwrap()
            .save_login(&url("https://a"), &pair("a", "r"))
            .unwrap();
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn only_missing_lock_support_degrades_to_unlocked_access() {
        use std::io::{Error, ErrorKind};
        #[cfg(unix)]
        assert!(locking_unsupported(&Error::from_raw_os_error(libc::ENOLCK)));
        assert!(locking_unsupported(&Error::from(ErrorKind::Unsupported)));
        assert!(!locking_unsupported(&Error::from(
            ErrorKind::PermissionDenied
        )));
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_world_readable_temp_file_never_becomes_the_credentials_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        let tmp = dir.path().join("credentials.json.tmp");
        std::fs::write(&tmp, "stale").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();

        file.lock()
            .unwrap()
            .save_login(&url("https://a"), &pair("a", "r"))
            .unwrap();
        let mode = std::fs::metadata(file.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
