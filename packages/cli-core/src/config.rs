use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::credentials::CredentialsFile;
use crate::session::Session;

/// Broccoli config directory (`~/.config/broccoli`); home-based, not cwd, to avoid writing tokens into a shared dir.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("broccoli")
}

/// Cache dir for a downloaded problem (`~/.config/broccoli/cache/<contest>/<problem>`).
pub fn problem_cache_dir(contest_id: &str, problem_id: &str) -> PathBuf {
    config_dir().join("cache").join(contest_id).join(problem_id)
}

/// Resolve the session to use: a token from `--token` or `BROCCOLI_TOKEN` is
/// used as-is (never refreshed); otherwise the pair saved by `broccoli login`.
/// The server comes from the flag, then `BROCCOLI_URL`, then `config.toml`,
/// then the most recently saved server.
pub fn resolve_credentials(server: Option<&str>, token: Option<&str>) -> anyhow::Result<Session> {
    resolve_credentials_from_sources(
        server,
        token,
        |name| std::env::var(name).ok(),
        load_user_config().server,
        &CredentialsFile::default_location(),
    )
}

/// [`resolve_credentials`] with injectable sources, for testing.
fn resolve_credentials_from_sources(
    server: Option<&str>,
    token: Option<&str>,
    env_lookup: impl Fn(&str) -> Option<String>,
    config_server: Option<String>,
    store: &CredentialsFile,
) -> anyhow::Result<Session> {
    let server = server
        .map(String::from)
        .or_else(|| env_lookup("BROCCOLI_URL"))
        .or(config_server);
    let token = token
        .map(String::from)
        .or_else(|| env_lookup("BROCCOLI_TOKEN"));
    // Explicit credentials never touch the credentials file, so a damaged
    // file cannot lock out `--server`/`--token`.
    if let (Some(server), Some(token)) = (&server, &token) {
        return Ok(Session::explicit(server.clone(), token.clone()));
    }

    if let Some(damage) = store.damage() {
        eprintln!(
            "Warning: ignoring saved credentials: {damage}. Run `broccoli login` to replace them."
        );
    }
    let saved = match &server {
        Some(server) => store.read(server).map(|tokens| (server.clone(), tokens)),
        None => store.most_recent(),
    };
    match (token, saved) {
        (Some(token), Some((server, _))) => Ok(Session::explicit(server, token)),
        (None, Some((server, tokens))) => Ok(Session::stored(server, tokens, store.clone())),
        _ => bail!(
            "No credentials found.\n\
             Run `broccoli login` to authenticate, or pass --server and --token."
        ),
    }
}

/// Per-user configuration stored in `~/.config/broccoli/config.toml`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub contest: Option<String>,

    #[serde(default)]
    pub language: Option<String>,

    /// Default server URL used when neither a command flag nor $BROCCOLI_URL is
    /// set. Flags and the environment variable take precedence over this value.
    #[serde(default)]
    pub server: Option<String>,

    /// `default` (OS trust store plus bundled Mozilla roots), `webpki` (bundled roots only),
    /// `system` (the OS verifier only), or `insecure`; overridden by `$BROCCOLI_TLS`.
    #[serde(default)]
    pub tls: Option<String>,

    /// Per-language runtimes (e.g. `{"python": "python3.12"}`).
    #[serde(default)]
    pub runtimes: HashMap<String, String>,
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Load config, merging `/etc/broccoli/config.toml` with user overrides taking precedence.
pub fn load_user_config() -> UserConfig {
    let mut cfg = load_toml(&system_config_path());

    let user = load_toml(&config_path());
    if user.contest.is_some() {
        cfg.contest = user.contest;
    }
    if user.language.is_some() {
        cfg.language = user.language;
    }
    if user.server.is_some() {
        cfg.server = user.server;
    }
    if user.tls.is_some() {
        cfg.tls = user.tls;
    }
    // merge per-key so overriding one language keeps the system's other runtimes
    for (lang, cmd) in user.runtimes {
        cfg.runtimes.insert(lang, cmd);
    }
    cfg
}

fn system_config_path() -> PathBuf {
    PathBuf::from("/etc/broccoli/config.toml")
}

fn load_toml(path: &Path) -> UserConfig {
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(config) => config,
            Err(e) => {
                eprintln!(
                    "Warning: failed to parse {}: {}. Using defaults.",
                    path.display(),
                    e
                );
                UserConfig::default()
            }
        },
        Err(_) => UserConfig::default(),
    }
}

