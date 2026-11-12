//! `credentials.json`: the access/refresh token pair saved per server.
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
    pub fn read(&self, server: &str) -> Option<StoredTokens> {
        self.load()
            .contents
            .entries
            .into_iter()
            .find(|e| e.server == server)
            .map(|e| e.tokens)
    }

    /// The most recently saved server and its pair.
    pub fn most_recent(&self) -> Option<(String, StoredTokens)> {
        self.load()
            .contents
            .entries
            .into_iter()
            .next()
            .map(|e| (e.server, e.tokens))
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
        let entries: Vec<Entry> = raw
            .entries
            .into_iter()
            .filter_map(|value| serde_json::from_value(value).ok())
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
    pub fn read(&self, server: &str) -> Option<StoredTokens> {
        self.file.read(server)
    }

    /// Save `tokens` for `server` and make it the most recent server. Other
    /// servers' valid entries are kept; anything unreadable is replaced.
    pub fn write(&self, server: &str, tokens: &StoredTokens) -> anyhow::Result<()> {
        let mut contents = self.file.load().contents;
        contents.entries.retain(|e| e.server != server);
        contents.entries.insert(
            0,
            Entry {
                server: server.to_string(),
                tokens: tokens.clone(),
            },
        );
        let text = serde_json::to_string_pretty(&contents)?;

        let tmp = self.file.path.with_extension("json.tmp");
        write_private(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, &self.file.path).context("Failed to replace credentials file")
    }
}

/// Create `path` readable by the owner only from the start, so the tokens are
/// never briefly world-readable.
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
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

    #[test]
    fn a_write_replaces_one_server_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        let lock = file.lock().unwrap();
        lock.write("https://a", &pair("a1", "ra1")).unwrap();
        lock.write("https://b", &pair("b1", "rb1")).unwrap();
        lock.write("https://a", &pair("a2", "ra2")).unwrap();
        drop(lock);

        assert_eq!(file.read("https://a"), Some(pair("a2", "ra2")));
        assert_eq!(file.read("https://b"), Some(pair("b1", "rb1")));
        let (server, _) = file.most_recent().unwrap();
        assert_eq!(server, "https://a");
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
        assert_eq!(file.read("https://x"), None);
        assert_eq!(file.read("https://y"), Some(pair("t", "r")));
        assert!(file.damage().unwrap().contains("1 entry"));
    }

    #[test]
    fn a_login_replaces_a_file_that_does_not_parse() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, "not json at all").unwrap();
        let file = CredentialsFile::at(&path);
        assert_eq!(file.most_recent(), None);
        assert!(file.damage().is_some());

        file.lock()
            .unwrap()
            .write("https://a", &pair("a", "r"))
            .unwrap();
        assert_eq!(file.read("https://a"), Some(pair("a", "r")));
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
            .write("https://a", &pair("a", "r"))
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
}
