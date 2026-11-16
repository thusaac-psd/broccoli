use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use broccoli_types::manifest::PluginManifest;
use clap::Args;
use console::style;
use notify::{Event, RecursiveMode, Watcher};

use crate::dev_config::{self, FileKind, ResolvedDevConfig};
use broccoli_cli_core::client::Client;
use broccoli_cli_core::config;

use super::wasm::copy_wasm_artifact;
use super::{parse_manifest, read_manifest};

#[derive(Args)]
pub struct WatchPluginArgs {
    #[arg(default_value = ".")]
    pub path: PathBuf,

    #[arg(long, env = "BROCCOLI_URL")]
    pub server: Option<String>,

    #[arg(long, env = "BROCCOLI_TOKEN")]
    pub token: Option<String>,

    #[arg(long)]
    pub install: bool,

    #[arg(long)]
    pub release: bool,

    #[arg(long, default_value = "500")]
    pub debounce: u64,
}

/// A parsed `plugin.toml` and the fingerprint of the bytes it was parsed from.
struct ManifestSnapshot {
    manifest: PluginManifest,
    fingerprint: u64,
}

impl ManifestSnapshot {
    fn read(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path).context("Failed to read plugin.toml")?;
        Self::parse(&content)
    }

    fn parse(content: &str) -> anyhow::Result<Self> {
        Ok(Self {
            manifest: parse_manifest(content)?,
            fingerprint: fingerprint_bytes(content.as_bytes()),
        })
    }
}

/// What the watch loop knows about the plugin as last built. It advances only
/// when a backend build and upload succeeds, so a failed one is retried on the
/// next trigger even if `plugin.toml` itself has not changed since.
struct WatchState {
    /// Fingerprint of the `plugin.toml` the last successful build used.
    built_manifest: Option<u64>,
    web_root: Option<PathBuf>,
    server_entry: Option<PathBuf>,
}

impl WatchState {
    fn new(plugin_dir: &Path, manifest: &PluginManifest) -> Self {
        let mut state = Self {
            built_manifest: None,
            web_root: None,
            server_entry: None,
        };
        state.track_paths(plugin_dir, manifest);
        state
    }

    /// Whether the build that last succeeded already used a `plugin.toml`
    /// with this fingerprint. An unreadable manifest (`None`) never counts as
    /// built.
    fn manifest_is_built(&self, current: Option<u64>) -> bool {
        current.is_some() && current == self.built_manifest
    }

    /// Records a successful build and passes a failed one through untouched.
    fn finish_build(
        &mut self,
        plugin_dir: &Path,
        result: anyhow::Result<ManifestSnapshot>,
    ) -> anyhow::Result<()> {
        let built = result?;
        self.built_manifest = Some(built.fingerprint);
        self.track_paths(plugin_dir, &built.manifest);
        Ok(())
    }

    fn track_paths(&mut self, plugin_dir: &Path, manifest: &PluginManifest) {
        self.web_root = manifest.web.as_ref().map(|w| plugin_dir.join(&w.root));
        self.server_entry = manifest.server.as_ref().map(|s| plugin_dir.join(&s.entry));
    }
}

enum ChangeKind {
    Backend,
    FrontendOutput,
    ManifestChanged,
}

