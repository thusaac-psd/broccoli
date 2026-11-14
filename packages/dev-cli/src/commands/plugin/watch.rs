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
use notify::event::ModifyKind;
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::dev_config::{self, FileKind, ResolvedDevConfig};
use broccoli_cli_core::client::Client;
use broccoli_cli_core::config;
use broccoli_cli_core::server::ServerUrl;

use super::read_manifest;
use super::wasm::copy_wasm_artifact;

#[derive(Args)]
pub struct WatchPluginArgs {
    /// Plugin directory (the one holding plugin.toml)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Server to upload to. Defaults to the server you last logged in to,
    /// then `server` in config.toml
    #[arg(long, env = "BROCCOLI_URL")]
    pub server: Option<ServerUrl>,

    /// Access token to use instead of the saved login (never refreshed)
    #[arg(long, env = "BROCCOLI_TOKEN", hide_env_values = true)]
    pub token: Option<String>,

    /// Run the frontend install command (`pnpm install`) even when
    /// node_modules exists; it runs anyway when node_modules is missing
    #[arg(long)]
    pub install: bool,

    /// Build an optimized WASM module
    #[arg(long)]
    pub release: bool,

    /// Milliseconds to wait for edits to settle before rebuilding
    #[arg(long, default_value = "500")]
    pub debounce: u64,
}

/// What the watch loop knows about the plugin's last build.
///
/// A build is started only by an edit: an event that changed a file's content
/// (see [`is_content_change`]) outside the build's own outputs. A failed build
/// or upload is therefore not retried until the next edit, and an event on
/// `plugin.toml` whose content equals what the last attempt read is not an
/// edit either, so the reads a build makes can never start another build.
struct WatchState {
    /// Fingerprint of the `plugin.toml` the last build attempt read, whether
    /// the attempt succeeded or not.
    attempted_manifest: Option<u64>,
    /// Paths from the last manifest that parsed.
    web_root: Option<PathBuf>,
    server_entry: Option<PathBuf>,
}

impl WatchState {
    fn new(plugin_dir: &Path, manifest: &PluginManifest) -> Self {
        let mut state = Self {
            attempted_manifest: None,
            web_root: None,
            server_entry: None,
        };
        state.track_paths(plugin_dir, manifest);
        state
    }

    /// Whether `plugin.toml`, now with this fingerprint, differs from what the
    /// last attempt read. An unreadable manifest (`None`) always does, so the
    /// attempt reports why it cannot be read.
    fn manifest_changed(&self, current: Option<u64>) -> bool {
        current.is_none() || current != self.attempted_manifest
    }

    /// Records an attempt that read a manifest with fingerprint `manifest`,
    /// and passes a failure through.
    fn finish_build(
        &mut self,
        plugin_dir: &Path,
        manifest: Option<u64>,
        result: anyhow::Result<PluginManifest>,
    ) -> anyhow::Result<()> {
        self.attempted_manifest = manifest;
        let built = result?;
        self.track_paths(plugin_dir, &built);
        Ok(())
    }

    fn track_paths(&mut self, plugin_dir: &Path, manifest: &PluginManifest) {
        self.web_root = manifest.web.as_ref().map(|w| plugin_dir.join(&w.root));
        self.server_entry = manifest.server.as_ref().map(|s| plugin_dir.join(&s.entry));
    }
}

/// Whether a file-system event changed what a file holds. Opening or reading
/// a file (the build reads `plugin.toml`) and metadata changes are not edits.
fn is_content_change(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any)
            | EventKind::Any
    )
}

