//! Where a scaffolded plugin takes the Broccoli SDKs from.
//!
//! The SDKs are the Rust crates `broccoli-server-sdk` (with
//! `broccoli-types`) and the web package `@broccoli/web-sdk`. A plugin
//! depends on them through a local SDK directory holding `server-sdk/`,
//! `broccoli-types/` and `web-sdk/`, which comes in two forms:
//!
//! - the `packages/` directory of a Broccoli checkout, whose web SDK is a
//!   pnpm workspace member with `catalog:` versions;
//! - the `sdk/` directory of a release bundle, staged by
//!   `scripts/stage-sdk.sh` with the web SDK as `pnpm pack` publishes it and
//!   the crates.io crates a backend needs in `vendor/`.
//!
//! The directory is the one `--sdk` names, else the first found above the
//! plugin directory (the checkout the plugin is created in), else the first
//! found above the `broccoli-dev` executable (the checkout it was built in,
//! or the bundle it ships in). Nothing is fetched for the SDKs, so a plugin
//! builds on a machine without internet access.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

/// A local SDK directory: absolute and canonical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkSource {
    /// The `packages/` directory of a Broccoli checkout.
    Checkout(PathBuf),
    /// The `sdk/` directory of a release bundle.
    Staged(PathBuf),
}

impl SdkSource {
    /// The SDK directory for a plugin created at `plugin_dir`; see the module
    /// documentation for the search order.
    pub fn locate(explicit: Option<&Path>, plugin_dir: &Path) -> Result<Self> {
        let exe = std::env::current_exe().and_then(|exe| exe.canonicalize());
        Self::locate_from(explicit, plugin_dir, exe.ok().as_deref())
    }

    fn locate_from(explicit: Option<&Path>, plugin_dir: &Path, exe: Option<&Path>) -> Result<Self> {
        if let Some(dir) = explicit {
            let dir = dir
                .canonicalize()
                .with_context(|| format!("Cannot find the SDK directory '{}'", dir.display()))?;
            return Self::at(dir.clone()).ok_or_else(|| {
                anyhow!(
                    "'{}' is not an SDK directory: it has no server-sdk/Cargo.toml. Pass the \
                     sdk/ directory of a release bundle or the packages/ directory of a \
                     Broccoli checkout.",
                    dir.display()
                )
            });
        }
        let plugin_dir = absolute(plugin_dir)?;
        [Some(plugin_dir.as_path()), exe.and_then(Path::parent)]
            .into_iter()
            .flatten()
            .flat_map(Path::ancestors)
            .flat_map(|dir| [dir.join("packages"), dir.join("sdk")])
            .find_map(Self::at)
            .ok_or_else(|| {
                anyhow!(
                    "Cannot find the Broccoli SDKs: '{}' is not inside a Broccoli checkout and \
                     broccoli-dev does not run from a release bundle (<bundle>/sdk). Pass \
                     --sdk <DIR>: the sdk/ directory of a release bundle or the packages/ \
                     directory of a Broccoli checkout.",
                    plugin_dir.display()
                )
            })
    }

    /// The SDK directory `dir`, if it is one: a checkout's `packages/` when a
    /// pnpm workspace manifest sits next to it, else a staged `sdk/`.
    fn at(dir: PathBuf) -> Option<Self> {
        if !dir.join("server-sdk/Cargo.toml").is_file() {
            return None;
        }
        let in_workspace = dir
            .parent()
            .is_some_and(|root| root.join("pnpm-workspace.yaml").is_file());
        Some(match in_workspace {
            true => Self::Checkout(dir),
            false => Self::Staged(dir),
        })
    }

    fn dir(&self) -> &Path {
        match self {
            Self::Checkout(dir) | Self::Staged(dir) => dir,
        }
    }

    /// The dependency on `broccoli-server-sdk` for a crate at `crate_dir`, as
    /// the fields of a `Cargo.toml` inline table.
    pub fn server_sdk_dep(&self, crate_dir: &Path) -> Result<String> {
        Ok(format!(
            "path = \"{}\"",
            relative(&absolute(crate_dir)?, &self.dir().join("server-sdk")).display()
        ))
    }

