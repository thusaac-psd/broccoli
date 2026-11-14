use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::credentials::CredentialsFile;
use crate::server::ServerUrl;
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

/// Where the server a command talks to came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerSource {
    /// `--server` or `$BROCCOLI_URL`.
    Explicit,
    /// The server `broccoli login` signed in to last.
    Active,
    /// `server` in `config.toml` (the user's, else `/etc/broccoli`).
    Config,
}

/// The server a command uses, in one order every command shares:
/// `--server`/`$BROCCOLI_URL`, then the server the last `broccoli login`
/// signed in to, then `server` in `config.toml`. So `broccoli login -s X`
/// followed by any command uses X, and the configured server is the default
/// for a machine nobody has logged in on yet.
pub fn select_server(
    explicit: Option<&ServerUrl>,
    env_lookup: impl Fn(&str) -> Option<String>,
    config_server: Option<String>,
    store: &CredentialsFile,
) -> anyhow::Result<Option<(ServerUrl, ServerSource)>> {
    if let Some(server) = explicit {
        return Ok(Some((server.clone(), ServerSource::Explicit)));
    }
    if let Some(env) = env_lookup("BROCCOLI_URL") {
        let server = env
            .parse()
            .with_context(|| format!("$BROCCOLI_URL is not a server address: '{env}'"))?;
        return Ok(Some((server, ServerSource::Explicit)));
    }
    if let Some((server, _)) = store.active() {
        return Ok(Some((server, ServerSource::Active)));
    }
    match config_server {
        Some(value) => {
            let server = value.parse().with_context(|| {
                format!("`server` in config.toml is not a server address: '{value}'")
            })?;
            Ok(Some((server, ServerSource::Config)))
        }
        None => Ok(None),
    }
}

/// The server `broccoli login` signs in to: [`select_server`]'s choice, else
/// a server on this machine (`http://localhost:3000`).
pub fn login_server(explicit: Option<&ServerUrl>) -> anyhow::Result<ServerUrl> {
    let selected = select_server(
        explicit,
        |name| std::env::var(name).ok(),
        load_user_config().server,
        &CredentialsFile::default_location(),
    )?;
    Ok(match selected {
        Some((server, _)) => server,
        None => "http://localhost:3000".parse().expect("a valid address"),
    })
}

/// Resolve the session to use: a token from `--token` or `BROCCOLI_TOKEN` is
/// used as-is (never refreshed); otherwise the pair saved by `broccoli login`
/// for the server [`select_server`] picks.
pub fn resolve_credentials(
    server: Option<&ServerUrl>,
    token: Option<&str>,
) -> anyhow::Result<Session> {
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
    server: Option<&ServerUrl>,
    token: Option<&str>,
    env_lookup: impl Fn(&str) -> Option<String>,
    config_server: Option<String>,
    store: &CredentialsFile,
) -> anyhow::Result<Session> {
    let token = token
        .map(String::from)
        .or_else(|| env_lookup("BROCCOLI_TOKEN"));
    let Some((server, _)) = select_server(server, &env_lookup, config_server, store)? else {
        bail!(
            "No server to talk to.\n\
             Run `broccoli login --server <URL>` to sign in, or pass --server and --token."
        );
    };
    // An explicit token never reads the saved pairs, so a damaged file cannot
    // lock out `--server`/`--token`.
    if let Some(token) = token {
        return Ok(Session::explicit(server, token));
    }
    if let Some(damage) = store.damage() {
        eprintln!(
            "Warning: ignoring saved credentials: {damage}. Run `broccoli login` to replace them."
        );
    }
    match store.read(&server) {
        Some(tokens) => {
            store.check_locking();
            Ok(Session::stored(server, tokens, store.clone()))
        }
        None => bail!(
            "No credentials found for {server}.\n\
             Run `broccoli login --server {server}` to sign in, or pass --server and --token."
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

/// A single-valued setting of `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKey {
    Contest,
    Language,
    Server,
    Tls,
}

impl ConfigKey {
    pub const ALL: [ConfigKey; 4] = [Self::Contest, Self::Language, Self::Server, Self::Tls];

    pub fn name(self) -> &'static str {
        match self {
            Self::Contest => "contest",
            Self::Language => "language",
            Self::Server => "server",
            Self::Tls => "tls",
        }
    }
}

impl std::str::FromStr for ConfigKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        Self::ALL
            .into_iter()
            .find(|key| key.name() == s)
            .ok_or_else(|| {
                let names: Vec<_> = Self::ALL.iter().map(|k| k.name()).collect();
                anyhow::anyhow!("Unknown config key '{s}'. Valid keys: {}", names.join(", "))
            })
    }
}

