use std::path::{Path, PathBuf};

use serde::Deserialize;

const DEV_CONFIG_FILE: &str = "broccoli.dev.toml";

pub const BUILTIN_IGNORE_DIRS: &[&str] = &["target", ".git", "node_modules"];

pub struct ResolvedDevConfig {
    pub extra_ignores: Vec<String>,
    pub frontend_dir: Option<PathBuf>,
    pub frontend_install_cmd: Vec<String>,
    pub frontend_build_cmd: Vec<String>,
    pub frontend_dev_cmd: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawDevConfig {
    watch: RawWatchConfig,
    build: RawBuildConfig,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawWatchConfig {
    ignore: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawBuildConfig {
    frontend_dir: Option<String>,
    frontend_install_cmd: Option<String>,
    frontend_build_cmd: Option<String>,
    frontend_dev_cmd: Option<String>,
}

pub fn resolve(plugin_dir: &Path, web_root: Option<&str>) -> ResolvedDevConfig {
    let raw = load_raw(plugin_dir);

    let frontend_dir =
        resolve_frontend_dir(plugin_dir, web_root, raw.build.frontend_dir.as_deref());

    let frontend_install_cmd = match raw.build.frontend_install_cmd {
        Some(cmd) => shell_words(cmd.trim()),
        None => default_install_cmd(frontend_dir.as_deref().unwrap_or(plugin_dir)),
    };

    let frontend_build_cmd = match raw.build.frontend_build_cmd {
        Some(cmd) => shell_words(cmd.trim()),
        None => vec!["pnpm".into(), "build".into()],
    };

    let frontend_dev_cmd = match raw.build.frontend_dev_cmd {
        Some(cmd) => shell_words(cmd.trim()),
        None => vec!["pnpm".into(), "dev".into()],
    };

    ResolvedDevConfig {
        extra_ignores: raw.watch.ignore,
        frontend_dir,
        frontend_install_cmd,
        frontend_build_cmd,
        frontend_dev_cmd,
    }
}

fn load_raw(plugin_dir: &Path) -> RawDevConfig {
    let path = plugin_dir.join(DEV_CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(config) => config,
            Err(e) => {
                eprintln!(
                    "Warning: failed to parse {}: {}. Using defaults.",
                    DEV_CONFIG_FILE, e
                );
                RawDevConfig::default()
            }
        },
        Err(_) => RawDevConfig::default(),
    }
}

fn resolve_frontend_dir(
    plugin_dir: &Path,
    web_root: Option<&str>,
    explicit: Option<&str>,
) -> Option<PathBuf> {
    if let Some(dir) = explicit {
        return Some(plugin_dir.join(dir));
    }

    if let Some(root) = web_root {
        let root_path = Path::new(root);
        if let Some(parent) = root_path.parent().filter(|p| !p.as_os_str().is_empty()) {
            let candidate = plugin_dir.join(parent);
            if candidate.join("package.json").exists() {
                return Some(candidate);
            }
        }
    }

    let candidate = plugin_dir.join("frontend");
    if candidate.join("package.json").exists() {
        return Some(candidate);
    }

    if plugin_dir.join("package.json").exists() {
        return Some(plugin_dir.to_path_buf());
    }

    None
}

/// `pnpm install`, plus `--ignore-workspace` when `dir` sits inside a pnpm
/// workspace that does not list it as a member: from such a directory pnpm
/// would otherwise install the enclosing workspace instead of the plugin.
fn default_install_cmd(dir: &Path) -> Vec<String> {
    let mut cmd = vec!["pnpm".to_string(), "install".to_string()];
    if outside_enclosing_workspace(dir) {
        cmd.push("--ignore-workspace".into());
    }
    cmd
}

/// Whether the nearest `pnpm-workspace.yaml` at or above `dir` exists and its
/// `packages` patterns leave `dir` out.
fn outside_enclosing_workspace(dir: &Path) -> bool {
    dir.canonicalize()
        .is_ok_and(|dir| outside_enclosing_workspace_of(&dir))
}

/// [`outside_enclosing_workspace`] for an absolute, symlink-free `dir`, which
/// need not exist yet.
pub(crate) fn outside_enclosing_workspace_of(dir: &Path) -> bool {
    for root in dir.ancestors() {
        let manifest = root.join("pnpm-workspace.yaml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let rel: Vec<String> = dir
            .strip_prefix(root)
            .unwrap_or(Path::new(""))
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        return !is_workspace_member(&rel, &workspace_patterns(&text));
    }
    false
}

/// The entries of a `pnpm-workspace.yaml`'s top-level `packages` list.
fn workspace_patterns(text: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    let mut in_packages = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if !line.starts_with([' ', '\t', '-']) && !trimmed.is_empty() {
            in_packages = trimmed.starts_with("packages:");
            continue;
        }
        if in_packages && let Some(item) = trimmed.strip_prefix('-') {
            let item = item.split(" #").next().unwrap_or("").trim();
            patterns.push(item.trim_matches(|c| c == '"' || c == '\'').to_string());
        }
    }
    patterns
}

/// pnpm's membership rule: the workspace root is always a member; otherwise a
/// directory is a member when a pattern matches it and no `!` pattern does.
fn is_workspace_member(rel: &[String], patterns: &[String]) -> bool {
    if rel.is_empty() {
        return true;
    }
    let matches = |pattern: &str| {
        let parts: Vec<&str> = pattern
            .trim_start_matches("./")
            .split('/')
            .filter(|p| !p.is_empty())
            .collect();
        path_glob_match(&parts, rel)
    };
    let included = patterns
        .iter()
        .filter(|p| !p.starts_with('!'))
        .any(|p| matches(p));
    let excluded = patterns
        .iter()
        .filter_map(|p| p.strip_prefix('!'))
        .any(matches);
    included && !excluded
}

/// Path-component glob: `**` matches any number of components, and every
/// other part matches one component with [`glob_match`].
fn path_glob_match(pattern: &[&str], path: &[String]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| path_glob_match(rest, &path[skip..])),
        Some((part, rest)) => path
            .split_first()
            .is_some_and(|(head, tail)| glob_match(part, head) && path_glob_match(rest, tail)),
    }
}