/// Save user configuration to `~/.config/broccoli/config.toml`.
pub fn save_user_config(config: &UserConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create config directory")?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, &content).context("Failed to write config file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::StoredTokens;
    use std::fs;

    #[test]
    fn test_config_dir() {
        let dir = config_dir();
        assert!(dir.ends_with("broccoli"));
    }

    #[test]
    fn test_user_config_default() {
        let config = UserConfig::default();
        assert!(config.contest.is_none());
        assert!(config.language.is_none());
        assert!(config.server.is_none());
        assert!(config.runtimes.is_empty());
    }

    #[test]
    fn test_user_config_roundtrip() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;

        let path = dir.path().join("config.toml");
        let config = UserConfig {
            contest: Some("test-contest".into()),
            language: Some("rust".into()),
            server: Some("https://example.com".into()),
            tls: Some("system".into()),
            runtimes: [("rust".into(), "1.85".into())].into(),
        };

        let content = toml::to_string_pretty(&config)?;
        fs::write(&path, &content)?;

        let loaded: UserConfig = toml::from_str(&fs::read_to_string(&path)?)?;
        assert_eq!(loaded.contest, Some("test-contest".into()));
        assert_eq!(loaded.language, Some("rust".into()));
        assert_eq!(loaded.server, Some("https://example.com".into()));
        assert_eq!(loaded.tls, Some("system".into()));
        assert_eq!(loaded.runtimes.get("rust"), Some(&"1.85".into()));

        Ok(())
    }

    fn empty_store() -> (tempfile::TempDir, CredentialsFile) {
        let dir = tempfile::tempdir().unwrap();
        let file = CredentialsFile::at(dir.path().join("credentials.json"));
        (dir, file)
    }

    fn saved_store(server: &str) -> (tempfile::TempDir, CredentialsFile) {
        let (dir, file) = empty_store();
        file.lock()
            .unwrap()
            .write(
                server,
                &StoredTokens {
                    token: "saved".into(),
                    refresh_token: "saved-refresh".into(),
                },
            )
            .unwrap();
        (dir, file)
    }

    fn resolve(
        server: Option<&str>,
        token: Option<&str>,
        env: &[(&str, &str)],
        config_server: Option<&str>,
        store: &CredentialsFile,
    ) -> anyhow::Result<Session> {
        let env: HashMap<&str, &str> = env.iter().copied().collect();
        resolve_credentials_from_sources(
            server,
            token,
            |name| env.get(name).map(|v| v.to_string()),
            config_server.map(String::from),
            store,
        )
    }

    #[test]
    fn env_token_and_server_are_used_as_given() {
        let (_dir, store) = empty_store();
        let env = [
            ("BROCCOLI_URL", "https://env.example.com"),
            ("BROCCOLI_TOKEN", "env-token"),
        ];
        let session = resolve(None, None, &env, None, &store).unwrap();
        assert_eq!(session.server(), "https://env.example.com");
        assert_eq!(session.bearer().unwrap(), "env-token");
    }

    #[test]
    fn server_precedence_is_flag_then_env_then_config() {
        let (_dir, store) = empty_store();
        let env = [
            ("BROCCOLI_URL", "https://env.example.com"),
            ("BROCCOLI_TOKEN", "t"),
        ];
        let config = Some("https://config.example.com");
        let s = resolve(None, None, &env[1..], config, &store).unwrap();
        assert_eq!(s.server(), "https://config.example.com");
        let s = resolve(None, None, &env, config, &store).unwrap();
        assert_eq!(s.server(), "https://env.example.com");
        let s = resolve(Some("https://arg.example.com"), None, &env, config, &store).unwrap();
        assert_eq!(s.server(), "https://arg.example.com");
    }

    #[test]
    fn an_explicit_token_never_borrows_the_saved_refresh_token() {
        let (_dir, store) = saved_store("https://judge");
        let session =
            resolve(Some("https://judge"), Some("other-user"), &[], None, &store).unwrap();
        assert_eq!(session.bearer().unwrap(), "other-user");
        // Without its own refresh token the explicit session cannot be
        // refreshed into the saved user's session.
        assert!(session.refresh("other-user", |_| unreachable!()).is_err());
    }

    #[test]
    fn the_saved_pair_is_used_without_an_explicit_token() {
        let (_dir, store) = saved_store("https://judge");
        let session = resolve(None, None, &[], None, &store).unwrap();
        assert_eq!(session.server(), "https://judge");
        assert_eq!(session.bearer().unwrap(), "saved");
    }

    #[test]
    fn a_damaged_file_neither_blocks_explicit_credentials_nor_resolves_a_session() {
        let (dir, store) = empty_store();
        std::fs::write(dir.path().join("credentials.json"), "{garbage").unwrap();
        let session = resolve(Some("https://judge"), Some("tok"), &[], None, &store).unwrap();
        assert_eq!(session.bearer().unwrap(), "tok");
        let env = [("BROCCOLI_URL", "https://judge"), ("BROCCOLI_TOKEN", "env")];
        assert!(resolve(None, None, &env, None, &store).is_ok());
        assert!(resolve(None, None, &[], None, &store).is_err());
    }

    #[test]
    fn nothing_to_resolve_is_an_error() {
        let (_dir, store) = empty_store();
        assert!(resolve(None, None, &[], None, &store).is_err());
    }
}
