pub mod build;
pub mod new;
mod wasm;
pub mod watch;

use std::path::Path;

use anyhow::Context;
use broccoli_types::manifest::PluginManifest;
use clap::{Args, Subcommand};

use self::build::BuildPluginArgs;
use self::new::NewPluginArgs;
use self::watch::WatchPluginArgs;

#[derive(Args)]
pub struct PluginArgs {
    #[command(subcommand)]
    pub command: PluginCommand,
}

#[derive(Subcommand)]
pub enum PluginCommand {
    /// Scaffold a new plugin
    New(NewPluginArgs),
    /// Build a plugin's backend (WASM) and frontend in place
    Build(BuildPluginArgs),
    /// Rebuild on every edit and upload the plugin to a server
    Watch(WatchPluginArgs),
}

/// The error for a directory without a `plugin.toml`.
fn not_a_plugin_dir(dir: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "Not a broccoli plugin directory: no plugin.toml found in '{}'.\n\
         Run `broccoli-dev plugin new <name>` to create a new plugin.",
        dir.display()
    )
}

fn read_manifest(path: &Path) -> anyhow::Result<PluginManifest> {
    let content = std::fs::read_to_string(path).context("Failed to read plugin.toml")?;
    parse_manifest(&content)
}

fn parse_manifest(content: &str) -> anyhow::Result<PluginManifest> {
    toml::from_str(content).context("Failed to parse plugin.toml")
}