    /// The `package.json` version spec of `@broccoli/web-sdk` for a frontend
    /// at `frontend_dir`. A member of the checkout's pnpm workspace takes
    /// `workspace:*`. Anything else needs the package as published, since
    /// pnpm resolves `catalog:` versions only inside the workspace that
    /// defines them: a staged web SDK is that already, and a checkout's is
    /// packed with `pnpm pack` into `frontend_dir`.
    pub fn web_sdk_dep(&self, frontend_dir: &Path) -> Result<String> {
        let frontend_dir = absolute(frontend_dir)?;
        let web_sdk = self.dir().join("web-sdk");
        Ok(match self {
            Self::Staged(_) => format!("file:{}", relative(&frontend_dir, &web_sdk).display()),
            Self::Checkout(dir)
                if dir
                    .parent()
                    .is_some_and(|root| frontend_dir.starts_with(root))
                    && !crate::dev_config::outside_enclosing_workspace_of(&frontend_dir) =>
            {
                "workspace:*".into()
            }
            Self::Checkout(_) => format!("file:{}", pack(&web_sdk, &frontend_dir)?),
        })
    }
}

/// Packs the web SDK at `web_sdk` into `dest` with `pnpm pack`, which writes
/// real versions for its `catalog:` and `workspace:` ones, and returns the
/// tarball's file name.
fn pack(web_sdk: &Path, dest: &Path) -> Result<String> {
    if !web_sdk.join("dist").is_dir() {
        bail!(
            "The web SDK in '{}' is not built: run `pnpm install` and \
             `pnpm --filter @broccoli/web-sdk build` in the checkout.",
            web_sdk.display()
        );
    }
    std::fs::create_dir_all(dest)?;
    let out = Command::new("pnpm")
        .args(["pack", "--json", "--pack-destination"])
        .arg(dest)
        .current_dir(web_sdk)
        .output()
        .context("Failed to run 'pnpm pack' for the web SDK. Is pnpm installed?")?;
    if !out.status.success() {
        bail!(
            "'pnpm pack' failed for the web SDK in '{}':\n{}",
            web_sdk.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    #[derive(serde::Deserialize)]
    struct Packed {
        filename: PathBuf,
    }
    let packed: Packed =
        serde_json::from_slice(&out.stdout).context("Cannot read the output of 'pnpm pack'")?;
    packed
        .filename
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("'pnpm pack' named no tarball")
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

    fn sdk_dir(dir: &Path) {
        std::fs::create_dir_all(dir.join("server-sdk")).unwrap();
        std::fs::write(dir.join("server-sdk/Cargo.toml"), "").unwrap();
    }

    #[test]
    fn a_plugin_inside_a_checkout_uses_its_packages() {
        let root = tempfile::tempdir().unwrap();
        sdk_dir(&root.path().join("packages"));
        std::fs::write(root.path().join("pnpm-workspace.yaml"), "").unwrap();
        let plugin = root.path().join("plugins/demo");

        let source = SdkSource::locate_from(None, &plugin, None).unwrap();
        assert_eq!(
            source,
            SdkSource::Checkout(root.path().canonicalize().unwrap().join("packages"))
        );
        assert_eq!(
            source.server_sdk_dep(&plugin).unwrap(),
            r#"path = "../../packages/server-sdk""#
        );
    }

    #[test]
    fn a_plugin_elsewhere_uses_the_sdk_of_the_bundle_the_cli_runs_from() {
        let bundle = tempfile::tempdir().unwrap();
        sdk_dir(&bundle.path().join("sdk"));
        let exe = bundle
            .path()
            .canonicalize()
            .unwrap()
            .join("cli/broccoli-dev");
        let elsewhere = tempfile::tempdir().unwrap();
        let plugin = elsewhere.path().join("demo");

        let source = SdkSource::locate_from(None, &plugin, Some(&exe)).unwrap();
        let sdk = bundle.path().canonicalize().unwrap().join("sdk");
        assert_eq!(source, SdkSource::Staged(sdk.clone()));
        let web = source.web_sdk_dep(&plugin).unwrap();
        let web = Path::new(web.strip_prefix("file:").unwrap());
        std::fs::create_dir_all(sdk.join("web-sdk")).unwrap();
        std::fs::create_dir_all(&plugin).unwrap();
        assert_eq!(
            plugin.join(web).canonicalize().unwrap(),
            sdk.join("web-sdk")
        );
    }

    #[test]
    fn a_plugin_with_no_sdk_in_reach_is_refused_with_the_way_to_name_one() {
        let elsewhere = tempfile::tempdir().unwrap();
        let exe = elsewhere.path().join("bin/broccoli-dev");
        let err = SdkSource::locate_from(None, &elsewhere.path().join("demo"), Some(&exe))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("--sdk <DIR>") && err.contains("<bundle>/sdk"),
            "{err}"
        );
    }
}
