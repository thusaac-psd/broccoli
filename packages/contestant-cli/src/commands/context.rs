use anyhow::{Context, Result, bail};
use broccoli_cli_core::api::plugin::LanguageRegistryItem;
use broccoli_cli_core::api::visibility::{ClosedBy, ClosedLanguage};
use broccoli_cli_core::client::{Client, ClientError};
use broccoli_cli_core::ids::{ContestId, ProblemId};
use broccoli_cli_core::model::{PluginNames, closed_by_text};
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

/// What a plugin closes now of the problem a command works on: every
/// language (its `closed_by`: an evaluator, a checker, or the plugin of its
/// last open language) and single languages (its `closed_languages`), with
/// the names to call those plugins by. A language whose plugin is out is in
/// no registry, so this is what says which plugin a contestant waits for.
pub struct Closed<'a> {
    pub problem: Option<ClosedBy>,
    pub languages: Vec<ClosedLanguage>,
    pub names: PluginNames<'a>,
}

impl Default for Closed<'_> {
    fn default() -> Self {
        Self {
            problem: None,
            languages: Vec::new(),
            names: PluginNames::ids(),
        }
    }
}

impl<'a> Closed<'a> {
    /// What is closed of `problem_id`, read through its contest when there
    /// is one, with plugins named by the server's catalog. Read only when
    /// detection fails; a problem that cannot be read closes nothing.
    pub fn of(client: &'a Client, contest_id: Option<ContestId>, problem_id: ProblemId) -> Self {
        let problem = match contest_id {
            Some(cid) => client.get_contest_problem(cid, problem_id),
            None => client.get_problem(problem_id),
        };
        let (problem, languages) = match problem {
            Ok(p) => (p.closed_by, p.closed_languages),
            Err(_) => (None, Vec::new()),
        };
        Self {
            problem,
            languages,
            names: client.plugin_names(),
        }
    }

    /// Whether no language is open for the problem.
    fn all(&self) -> bool {
        self.problem.is_some()
    }

    /// One line per plugin that closes the problem or its languages: `closed
    /// by ...` for the problem's own, unless it is a language's, and `c,
    /// cpp: closed by ...` for languages.
    fn lines(&self) -> Vec<String> {
        let own = self
            .problem
            .as_ref()
            .filter(|by| !self.languages.iter().any(|l| l.closed_by == **by))
            .map(|by| format!("closed by {}", closed_by_text(by, &self.names)));
        let mut groups: Vec<(&ClosedBy, Vec<&str>)> = Vec::new();
        for closed in &self.languages {
            match groups.iter_mut().find(|(by, _)| **by == closed.closed_by) {
                Some((_, languages)) => languages.push(&closed.language),
                None => groups.push((&closed.closed_by, vec![&closed.language])),
            }
        }
        own.into_iter()
            .chain(groups.into_iter().map(|(by, languages)| {
                format!(
                    "{}: closed by {}",
                    languages.join(", "),
                    closed_by_text(by, &self.names)
                )
            }))
            .collect()
    }
}

/// The language to judge `files` as: `explicit` when given (the server
/// checks it and names the languages it supports when it is not one), else
/// the one language whose registered extensions cover the files. The
/// registry is the server's, fetched by `languages` only when detection
/// needs it; what is closed for the problem (`closed`) is read only when
/// detection fails, to say which plugin keeps a language out.
pub fn resolve_language<'a>(
    files: &[String],
    explicit: Option<&str>,
    languages: impl FnOnce() -> Result<Vec<LanguageRegistryItem>>,
    closed: impl FnOnce() -> Closed<'a>,
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
            let closed = closed();
            // With no language open, no --language would be taken.
            let mut message = if closed.all() || languages.is_empty() {
                "No language is open for this problem right now.".to_string()
            } else {
                format!(
                    "Cannot detect a language from the file extensions; specify --language. \
                     This server supports: {}.",
                    supported(&languages)
                )
            };
            for line in closed.lines() {
                message.push_str("\n  ");
                message.push_str(&line);
            }
            anyhow::anyhow!(message)
        })
}

