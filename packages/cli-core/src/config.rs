use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

/// Server URL, access token, and optional long-lived refresh token.
pub struct Credentials {
    pub server: String,
    pub token: String,
    pub refresh_token: Option<String>,
}

#[derive(Serialize, Deserialize, Default)]
struct CredentialsFile {
    entries: Vec<CredentialEntry>,
}

#[derive(Serialize, Deserialize)]
struct CredentialEntry {
    server: String,
    token: String,
    /// Absent in older credential files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

/// Broccoli config directory (`~/.config/broccoli`); home-based, not cwd, to avoid writing tokens into a shared dir.
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".config")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("broccoli")
}

fn credentials_path() -> PathBuf {
    config_dir().join("credentials.json")
}

/// Cache dir for a downloaded problem (`~/.config/broccoli/cache/<contest>/<problem>`).
pub fn problem_cache_dir(contest_id: &str, problem_id: &str) -> PathBuf {
    config_dir().join("cache").join(contest_id).join(problem_id)
}

/// Resolve credentials: CLI args, then `BROCCOLI_URL`/`BROCCOLI_TOKEN`, then the
/// `server` key from `config.toml`, then `credentials.json`.
pub fn resolve_credentials(
    server: Option<&str>,
    token: Option<&str>,
) -> anyhow::Result<Credentials> {
    resolve_credentials_from_sources(
        server,
        token,
        |name| std::env::var(name).ok(),
        load_user_config().server,
    )
}

/// [`resolve_credentials`] with injectable env lookup and config-file server, for testing.
fn resolve_credentials_from_sources(
    server: Option<&str>,
    token: Option<&str>,
    env_lookup: impl Fn(&str) -> Option<String>,
    config_server: Option<String>,
) -> anyhow::Result<Credentials> {
    if let (Some(s), Some(t)) = (server, token) {
        return Ok(Credentials {
            server: s.to_string(),
            token: t.to_string(),
            refresh_token: None,
        });
    }

    let env_server = env_lookup("BROCCOLI_URL");
    let env_token = env_lookup("BROCCOLI_TOKEN");

    // Precedence: CLI flag > env var > config.toml `server` > credentials.json.
    let resolved_server = server.map(String::from).or(env_server).or(config_server);
    let resolved_token = token.map(String::from).or(env_token);

    if let (Some(s), Some(t)) = (resolved_server.as_ref(), resolved_token.as_ref()) {
        return Ok(Credentials {
            server: s.clone(),
            token: t.clone(),
            refresh_token: None,
        });
    }

    let creds_path = credentials_path();
    if creds_path.exists() {
        let content =
            std::fs::read_to_string(&creds_path).context("Failed to read credentials file")?;
        let file: CredentialsFile =
            serde_json::from_str(&content).context("Failed to parse credentials file")?;

        if let Some(target_server) = &resolved_server {
            if let Some(entry) = file.entries.iter().find(|e| &e.server == target_server) {
                return Ok(Credentials {
                    server: entry.server.clone(),
                    token: resolved_token.unwrap_or_else(|| entry.token.clone()),
                    refresh_token: entry.refresh_token.clone(),
                });
            }
        } else if let Some(entry) = file.entries.first() {
            return Ok(Credentials {
                server: entry.server.clone(),
                token: resolved_token.unwrap_or_else(|| entry.token.clone()),
                refresh_token: entry.refresh_token.clone(),
            });
        }
    }

    bail!(
        "No credentials found.\n\
         Run `broccoli login` to authenticate, or pass --server and --token."
    );
}

/// Save access token only (no refresh token) for a server.
pub fn save_credentials(server: &str, token: &str) -> anyhow::Result<()> {
    save_credentials_full(server, token, None)
}