fn shell_words(cmd: &str) -> Vec<String> {
    shlex::split(cmd).unwrap_or_else(|| cmd.split_whitespace().map(String::from).collect())
}

pub fn should_ignore(
    relative: &Path,
    extra_ignores: &[String],
    web_root_relative: Option<&Path>,
) -> bool {
    let components: Vec<_> = relative.components().collect();

    for comp in &components {
        let s = comp.as_os_str().to_string_lossy();
        if BUILTIN_IGNORE_DIRS.contains(&s.as_ref()) {
            return true;
        }
    }

    if web_root_relative.is_some_and(|wr| relative.starts_with(wr)) {
        return true;
    }

    let filename = relative.file_name().unwrap_or_default().to_string_lossy();

    for pattern in extra_ignores {
        let pat = pattern.trim_end_matches('/');

        if pattern.ends_with('/') {
            for comp in &components {
                if comp.as_os_str().to_string_lossy() == pat {
                    return true;
                }
            }
            continue;
        }

        if pat.contains('*') {
            if glob_match(pat, &filename) {
                return true;
            }
            continue;
        }

        if filename == pat {
            return true;
        }
        for comp in &components {
            if comp.as_os_str().to_string_lossy() == pat {
                return true;
            }
        }
    }

    false
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let mut p = pattern.chars().peekable();
    let mut t = text.chars().peekable();

    while p.peek().is_some() || t.peek().is_some() {
        match p.peek() {
            Some('*') => {
                p.next();
                if p.peek().is_none() {
                    return true;
                }
                let remaining: String = p.collect();
                let text_remaining: String = t.collect();
                for i in 0..=text_remaining.len() {
                    if glob_match(&remaining, &text_remaining[i..]) {
                        return true;
                    }
                }
                return false;
            }
            Some('?') => {
                p.next();
                if t.next().is_none() {
                    return false;
                }
            }
            Some(&pc) => {
                p.next();
                match t.next() {
                    Some(tc) if tc == pc => {}
                    _ => return false,
                }
            }
            None => return false,
        }
    }

    true
}

pub enum FileKind {
    Backend,
    Frontend,
    PluginManifest,
    Unknown,
}