/// `id (.ext, .ext)` for each language, for error messages.
fn supported(languages: &[LanguageRegistryItem]) -> String {
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

    /// A problem none of whose languages is closed.
    fn open() -> Closed<'static> {
        Closed::default()
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
            resolve_language(&files(&["notes.txt", "main.py"]), None, registry, open)
                .unwrap()
                .id,
            "python3"
        );
        assert_eq!(
            resolve_language(&files(&["Main.PAS"]), None, registry, open)
                .unwrap()
                .id,
            "pascal"
        );
    }

    #[test]
    fn rejects_ambiguous_languages() {
        assert!(
            resolve_language(&files(&["main.py", "main.cpp"]), None, registry, open)
                .unwrap_err()
                .to_string()
                .contains("different languages")
        );
    }

    #[test]
    fn an_unknown_extension_lists_the_servers_languages() {
        let err = resolve_language(&files(&["main.rs"]), None, registry, open)
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
            resolve_language(
                &files(&["main.py", "main.cpp"]),
                Some("rust"),
                || unreachable!("not fetched"),
                || unreachable!("not fetched")
            )
            .unwrap()
            .id,
            "rust"
        );
    }

    /// With a problem's language plugin out, the server registers no
    /// language for it: detection says which plugin closes which languages
    /// and why, never that no language plugin is loaded.
    #[test]
    fn a_closed_language_names_the_plugin_that_closes_it() {
        use broccoli_cli_core::api::visibility::{
            ClosedBy, ClosedLanguage, ClosedReason, Provided,
        };
        let closed_by = |plugin_id: &str, reason| ClosedBy {
            plugin_id: plugin_id.into(),
            provides: Provided::Language,
            reason,
        };
        let closed = || Closed {
            problem: None,
            languages: vec![
                ClosedLanguage {
                    language: "c".into(),
                    closed_by: closed_by("standard-languages", ClosedReason::Disabled),
                },
                ClosedLanguage {
                    language: "cpp".into(),
                    closed_by: closed_by("standard-languages", ClosedReason::Disabled),
                },
                ClosedLanguage {
                    language: "java".into(),
                    closed_by: closed_by("extra-languages", ClosedReason::NotLoaded),
                },
            ],
            names: broccoli_cli_core::model::PluginNames::ids(),
        };
        let err = resolve_language(
            &files(&["main.cpp"]),
            None,
            || Ok(vec![lang("python", &["py"])]),
            closed,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains(
                "c, cpp: closed by its language plugin standard-languages \
                 (disabled until an administrator enables it)"
            ),
            "{err}"
        );
        assert!(
            err.contains(
                "java: closed by its language plugin extra-languages \
                 (loaded on no server until one loads it)"
            ),
            "{err}"
        );
        assert!(!err.contains(": closed while"), "{err}");
        assert!(!err.contains("no language plugin is loaded"), "{err}");
    }

    /// A problem no language is open for (its evaluator out, or every
    /// language closed) says so and names why, never asking for a
    /// `--language` no answer would take.
    #[test]
    fn a_problem_no_language_is_open_for_names_why_without_asking_for_a_language() {
        use broccoli_cli_core::api::visibility::{
            ClosedBy, ClosedLanguage, ClosedReason, Provided,
        };
        let evaluator = ClosedBy {
            plugin_id: "batch-evaluator".into(),
            provides: Provided::ProblemType,
            reason: ClosedReason::Disabled,
        };
        let err = resolve_language(
            &files(&["main.cpp"]),
            None,
            || Ok(vec![lang("python", &["py"])]),
            || Closed {
                problem: Some(evaluator.clone()),
                languages: Vec::new(),
                names: PluginNames::ids(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.starts_with("No language is open for this problem right now"),
            "{err}"
        );
        assert!(!err.contains("--language"), "{err}");
        assert!(
            err.contains(
                "closed by its evaluator plugin batch-evaluator \
                 (disabled until an administrator enables it)"
            ),
            "{err}"
        );

        // Every language closed by its plugin: the problem's own closure is
        // that plugin, named once.
        let languages = ClosedBy {
            plugin_id: "standard-languages".into(),
            provides: Provided::Language,
            reason: ClosedReason::Disabled,
        };
        let err = resolve_language(
            &files(&["main.cpp"]),
            None,
            || Ok(Vec::new()),
            || Closed {
                problem: Some(languages.clone()),
                languages: vec![ClosedLanguage {
                    language: "cpp".into(),
                    closed_by: languages.clone(),
                }],
                names: PluginNames::ids(),
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.starts_with("No language is open for this problem right now"),
            "{err}"
        );
        assert!(!err.contains("--language"), "{err}");
        assert_eq!(err.matches("standard-languages").count(), 1, "{err}");
    }
}