/// Save credentials to `credentials.json` (0o600 on Unix), with optional refresh token.
pub fn save_credentials_full(
    server: &str,
    token: &str,
    refresh_token: Option<&str>,
) -> anyhow::Result<()> {
    let creds_path = credentials_path();

    let mut file = if creds_path.exists() {
        // Propagate read/parse errors instead of unwrap_or_default(): a corrupt or
        // unreadable file previously collapsed to an empty CredentialsFile, so the
        // write below would silently drop every OTHER server's saved token. Fail
        // loudly (matching the read path) rather than destroying credentials.
        let content =
            std::fs::read_to_string(&creds_path).context("Failed to read credentials file")?;
        serde_json::from_str::<CredentialsFile>(&content)
            .context("Failed to parse credentials file")?
    } else {
        CredentialsFile::default()
    };

    // keep existing refresh token when caller supplies none
    let refresh_token = refresh_token.map(String::from).or_else(|| {
        file.entries
            .iter()
            .find(|e| e.server == server)
            .and_then(|e| e.refresh_token.clone())
    });

    // insert at front so most-recently-used server is the default
    file.entries.retain(|e| e.server != server);
    file.entries.insert(
        0,
        CredentialEntry {
            server: server.to_string(),
            token: token.to_string(),
            refresh_token,
        },
    );

    if let Some(parent) = creds_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create config directory")?;
    }

    let content = serde_json::to_string_pretty(&file)?;

    // Create the file 0600 from the start so the bearer/refresh tokens are never
    // even briefly world-readable. `std::fs::write` would create with the default
    // umask (typically 0644) and only tighten afterwards, leaving a window in
    // which another local user could read the credentials.
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&creds_path)
            .context("Failed to open credentials file")?;
        // Normalize perms too, in case the file predates this and is still 0644.
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(content.as_bytes())
            .context("Failed to write credentials file")?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&creds_path, &content).context("Failed to write credentials file")?;
    }

    Ok(())
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

    #[test]
    fn test_save_and_load_credentials() -> anyhow::Result<()> {
        let server = "https://contest.example.com";
        let token = "secret-token-123";

        let mut file = CredentialsFile::default();
        file.entries.push(CredentialEntry {
            server: server.to_string(),
            token: token.to_string(),
            refresh_token: Some("refresh-abc".to_string()),
        });

        let json = serde_json::to_string_pretty(&file)?;
        let restored: CredentialsFile = serde_json::from_str(&json)?;
        assert_eq!(restored.entries.len(), 1);
        assert_eq!(restored.entries[0].server, server);
        assert_eq!(restored.entries[0].token, token);
        assert_eq!(
            restored.entries[0].refresh_token.as_deref(),
            Some("refresh-abc")
        );

        Ok(())
    }

    #[test]
    fn test_credential_entry_back_compat_without_refresh() {
        let json = r#"{"entries":[{"server":"https://x","token":"t"}]}"#;
        let file: CredentialsFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.entries[0].refresh_token, None);
    }

    #[test]
    fn test_resolve_from_env() {
        let env: HashMap<&str, &str> = [
            ("BROCCOLI_URL", "https://env.example.com"),
            ("BROCCOLI_TOKEN", "env-token"),
        ]
        .into();

        let creds = resolve_credentials_from_sources(
            None,
            None,
            |name| env.get(name).copied().map(String::from),
            None,
        )
        .unwrap();
        assert_eq!(creds.server, "https://env.example.com");
        assert_eq!(creds.token, "env-token");
    }

    #[test]
    fn test_config_server_used_when_no_flag_or_env_server() {
        let env: HashMap<&str, &str> = [("BROCCOLI_TOKEN", "env-token")].into();

        let creds = resolve_credentials_from_sources(
            None,
            None,
            |name| env.get(name).copied().map(String::from),
            Some("https://config.example.com".into()),
        )
        .unwrap();
        assert_eq!(creds.server, "https://config.example.com");
        assert_eq!(creds.token, "env-token");
    }

    #[test]
    fn test_env_server_beats_config_server() {
        let env: HashMap<&str, &str> = [
            ("BROCCOLI_URL", "https://env.example.com"),
            ("BROCCOLI_TOKEN", "env-token"),
        ]
        .into();

        let creds = resolve_credentials_from_sources(
            None,
            None,
            |name| env.get(name).copied().map(String::from),
            Some("https://config.example.com".into()),
        )
        .unwrap();
        assert_eq!(creds.server, "https://env.example.com");
    }

    #[test]
    fn test_flag_server_beats_config_server() {
        let creds = resolve_credentials_from_sources(
            Some("https://arg.example.com"),
            Some("arg-token"),
            |_| None,
            Some("https://config.example.com".into()),
        )
        .unwrap();
        assert_eq!(creds.server, "https://arg.example.com");
        assert_eq!(creds.token, "arg-token");
    }

    #[test]
    fn test_resolve_from_args() {
        let creds =
            resolve_credentials(Some("https://arg.example.com"), Some("arg-token")).unwrap();
        assert_eq!(creds.server, "https://arg.example.com");
        assert_eq!(creds.token, "arg-token");
    }
}
