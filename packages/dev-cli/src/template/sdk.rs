//! Where a scaffolded plugin takes the Broccoli SDKs from.
//!
//! The SDKs are the Rust crates `broccoli-server-sdk` (with
//! `broccoli-types`) and the web package `@broccoli/web-sdk`. A plugin
//! depends on them in one of two ways:
//!
//! - from a local SDK directory laid out like a Broccoli checkout's
//!   `packages/` (`server-sdk/`, `broccoli-types/`, `web-sdk/`): the
//!   checkout the plugin is created in, found automatically, or the
//!   directory `--sdk` names. Nothing is fetched for the SDKs themselves, so
//!   this is how a plugin is built on a machine without internet access.
//! - from crates.io, at the version of the SDK this CLI was built with. The
//!   web SDK has no such source, so a frontend outside a checkout needs
//!   `--sdk`.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};

/// Where the SDKs come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkSource {
    /// An absolute, canonical directory holding the SDK packages.
    Local(PathBuf),
    /// crates.io, at [`broccoli_server_sdk::VERSION`].
    Registry,
}

impl SdkSource {
    /// The SDK directory `explicit` names, else the `packages/` of a Broccoli
    /// checkout that `plugin_dir` lies in, else crates.io.
    pub fn locate(explicit: Option<&Path>, plugin_dir: &Path) -> Result<Self> {
        if let Some(dir) = explicit {
            let dir = dir
                .canonicalize()
                .with_context(|| format!("Cannot find the SDK directory '{}'", dir.display()))?;
            if !is_sdk_dir(&dir) {
                bail!(
                    "'{}' is not an SDK directory: it has no server-sdk/Cargo.toml. \
                     Pass the packages/ directory of a Broccoli checkout.",
                    dir.display()
                );
            }
            return Ok(Self::Local(dir));
        }
        let start = absolute(plugin_dir)?;
        Ok(start
            .ancestors()
            .map(|a| a.join("packages"))
            .find(|packages| is_sdk_dir(packages))
            .map_or(Self::Registry, Self::Local))
    }

    /// The dependency on `broccoli-server-sdk` for a crate at `crate_dir`, as
    /// the fields of a `Cargo.toml` inline table.
    pub fn server_sdk_dep(&self, crate_dir: &Path) -> Result<String> {
        Ok(match self {
            Self::Local(dir) => format!(
                "path = \"{}\"",
                relative(&absolute(crate_dir)?, &dir.join("server-sdk")).display()
            ),
            Self::Registry => format!("version = \"{}\"", broccoli_server_sdk::VERSION),
        })
    }

    /// The `package.json` version spec of `@broccoli/web-sdk` for a frontend
    /// at `frontend_dir`: `workspace:*` when the frontend is a member of the
    /// pnpm workspace around the SDK, else a `file:` path to it.
    pub fn web_sdk_dep(&self, frontend_dir: &Path) -> Result<String> {
        let Self::Local(dir) = self else {
            bail!(
                "A plugin frontend outside a Broccoli checkout needs the web SDK from a local \
                 directory: pass --sdk <DIR>, the packages/ directory of a Broccoli checkout \
                 (with packages/web-sdk built)."
            );
        };
        let frontend_dir = absolute(frontend_dir)?;
        let in_workspace = dir
            .parent()
            .is_some_and(|root| root.join("pnpm-workspace.yaml").is_file())
            && dir
                .parent()
                .is_some_and(|root| frontend_dir.starts_with(root))
            && !crate::dev_config::outside_enclosing_workspace_of(&frontend_dir);
        Ok(if in_workspace {
            "workspace:*".into()
        } else {
            format!(
                "file:{}",
                relative(&frontend_dir, &dir.join("web-sdk")).display()
            )
        })
    }
}

fn is_sdk_dir(dir: &Path) -> bool {
    dir.join("server-sdk/Cargo.toml").is_file()
}

/// `path` made absolute, with the part that exists resolved through
/// symlinks, so it compares with a canonical path. The rest (directories
/// the scaffold has yet to create) is appended as given.
fn absolute(path: &Path) -> Result<PathBuf> {
    let path = std::path::absolute(path).context("Cannot resolve the plugin directory")?;
    let mut existing = path.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return Ok(missing.iter().rev().fold(canonical, |p, c| p.join(c)));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return Ok(path),
        }
    }
}

/// The path from directory `from` to `to`, both absolute.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let from: Vec<Component> = from.components().collect();
    let to: Vec<Component> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path: PathBuf = from[common..].iter().map(|_| "..").collect();
    path.extend(&to[common..]);
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_climb_to_the_common_ancestor() {
        assert_eq!(
            relative(
                Path::new("/r/plugins/demo"),
                Path::new("/r/packages/server-sdk")
            ),
            Path::new("../../packages/server-sdk")
        );
        assert_eq!(relative(Path::new("/a"), Path::new("/a/b")), Path::new("b"));
    }

    #[test]
    fn a_plugin_inside_a_checkout_uses_its_packages() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("packages/server-sdk")).unwrap();
        std::fs::write(root.path().join("packages/server-sdk/Cargo.toml"), "").unwrap();
        let plugin = root.path().join("plugins/demo");

        let source = SdkSource::locate(None, &plugin).unwrap();
        assert_eq!(
            source,
            SdkSource::Local(root.path().canonicalize().unwrap().join("packages"))
        );
        assert_eq!(
            source.server_sdk_dep(&plugin).unwrap(),
            r#"path = "../../packages/server-sdk""#
        );
    }

    #[test]
    fn a_plugin_elsewhere_uses_the_published_crate_and_needs_a_local_web_sdk() {
        let dir = tempfile::tempdir().unwrap();
        let source = SdkSource::locate(None, &dir.path().join("demo")).unwrap();
        assert_eq!(source, SdkSource::Registry);
        assert_eq!(
            source.server_sdk_dep(dir.path()).unwrap(),
            format!("version = \"{}\"", broccoli_server_sdk::VERSION)
        );
        assert!(source.web_sdk_dep(dir.path()).is_err());
    }
}
