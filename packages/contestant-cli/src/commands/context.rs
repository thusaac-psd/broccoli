use anyhow::{Context, Result, bail};
use broccoli_cli_core::api::plugin::LanguageRegistryItem;
use broccoli_cli_core::client::{Client, ClientError};
use serde::{Deserialize, Serialize};
use std::path::Path;

const CONTEXT_FILE: &str = ".broccoli";

#[derive(Serialize, Deserialize, Default)]
pub struct ProjectContext {
    pub contest: Option<String>,
    pub problem: Option<String>,
    pub language: Option<String>,
}

/// Walk up from cwd looking for a .broccoli file
pub fn discover_context() -> Option<ProjectContext> {
    let cwd = std::env::current_dir().ok()?;
    for dir in cwd.ancestors() {
        let path = dir.join(CONTEXT_FILE);
        if path.exists()
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(ctx) = toml::from_str(&content)
        {
            return Some(ctx);
        }
    }
    None
}

/// The contest a command uses: `arg`, else the `.broccoli` file's, else
/// `contest` in config.toml.
pub fn contest_ref(arg: Option<&str>) -> Result<String> {
    if let Some(contest) = arg {
        return Ok(contest.to_string());
    }
    discover_context()
        .and_then(|c| c.contest)
        .or_else(|| broccoli_cli_core::config::load_user_config().contest)
        .context(
            "No contest given. Pass one (id or name), run from a directory with a .broccoli \
             file, or set one with `broccoli config set contest <id>`.",
        )
}

/// Write .broccoli file in cwd
pub fn save_context(ctx: &ProjectContext) -> anyhow::Result<()> {
    let content = toml::to_string_pretty(ctx)?;
    std::fs::write(CONTEXT_FILE, content)?;
    Ok(())
}

/// A language to judge as, and the extensions the server registered for it
/// (none when it was given explicitly rather than detected).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Language {
    pub id: String,
    pub extensions: Vec<String>,
}

impl Language {
    /// Whether `filename` has one of this language's extensions.
    pub fn claims(&self, filename: &str) -> bool {
        Path::new(filename)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| {
                self.extensions
                    .iter()
                    .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
            })
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.id)
    }
}

/// The server's languages. Each fetch is cached under the config directory,
/// so `test --local` still detects languages while the server is unreachable.
pub fn server_languages(client: &Client) -> Result<Vec<LanguageRegistryItem>> {
    let cache = broccoli_cli_core::config::config_dir()
        .join("cache")
        .join("languages.json");
    match client.languages() {
        Ok(languages) => {
            if let Some(dir) = cache.parent()
                && std::fs::create_dir_all(dir).is_ok()
                && let Ok(json) = serde_json::to_vec(&languages)
            {
                let _ = std::fs::write(&cache, json);
            }
            Ok(languages)
        }
        Err(e @ ClientError::Network { .. }) => std::fs::read(&cache)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or_else(|| anyhow::Error::new(e)),
        Err(e) => Err(e.into()),
    }
}

/// The language to judge `files` as: `explicit` when given (the server
/// checks it and names the languages it supports when it is not one), else
/// the one language whose registered extensions cover the files. The
/// registry is the server's, fetched by `languages` only when detection
/// needs it.
pub fn resolve_language(
    files: &[String],
    explicit: Option<&str>,
    languages: impl FnOnce() -> Result<Vec<LanguageRegistryItem>>,
) -> Result<Language> {
    if let Some(language) = explicit {
        return Ok(Language {
            id: language.to_string(),
            extensions: Vec::new(),
        });
    }
    let languages = languages().context("Failed to fetch the server's languages")?;

    let mut detected: Option<&LanguageRegistryItem> = None;
    for file in files {
        let Some(ext) = Path::new(file).extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let claiming: Vec<_> = languages
            .iter()
            .filter(|l| {
                l.extensions
                    .iter()
                    .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(ext))
            })
            .collect();
        let language = match claiming.as_slice() {
            [] => continue,
            [one] => *one,
            many => bail!(
                "Several languages use .{ext} ({}); specify --language",
                many.iter()
                    .map(|l| l.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        if detected.is_some_and(|previous| previous.id != language.id) {
            bail!("Source files have different languages; specify --language explicitly");
        }
        detected = Some(language);
    }

    detected
        .map(|l| Language {
            id: l.id.clone(),
            extensions: l.extensions.clone(),
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Cannot detect a language from the file extensions; specify --language. \
             This server supports: {}",
                supported(&languages)
            )
        })
}

/// `id (.ext, .ext)` for each language, for error messages.
fn supported(languages: &[LanguageRegistryItem]) -> String {
    if languages.is_empty() {
        return "no languages (no language plugin is loaded)".into();
    }
    languages
        .iter()
        .map(|l| {
            let exts: Vec<String> = l
                .extensions
                .iter()
                .map(|e| format!(".{}", e.trim_start_matches('.')))
                .collect();
            format!("{} ({})", l.id, exts.join(", "))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn lang(id: &str, extensions: &[&str]) -> LanguageRegistryItem {
        LanguageRegistryItem {
            id: id.into(),
            name: id.into(),
            default_filename: String::new(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
            template: String::new(),
        }
    }

    fn registry() -> Result<Vec<LanguageRegistryItem>> {
        Ok(vec![
            lang("cpp", &["cpp", "cc"]),
            lang("python3", &["py"]),
            lang("pascal", &[".pas"]),
        ])
    }

    #[test]
    fn detects_the_language_the_server_registered_for_the_extension() {
        assert_eq!(
            resolve_language(&files(&["notes.txt", "main.py"]), None, registry)
                .unwrap()
                .id,
            "python3"
        );
        assert_eq!(
            resolve_language(&files(&["Main.PAS"]), None, registry)
                .unwrap()
                .id,
            "pascal"
        );
    }

    #[test]
    fn rejects_ambiguous_languages() {
        assert!(
            resolve_language(&files(&["main.py", "main.cpp"]), None, registry)
                .unwrap_err()
                .to_string()
                .contains("different languages")
        );
    }

    #[test]
    fn an_unknown_extension_lists_the_servers_languages() {
        let err = resolve_language(&files(&["main.rs"]), None, registry)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("cpp (.cpp, .cc), python3 (.py), pascal (.pas)"),
            "{err}"
        );
    }

    #[test]
    fn an_explicit_language_needs_no_registry() {
        assert_eq!(
            resolve_language(&files(&["main.py", "main.cpp"]), Some("rust"), || {
                unreachable!("not fetched")
            })
            .unwrap()
            .id,
            "rust"
        );
    }
}