impl std::fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl UserConfig {
    pub fn get(&self, key: ConfigKey) -> Option<&str> {
        self.value(key).as_deref()
    }

    fn value(&self, key: ConfigKey) -> &Option<String> {
        match key {
            ConfigKey::Contest => &self.contest,
            ConfigKey::Language => &self.language,
            ConfigKey::Server => &self.server,
            ConfigKey::Tls => &self.tls,
        }
    }

    fn value_mut(&mut self, key: ConfigKey) -> &mut Option<String> {
        match key {
            ConfigKey::Contest => &mut self.contest,
            ConfigKey::Language => &mut self.language,
            ConfigKey::Server => &mut self.server,
            ConfigKey::Tls => &mut self.tls,
        }
    }

    /// `self` with every value `over` sets taking precedence.
    fn overlaid_with(mut self, over: UserConfig) -> UserConfig {
        for key in ConfigKey::ALL {
            if let Some(value) = over.value(key).clone() {
                *self.value_mut(key) = Some(value);
            }
        }
        // Per language, so overriding one runtime keeps the others.
        self.runtimes.extend(over.runtimes);
        self
    }
}

/// The two `config.toml` files: the machine's (`/etc/broccoli`, preseeded by
/// whoever set up the lab) and the user's, whose values take precedence.
/// Commands that change the configuration change the user's file only, so a
/// machine value is never copied into it.
pub struct ConfigLayers {
    system_path: PathBuf,
    user_path: PathBuf,
    pub system: UserConfig,
    pub user: UserConfig,
}

impl ConfigLayers {
    pub fn load() -> Self {
        Self::load_from(system_config_path(), config_path())
    }

    fn load_from(system_path: PathBuf, user_path: PathBuf) -> Self {
        Self {
            system: load_toml(&system_path),
            user: load_toml(&user_path),
            system_path,
            user_path,
        }
    }

    pub fn user_path(&self) -> &Path {
        &self.user_path
    }

    pub fn system_path(&self) -> &Path {
        &self.system_path
    }

    /// What commands use: the user's values over the machine's.
    pub fn merged(&self) -> UserConfig {
        self.system.clone().overlaid_with(self.user.clone())
    }

    /// Set `key` in the user's file.
    pub fn set(&mut self, key: ConfigKey, value: String) -> anyhow::Result<()> {
        *self.user.value_mut(key) = Some(value);
        self.save_user()
    }

    /// Remove `key` from the user's file. Returns the machine's value, which
    /// applies from now on, if it sets one.
    pub fn unset(&mut self, key: ConfigKey) -> anyhow::Result<Option<&str>> {
        *self.user.value_mut(key) = None;
        self.save_user()?;
        Ok(self.system.get(key))
    }

    fn save_user(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.user_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create config directory")?;
        }
        let content = toml::to_string_pretty(&self.user)?;
        std::fs::write(&self.user_path, content)
            .with_context(|| format!("Failed to write {}", self.user_path.display()))
    }
}

fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// The configuration commands use: `/etc/broccoli/config.toml` with the
/// user's `config.toml` taking precedence.
pub fn load_user_config() -> UserConfig {
    ConfigLayers::load().merged()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::StoredTokens;
    use std::fs;

    fn layers(system: &str, user: Option<&str>) -> (tempfile::TempDir, ConfigLayers) {
        let dir = tempfile::tempdir().unwrap();
        let system_path = dir.path().join("etc/config.toml");
        fs::create_dir_all(system_path.parent().unwrap()).unwrap();
        fs::write(&system_path, system).unwrap();
        let user_path = dir.path().join("home/config.toml");
        if let Some(user) = user {
            fs::create_dir_all(user_path.parent().unwrap()).unwrap();
            fs::write(&user_path, user).unwrap();
        }
        let layers = ConfigLayers::load_from(system_path, user_path);
        (dir, layers)
    }

    #[test]
    fn setting_a_key_never_copies_machine_values_into_the_user_file() {
        let (_dir, mut layers) = layers("server = \"http://judge.lab\"\ntls = \"system\"\n", None);
        layers.set(ConfigKey::Language, "cpp".into()).unwrap();

        let saved: UserConfig =
            toml::from_str(&fs::read_to_string(layers.user_path()).unwrap()).unwrap();
        assert_eq!(saved.language.as_deref(), Some("cpp"));
        assert_eq!(saved.server, None);
        assert_eq!(saved.tls, None);
        assert_eq!(
            layers.merged().get(ConfigKey::Server),
            Some("http://judge.lab")
        );
    }

    #[test]
    fn unsetting_a_key_removes_the_users_value_and_reports_the_machines() {
        let (_dir, mut layers) = layers(
            "server = \"http://judge.lab\"\n",
            Some("server = \"http://mine\"\ncontest = \"3\"\n"),
        );
        assert_eq!(layers.merged().get(ConfigKey::Server), Some("http://mine"));

        assert_eq!(
            layers.unset(ConfigKey::Server).unwrap(),
            Some("http://judge.lab")
        );
        assert_eq!(layers.unset(ConfigKey::Contest).unwrap(), None);
        let reloaded = ConfigLayers::load_from(
            layers.system_path().to_path_buf(),
            layers.user_path().to_path_buf(),
        );
        assert_eq!(reloaded.user.server, None);
        assert_eq!(reloaded.user.contest, None);
        assert_eq!(
            reloaded.merged().get(ConfigKey::Server),
            Some("http://judge.lab")
        );
    }

    #[test]
    fn config_keys_parse_from_their_names() {
        for key in ConfigKey::ALL {
            assert_eq!(key.name().parse::<ConfigKey>().unwrap(), key);
        }
        assert!("colour".parse::<ConfigKey>().is_err());
    }

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
            .save_login(
                &server.parse().unwrap(),
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
        let server = server.map(|s| s.parse::<ServerUrl>().unwrap());
        resolve_credentials_from_sources(
            server.as_ref(),
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
        assert_eq!(session.server().as_str(), "https://env.example.com");
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
        assert_eq!(s.server().as_str(), "https://config.example.com");
        let s = resolve(None, None, &env, config, &store).unwrap();
        assert_eq!(s.server().as_str(), "https://env.example.com");
        let s = resolve(Some("https://arg.example.com"), None, &env, config, &store).unwrap();
        assert_eq!(s.server().as_str(), "https://arg.example.com");
    }

    #[test]
    fn the_server_logged_in_to_last_wins_over_the_configured_one() {
        let (_dir, store) = saved_store("http://127.0.0.1:3100");
        let session = resolve(None, None, &[], Some("https://preseeded.lab"), &store).unwrap();
        assert_eq!(session.server().as_str(), "http://127.0.0.1:3100");
        assert_eq!(session.bearer().unwrap(), "saved");
    }

    #[test]
    fn a_missing_login_names_the_server_it_looked_for() {
        let (_dir, store) = empty_store();
        let Err(err) = resolve(None, None, &[], Some("https://preseeded.lab/"), &store) else {
            panic!("nothing is saved for the configured server");
        };
        assert!(
            err.to_string().contains("for https://preseeded.lab."),
            "{err}"
        );
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
        assert_eq!(session.server().as_str(), "https://judge");
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