pub fn classify_file(path: &Path, plugin_dir: &Path, frontend_dir: Option<&Path>) -> FileKind {
    let relative = path.strip_prefix(plugin_dir).unwrap_or(path);
    let filename = relative.file_name().unwrap_or_default().to_string_lossy();

    if filename == "plugin.toml" {
        return FileKind::PluginManifest;
    }

    let ext = path.extension().unwrap_or_default().to_string_lossy();

    let in_fe_dir = frontend_dir.is_some_and(|fd| path.starts_with(fd));

    match ext.as_ref() {
        "rs" => FileKind::Backend,
        "tsx" | "jsx" | "css" | "scss" | "less" | "svg" | "html" => FileKind::Frontend,
        "ts" | "js" | "json" => {
            if in_fe_dir {
                FileKind::Frontend
            } else {
                FileKind::Unknown
            }
        }
        "toml" => {
            if in_fe_dir {
                FileKind::Unknown
            } else {
                FileKind::Backend
            }
        }
        _ => FileKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(path: &str) -> Vec<String> {
        path.split('/').map(String::from).collect()
    }

    #[test]
    fn workspace_patterns_read_the_packages_list_only() {
        let yaml = "packages:\n  - packages/*\n  - 'plugins/*/frontend' # plugins\n  - \"!plugins/old/frontend\"\n\ncatalog:\n  tsdown: 0.21.10\n";
        assert_eq!(
            workspace_patterns(yaml),
            vec!["packages/*", "plugins/*/frontend", "!plugins/old/frontend"]
        );
    }

    #[test]
    fn membership_follows_pnpm_globs_and_negations() {
        let patterns: Vec<String> = [
            "packages/*",
            "plugins/*/frontend",
            "docs/**",
            "!plugins/old/frontend",
        ]
        .map(String::from)
        .into();
        assert!(is_workspace_member(&[], &patterns));
        assert!(is_workspace_member(&rel("packages/web"), &patterns));
        assert!(is_workspace_member(
            &rel("plugins/icpc/frontend"),
            &patterns
        ));
        assert!(is_workspace_member(&rel("docs/a/b"), &patterns));
        assert!(!is_workspace_member(
            &rel("plugins/old/frontend"),
            &patterns
        ));
        assert!(!is_workspace_member(&rel("plugins/icpc"), &patterns));
        assert!(!is_workspace_member(&rel("examples/p/frontend"), &patterns));
        assert!(is_workspace_member(
            &rel("packages/web-sdk"),
            &["packages/web-*".into()]
        ));
    }

    #[test]
    fn install_ignores_the_workspace_only_for_a_non_member() {
        let root = std::env::temp_dir().join(format!("broccoli-dev-cli-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["plugins/a/frontend", "examples/b/frontend"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(
            root.join("pnpm-workspace.yaml"),
            "packages:\n  - plugins/*/frontend\n",
        )
        .unwrap();
        assert_eq!(
            default_install_cmd(&root.join("plugins/a/frontend")),
            ["pnpm", "install"]
        );
        assert_eq!(
            default_install_cmd(&root.join("examples/b/frontend")),
            ["pnpm", "install", "--ignore-workspace"]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn test_glob_match() {
        assert!(glob_match("*.log", "test.log"));
        assert!(glob_match("*.log", "app.log"));
        assert!(!glob_match("*.log", "test.txt"));
        assert!(glob_match("*.rs", "lib.rs"));
        assert!(glob_match("test_*", "test_foo"));
        assert!(!glob_match("test_*", "prod_foo"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn test_should_ignore_builtin() {
        assert!(should_ignore(Path::new("target/debug/foo.rs"), &[], None));
        assert!(should_ignore(Path::new(".git/config"), &[], None));
        assert!(should_ignore(
            Path::new("node_modules/pkg/index.js"),
            &[],
            None
        ));
        assert!(!should_ignore(Path::new("src/lib.rs"), &[], None));
    }

    #[test]
    fn test_should_ignore_web_root() {
        let wr = Path::new("frontend/dist");
        assert!(should_ignore(
            Path::new("frontend/dist/index.js"),
            &[],
            Some(wr)
        ));
        assert!(!should_ignore(
            Path::new("frontend/src/App.tsx"),
            &[],
            Some(wr)
        ));
    }

    #[test]
    fn test_should_ignore_extra_patterns() {
        let extras = vec!["*.log".to_string(), "tmp/".to_string()];
        assert!(should_ignore(Path::new("app.log"), &extras, None));
        assert!(should_ignore(Path::new("tmp/cache"), &extras, None));
        assert!(!should_ignore(Path::new("src/main.rs"), &extras, None));
    }

    #[test]
    fn test_shell_words() {
        assert_eq!(shell_words("pnpm build"), vec!["pnpm", "build"]);
        assert_eq!(shell_words("npm run build"), vec!["npm", "run", "build"]);
        assert_eq!(shell_words("bun build"), vec!["bun", "build"]);
        assert_eq!(
            shell_words(r#"sh -c "npm run build""#),
            vec!["sh", "-c", "npm run build"]
        );
    }
}
