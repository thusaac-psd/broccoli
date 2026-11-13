use anyhow::{Result, bail};
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

/// Write .broccoli file in cwd
pub fn save_context(ctx: &ProjectContext) -> anyhow::Result<()> {
    let content = toml::to_string_pretty(ctx)?;
    std::fs::write(CONTEXT_FILE, content)?;
    Ok(())
}

/// Maps to server-side language identifiers expected by the `standard-languages` plugin.
pub fn detect_language(filename: &str) -> Option<&'static str> {
    let ext = Path::new(filename).extension()?.to_str()?;
    Some(match ext {
        "py" => "python3",
        "cpp" | "cc" | "cxx" => "cpp",
        "c" => "c",
        "java" => "java",
        "rs" => "rust",
        "go" => "go",
        "js" => "javascript",
        "ts" => "typescript",
        "kt" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "hs" => "haskell",
        "cs" => "csharp",
        _ => return None,
    })
}

pub fn resolve_language(files: &[String], explicit: Option<&str>) -> Result<String> {
    if let Some(language) = explicit {
        return Ok(language.to_string());
    }

    let mut detected = None;
    for file in files {
        if let Some(language) = detect_language(file) {
            if detected.is_some_and(|previous| previous != language) {
                bail!("Source files have different languages; specify --language explicitly");
            }
            detected = Some(language);
        }
    }

    detected.map(str::to_string).ok_or_else(|| {
        anyhow::anyhow!("Cannot detect a language from source files; specify --language explicitly")
    })
}

#[cfg(test)]
mod tests {
    use super::resolve_language;

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn detects_language_after_unrecognized_file() {
        assert_eq!(
            resolve_language(&files(&["notes.txt", "main.py"]), None).unwrap(),
            "python3"
        );
    }

    #[test]
    fn rejects_ambiguous_languages() {
        assert!(
            resolve_language(&files(&["main.py", "main.cpp"]), None)
                .unwrap_err()
                .to_string()
                .contains("different languages")
        );
    }

    #[test]
    fn rejects_unknown_language_instead_of_defaulting_to_cpp() {
        assert!(
            resolve_language(&files(&["main.unknown"]), None)
                .unwrap_err()
                .to_string()
                .contains("Cannot detect a language")
        );
    }

    #[test]
    fn explicit_language_overrides_detection() {
        assert_eq!(
            resolve_language(&files(&["main.py", "main.cpp"]), Some("rust")).unwrap(),
            "rust"
        );
    }
}