/// Files the build itself writes in the plugin directory, besides `target/`
/// and the copied server entry: `cargo build` creates or updates the lock
/// file.
const BUILD_OUTPUTS: &[&str] = &["Cargo.lock"];

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
        return Err(super::not_a_plugin_dir(&plugin_dir));
    }

    let creds = config::resolve_credentials(args.server.as_ref(), args.token.as_deref())?;
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

    let manifest_fingerprint = fingerprint_file(&manifest_path);
    let initial = initial_build_and_upload(
        &plugin_dir,
        &manifest_path,
        &client,
        &dev,
        args.release,
        &mut last_uploaded_archive_fingerprint,
    );
    if let Err(e) = state.finish_build(&plugin_dir, manifest_fingerprint, initial) {
        eprintln!("{}  Initial build failed: {}", style("✗").red().bold(), e);
    }

    let debounce = Duration::from_millis(args.debounce);
    let mut last_build = Instant::now();
    let mut pending_changes: HashSet<PathBuf> = HashSet::new();

    loop {
        match rx.recv_timeout(debounce) {
            Ok(event) => {
                if !is_content_change(&event.kind) {
                    continue;
                }
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
                let manifest_fingerprint = fingerprint_file(&manifest_path);
                if let ChangeKind::ManifestChanged = change_kind {
                    if !state.manifest_changed(manifest_fingerprint) {
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
                if let Err(e) = state.finish_build(&plugin_dir, manifest_fingerprint, rebuilt) {
                    eprintln!("{}  Build/upload failed: {}", style("✗").red().bold(), e);
                    eprintln!("   Waiting for the next edit...");
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
                    eprintln!("   Waiting for the next edit...");
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

        if server_entry_abs.is_some_and(|entry| path == entry)
            || BUILD_OUTPUTS.contains(&filename.as_ref())
        {
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
) -> anyhow::Result<PluginManifest> {
    let manifest = read_manifest(manifest_path)?;

    if manifest.server.is_some() {
        build_backend(plugin_dir, release)?;

        if let Some(ref server) = manifest.server {
            copy_wasm_artifact(plugin_dir, &server.entry, release)?;
        }
    }

    if manifest.web.is_some() {
        build_frontend(dev)?;
    }

    let archive = package_plugin(plugin_dir, &manifest)?;
    upload_plugin(&archive, client, last_uploaded_archive_fingerprint)?;

    println!("{}  Plugin uploaded to server", style("✓").green().bold());

    Ok(manifest)
}

fn backend_build_and_upload(
    plugin_dir: &Path,
    manifest_path: &Path,
    client: &Client,
    release: bool,
    last_uploaded_archive_fingerprint: &mut Option<u64>,
) -> anyhow::Result<PluginManifest> {
    let manifest = read_manifest(manifest_path)?;

    if manifest.server.is_some() {
        build_backend(plugin_dir, release)?;

        if let Some(ref server) = manifest.server {
            copy_wasm_artifact(plugin_dir, &server.entry, release)?;
        }
    }

    let archive = package_plugin(plugin_dir, &manifest)?;
    upload_plugin(&archive, client, last_uploaded_archive_fingerprint)?;

    println!("{}  Plugin reloaded on server", style("✓").green().bold());

    Ok(manifest)
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

    let published = client.publish_plugin(archive)?;
    *last_uploaded_archive_fingerprint = Some(fingerprint);
    for config in &published.incompatible_configs {
        let problems: Vec<String> = config
            .errors
            .iter()
            .map(|e| match e.path.as_str() {
                "" => e.message.clone(),
                path => format!("{path}: {}", e.message),
            })
            .collect();
        println!(
            "{}  Stored config {} at {} {} no longer fits the schema: {}",
            style("!").yellow().bold(),
            config.namespace,
            config.scope.as_str(),
            config.ref_id,
            problems.join("; ")
        );
    }
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

    fn manifest(content: &str) -> PluginManifest {
        super::super::parse_manifest(content).unwrap()
    }

    fn fp(content: &str) -> Option<u64> {
        Some(fingerprint_bytes(content.as_bytes()))
    }

    #[test]
    fn a_failed_build_waits_for_the_manifest_to_change() {
        let dir = Path::new("/plugin");
        let mut state = WatchState::new(dir, &manifest(MANIFEST_V1));
        assert!(state.manifest_changed(fp(MANIFEST_V1)));

        let failed = state.finish_build(dir, fp(MANIFEST_V1), Err(anyhow!("upload refused")));
        assert!(failed.is_err());
        assert!(
            !state.manifest_changed(fp(MANIFEST_V1)),
            "the build reading plugin.toml again is not an edit"
        );
        assert!(state.manifest_changed(fp(MANIFEST_V2)));
    }

    #[test]
    fn a_failed_rebuild_keeps_the_paths_of_the_last_manifest_that_parsed() {
        let dir = Path::new("/plugin");
        let mut state = WatchState::new(dir, &manifest(MANIFEST_V1));
        state
            .finish_build(dir, fp(MANIFEST_V1), Ok(manifest(MANIFEST_V1)))
            .unwrap();

        assert!(
            state
                .finish_build(dir, fp(MANIFEST_V2), Err(anyhow!("build failed")))
                .is_err()
        );
        assert_eq!(state.server_entry, Some(dir.join("demo.wasm")));
        assert_eq!(state.web_root, None);

        state
            .finish_build(dir, fp(MANIFEST_V2), Ok(manifest(MANIFEST_V2)))
            .unwrap();
        assert_eq!(state.server_entry, Some(dir.join("renamed.wasm")));
        assert_eq!(state.web_root, Some(dir.join("dist")));
    }

    #[test]
    fn an_unreadable_manifest_is_always_attempted() {
        let dir = Path::new("/plugin");
        let mut state = WatchState::new(dir, &manifest(MANIFEST_V1));
        let _ = state.finish_build(dir, None, Err(anyhow!("cannot read")));
        assert!(state.manifest_changed(None));
    }

    #[test]
    fn only_events_that_change_content_are_edits() {
        use notify::event::{AccessKind, AccessMode, CreateKind, DataChange, MetadataKind};
        assert!(!is_content_change(&EventKind::Access(AccessKind::Open(
            AccessMode::Any
        ))));
        assert!(!is_content_change(&EventKind::Access(AccessKind::Close(
            AccessMode::Read
        ))));
        assert!(!is_content_change(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::Any)
        )));
        assert!(is_content_change(&EventKind::Modify(ModifyKind::Data(
            DataChange::Content
        ))));
        assert!(is_content_change(&EventKind::Create(CreateKind::File)));
    }

    #[test]
    fn files_the_build_writes_are_not_edits() {
        let dir = Path::new("/plugin");
        let entry = dir.join("demo.wasm");
        let changed: HashSet<PathBuf> = [dir.join("Cargo.lock"), entry.clone()].into();
        assert!(classify_changes(&changed, dir, None, Some(&entry), None).is_none());

        let edited: HashSet<PathBuf> = [dir.join("src/lib.rs")].into();
        assert!(matches!(
            classify_changes(&edited, dir, None, Some(&entry), None),
            Some(ChangeKind::Backend)
        ));
    }
}