pub fn run(args: WatchPluginArgs) -> anyhow::Result<()> {
    let plugin_dir = args
        .path
        .canonicalize()
        .with_context(|| format!("Cannot find directory '{}'", args.path.display()))?;

    let manifest_path = plugin_dir.join("plugin.toml");
    if !manifest_path.exists() {
        bail!(
            "Not a broccoli plugin directory: no plugin.toml found in '{}'.\n\
             Run `broccoli plugin new` to create a new plugin.",
            plugin_dir.display()
        );
    }

    let creds = config::resolve_credentials(args.server.as_deref(), args.token.as_deref())?;
    // One shared client so plugin uploads refresh the token on 401 (and persist
    // the rotation) instead of failing mid-session with a stale token.
    let client = Client::new(creds);

    let manifest = read_manifest(&manifest_path)?;
    let plugin_name = manifest.name.as_str();

    println!(
        "{}  Watching plugin {}...",
        style("→").blue().bold(),
        style(plugin_name).cyan()
    );
    println!("   Server: {}", style(client.server()).dim());

    let web_root_str = manifest.web.as_ref().map(|w| w.root.as_str());
    let dev = dev_config::resolve(&plugin_dir, web_root_str);

    let mut state = WatchState::new(&plugin_dir, &manifest);
    let mut last_uploaded_archive_fingerprint = None;

    let mut fe_child: Option<Child> = None;
    if manifest.web.is_some() {
        let fe_dir = dev.frontend_dir.as_deref().unwrap_or(&plugin_dir);

        let node_modules_exists = fe_dir.join("node_modules").exists();
        if !node_modules_exists || args.install {
            let install_cmd_str = dev.frontend_install_cmd.join(" ");
            println!(
                "{}  Preparing frontend dependencies: '{}'...",
                style("→").blue().bold(),
                style(&install_cmd_str).cyan()
            );

            let (prog, args) = dev
                .frontend_install_cmd
                .split_first()
                .context("frontend_install_cmd is empty")?;

            let status = Command::new(prog)
                .args(args)
                .current_dir(fe_dir)
                .status()
                .with_context(|| format!("Failed to run '{}'", install_cmd_str))?;

            if !status.success() {
                bail!("Frontend installation failed");
            }
        }

        match spawn_frontend_dev(&dev, &plugin_dir) {
            Ok(child) => {
                fe_child = Some(child);
                println!(
                    "{}  Frontend dev server started ({})",
                    style("✓").green().bold(),
                    style(dev.frontend_dev_cmd.join(" ")).dim()
                );
            }
            Err(e) => {
                eprintln!(
                    "{}  Failed to start frontend dev server: {}",
                    style("✗").red().bold(),
                    e
                );
                eprintln!(
                    "   Frontend changes will not be auto-rebuilt.\n\
                     Set build.frontend_dev_cmd in broccoli.dev.toml to customize."
                );
            }
        }
    }

    let child_id = fe_child.as_ref().map(|c| c.id());
    ctrlc::set_handler(move || {
        if let Some(pid) = child_id {
            #[cfg(unix)]
            {
                unsafe {
                    libc::kill(pid as i32, libc::SIGTERM);
                }
            }
            #[cfg(not(unix))]
            {
                let _ = pid;
            }
        }
        std::process::exit(0);
    })
    .ok();

    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .context("Failed to create file watcher")?;

    watcher
        .watch(&plugin_dir, RecursiveMode::Recursive)
        .context("Failed to watch plugin directory")?;

    println!(
        "{}  Watching for changes... (Ctrl+C to stop)",
        style("✓").green().bold()
    );

    let initial = initial_build_and_upload(
        &plugin_dir,
        &manifest_path,
        &client,
        &dev,
        args.release,
        &mut last_uploaded_archive_fingerprint,
    );
    if let Err(e) = state.finish_build(&plugin_dir, initial) {
        eprintln!("{}  Initial build failed: {}", style("✗").red().bold(), e);
    }

    let debounce = Duration::from_millis(args.debounce);
    let mut last_build = Instant::now();
    let mut pending_changes: HashSet<PathBuf> = HashSet::new();

    loop {
        match rx.recv_timeout(debounce) {
            Ok(event) => {
                for path in event.paths {
                    let relative = path.strip_prefix(&plugin_dir).unwrap_or(&path);

                    if dev_config::should_ignore(relative, &dev.extra_ignores, None) {
                        continue;
                    }

                    pending_changes.insert(path);
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if pending_changes.is_empty() {
                    continue;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
        }

        if last_build.elapsed() < debounce {
            continue;
        }

        let change_kind = classify_changes(
            &pending_changes,
            &plugin_dir,
            state.web_root.as_deref(),
            state.server_entry.as_deref(),
            dev.frontend_dir.as_deref(),
        );
        pending_changes.clear();

        let Some(change_kind) = change_kind else {
            continue;
        };
        last_build = Instant::now();

        match change_kind {
            ChangeKind::ManifestChanged | ChangeKind::Backend => {
                if let ChangeKind::ManifestChanged = change_kind {
                    if state.manifest_is_built(fingerprint_file(&manifest_path)) {
                        continue;
                    }
                    println!(
                        "\n{}  plugin.toml changed, rebuilding backend...",
                        style("→").blue().bold(),
                    );
                } else {
                    println!(
                        "\n{}  Backend changes detected, rebuilding...",
                        style("→").blue().bold(),
                    );
                }

                let rebuilt = backend_build_and_upload(
                    &plugin_dir,
                    &manifest_path,
                    &client,
                    args.release,
                    &mut last_uploaded_archive_fingerprint,
                );
                if let Err(e) = state.finish_build(&plugin_dir, rebuilt) {
                    eprintln!("{}  Build/upload failed: {}", style("✗").red().bold(), e);
                    eprintln!("   Waiting for next change...");
                }
            }
            ChangeKind::FrontendOutput => {
                println!(
                    "\n{}  Frontend output changed, uploading...",
                    style("→").blue().bold(),
                );

                if let Err(e) = package_and_upload(
                    &plugin_dir,
                    &manifest_path,
                    &client,
                    &mut last_uploaded_archive_fingerprint,
                ) {
                    eprintln!("{}  Upload failed: {}", style("✗").red().bold(), e);
                    eprintln!("   Waiting for next change...");
                }
            }
        }
    }

    if let Some(mut child) = fe_child {
        let _ = child.kill();
        let _ = child.wait();
    }

    Ok(())
}

fn fingerprint_file(path: &Path) -> Option<u64> {
    std::fs::read(path)
        .ok()
        .map(|bytes| fingerprint_bytes(&bytes))
}

fn classify_changes(
    changed: &HashSet<PathBuf>,
    plugin_dir: &Path,
    web_root_abs: Option<&Path>,
    server_entry_abs: Option<&Path>,
    frontend_dir: Option<&Path>,
) -> Option<ChangeKind> {
    let mut has_backend = false;
    let mut has_frontend_output = false;
    let mut has_unknown = false;

    for path in changed {
        let relative = path.strip_prefix(plugin_dir).unwrap_or(path);
        let filename = relative.file_name().unwrap_or_default().to_string_lossy();

        if filename == "plugin.toml" {
            return Some(ChangeKind::ManifestChanged);
        }

        if server_entry_abs.is_some_and(|entry| path == entry) {
            continue;
        }

        if web_root_abs.is_some_and(|wr| path.starts_with(wr)) {
            has_frontend_output = true;
            continue;
        }

        if frontend_dir.is_some_and(|fd| path.starts_with(fd)) {
            continue;
        }

        match dev_config::classify_file(path, plugin_dir, frontend_dir) {
            FileKind::Backend => has_backend = true,
            FileKind::PluginManifest => return Some(ChangeKind::ManifestChanged),
            _ => has_unknown = true,
        }
    }

    if has_backend {
        Some(ChangeKind::Backend)
    } else if has_frontend_output {
        Some(ChangeKind::FrontendOutput)
    } else if has_unknown {
        Some(ChangeKind::Backend)
    } else {
        None
    }
}

fn spawn_frontend_dev(dev: &ResolvedDevConfig, _plugin_dir: &Path) -> anyhow::Result<Child> {
    let fe_dir = dev.frontend_dir.as_deref().context(
        "Cannot determine frontend directory. Set build.frontend_dir in broccoli.dev.toml",
    )?;

    if !fe_dir.exists() {
        bail!(
            "Frontend directory '{}' does not exist.\n\
             Check build.frontend_dir in broccoli.dev.toml.",
            fe_dir.display()
        );
    }

    let (program, cmd_args) = dev
        .frontend_dev_cmd
        .split_first()
        .context("frontend_dev_cmd is empty in broccoli.dev.toml")?;

    let child = Command::new(program)
        .args(cmd_args)
        .current_dir(fe_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| {
            format!(
                "Failed to run '{}' in '{}'. Is it installed?",
                dev.frontend_dev_cmd.join(" "),
                fe_dir.display()
            )
        })?;

    Ok(child)
}

fn initial_build_and_upload(
    plugin_dir: &Path,
    manifest_path: &Path,
    client: &Client,
    dev: &ResolvedDevConfig,
    release: bool,
    last_uploaded_archive_fingerprint: &mut Option<u64>,
) -> anyhow::Result<ManifestSnapshot> {
    let snapshot = ManifestSnapshot::read(manifest_path)?;
    let manifest = &snapshot.manifest;

    if manifest.server.is_some() {
        build_backend(plugin_dir, release)?;

        if let Some(ref server) = manifest.server {
            copy_wasm_artifact(plugin_dir, &server.entry, release)?;
        }
    }

    if manifest.web.is_some() {
        build_frontend(dev)?;
    }

    let archive = package_plugin(plugin_dir, manifest)?;
    upload_plugin(&archive, client, last_uploaded_archive_fingerprint)?;

    println!("{}  Plugin uploaded to server", style("✓").green().bold());

    Ok(snapshot)
}

fn backend_build_and_upload(
    plugin_dir: &Path,
    manifest_path: &Path,
    client: &Client,
    release: bool,
    last_uploaded_archive_fingerprint: &mut Option<u64>,
) -> anyhow::Result<ManifestSnapshot> {
    let snapshot = ManifestSnapshot::read(manifest_path)?;
    let manifest = &snapshot.manifest;

    if manifest.server.is_some() {
        build_backend(plugin_dir, release)?;

        if let Some(ref server) = manifest.server {
            copy_wasm_artifact(plugin_dir, &server.entry, release)?;
        }
    }

    let archive = package_plugin(plugin_dir, manifest)?;
    upload_plugin(&archive, client, last_uploaded_archive_fingerprint)?;

    println!("{}  Plugin reloaded on server", style("✓").green().bold());

    Ok(snapshot)
}

fn package_and_upload(
    plugin_dir: &Path,
    manifest_path: &Path,
    client: &Client,
    last_uploaded_archive_fingerprint: &mut Option<u64>,
) -> anyhow::Result<()> {
    let manifest = read_manifest(manifest_path)?;
    let archive = package_plugin(plugin_dir, &manifest)?;
    upload_plugin(&archive, client, last_uploaded_archive_fingerprint)?;

    println!("{}  Plugin reloaded on server", style("✓").green().bold());

    Ok(())
}

fn build_backend(plugin_dir: &Path, release: bool) -> anyhow::Result<()> {
    println!("  {}  Building backend...", style("→").blue());

    let mut cargo_args = vec!["build", "--target", "wasm32-wasip1"];
    if release {
        cargo_args.push("--release");
    }

    let status = Command::new("cargo")
        .args(&cargo_args)
        .current_dir(plugin_dir)
        .status()
        .context("Failed to run cargo build")?;

    if !status.success() {
        bail!("Backend build failed");
    }

    println!("  {}  Backend build complete", style("✓").green());
    Ok(())
}

fn build_frontend(dev: &ResolvedDevConfig) -> anyhow::Result<()> {
    println!("  {}  Building frontend...", style("→").blue());

    let fe_dir = dev.frontend_dir.as_deref().context(
        "Cannot determine frontend directory. Set build.frontend_dir in broccoli.dev.toml",
    )?;

    if !fe_dir.exists() {
        bail!(
            "Frontend directory '{}' does not exist.\n\
             Check build.frontend_dir in broccoli.dev.toml.",
            fe_dir.display()
        );
    }

    let (program, cmd_args) = dev
        .frontend_build_cmd
        .split_first()
        .context("frontend_build_cmd is empty in broccoli.dev.toml")?;

    let status = Command::new(program)
        .args(cmd_args)
        .current_dir(fe_dir)
        .status()
        .with_context(|| {
            format!(
                "Failed to run '{}'. Is it installed?",
                dev.frontend_build_cmd.join(" ")
            )
        })?;

    if !status.success() {
        bail!("Frontend build failed");
    }

    println!("  {}  Frontend build complete", style("✓").green());
    Ok(())
}

fn package_plugin(plugin_dir: &Path, manifest: &PluginManifest) -> anyhow::Result<Vec<u8>> {
    let plugin_id = plugin_dir
        .file_name()
        .and_then(|n| n.to_str())
        .context("Invalid plugin directory name")?;

    let mut builder = tar::Builder::new(Vec::new());

    add_file_to_tar(&mut builder, plugin_dir, "plugin.toml", plugin_id)?;

    if let Some(ref server) = manifest.server {
        add_file_to_tar(&mut builder, plugin_dir, &server.entry, plugin_id)?;
    }

    if let Some(ref web) = manifest.web {
        let web_root = plugin_dir.join(&web.root);
        if web_root.exists() {
            add_dir_to_tar(&mut builder, plugin_dir, &web.root, plugin_id)?;
        }
    }

    for path in manifest.translations.values() {
        add_file_to_tar(&mut builder, plugin_dir, path, plugin_id)?;
    }

    let config_dir = plugin_dir.join("config");
    if config_dir.exists() {
        add_dir_to_tar(&mut builder, plugin_dir, "config", plugin_id)?;
    }

    let tar_data = builder.into_inner().context("Failed to finalize tar")?;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(&tar_data)?;
    encoder.finish().context("Failed to compress archive")
}

fn add_file_to_tar(
    builder: &mut tar::Builder<Vec<u8>>,
    base_dir: &Path,
    relative_path: &str,
    plugin_id: &str,
) -> anyhow::Result<()> {
    let full_path = base_dir.join(relative_path);
    if !full_path.exists() {
        return Ok(());
    }
    let tar_path = format!("{}/{}", plugin_id, relative_path);
    builder
        .append_path_with_name(&full_path, &tar_path)
        .with_context(|| format!("Failed to add '{}' to archive", relative_path))?;
    Ok(())
}

fn add_dir_to_tar(
    builder: &mut tar::Builder<Vec<u8>>,
    base_dir: &Path,
    relative_dir: &str,
    plugin_id: &str,
) -> anyhow::Result<()> {
    let full_dir = base_dir.join(relative_dir);
    if !full_dir.exists() {
        return Ok(());
    }
    let tar_prefix = format!("{}/{}", plugin_id, relative_dir);
    builder
        .append_dir_all(&tar_prefix, &full_dir)
        .with_context(|| format!("Failed to add directory '{}' to archive", relative_dir))?;
    Ok(())
}

fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    hasher.finish()
}

fn upload_plugin(
    archive: &[u8],
    client: &Client,
    last_uploaded_archive_fingerprint: &mut Option<u64>,
) -> anyhow::Result<()> {
    let fingerprint = fingerprint_bytes(archive);
    if last_uploaded_archive_fingerprint.is_some_and(|last| last == fingerprint) {
        println!(
            "{}  Plugin output unchanged, skipping upload",
            style("✓").green().bold()
        );
        return Ok(());
    }

    // Routed through cli-core so a token that expires mid-session is refreshed
    // and retried instead of failing the upload with a 401. Transient 5xx retries live in the client too.
    client.upload_plugin(archive)?;
    *last_uploaded_archive_fingerprint = Some(fingerprint);
    Ok(())
}

#[cfg(test)]
mod tests {
    use anyhow::anyhow;

    use super::*;

    const MANIFEST_V1: &str = r#"
name = "demo"
version = "0.1.0"

[server]
entry = "demo.wasm"
"#;

    const MANIFEST_V2: &str = r#"
name = "demo"
version = "0.1.0"

[server]
entry = "renamed.wasm"

[web]
root = "dist"
entry = "index.js"
"#;

    fn snapshot(content: &str) -> ManifestSnapshot {
        ManifestSnapshot::parse(content).unwrap()
    }

    #[test]
    fn failed_build_is_retried_for_the_same_manifest() {
        let dir = Path::new("/plugin");
        let mut state = WatchState::new(dir, &snapshot(MANIFEST_V1).manifest);
        let v1 = snapshot(MANIFEST_V1).fingerprint;

        assert!(
            state
                .finish_build(dir, Err(anyhow!("build failed")))
                .is_err()
        );
        assert!(!state.manifest_is_built(Some(v1)));

        state.finish_build(dir, Ok(snapshot(MANIFEST_V1))).unwrap();
        assert!(state.manifest_is_built(Some(v1)));
    }

    #[test]
    fn failed_rebuild_keeps_the_last_successful_manifest() {
        let dir = Path::new("/plugin");
        let mut state = WatchState::new(dir, &snapshot(MANIFEST_V1).manifest);
        state.finish_build(dir, Ok(snapshot(MANIFEST_V1))).unwrap();
        let v2 = snapshot(MANIFEST_V2).fingerprint;

        assert!(
            state
                .finish_build(dir, Err(anyhow!("build failed")))
                .is_err()
        );
        assert!(!state.manifest_is_built(Some(v2)));
        assert_eq!(state.server_entry, Some(dir.join("demo.wasm")));
        assert_eq!(state.web_root, None);

        state.finish_build(dir, Ok(snapshot(MANIFEST_V2))).unwrap();
        assert!(state.manifest_is_built(Some(v2)));
        assert_eq!(state.server_entry, Some(dir.join("renamed.wasm")));
        assert_eq!(state.web_root, Some(dir.join("dist")));
    }

    #[test]
    fn unreadable_manifest_is_never_built() {
        let dir = Path::new("/plugin");
        let mut state = WatchState::new(dir, &snapshot(MANIFEST_V1).manifest);
        state.finish_build(dir, Ok(snapshot(MANIFEST_V1))).unwrap();

        assert!(!state.manifest_is_built(None));
    }
}
