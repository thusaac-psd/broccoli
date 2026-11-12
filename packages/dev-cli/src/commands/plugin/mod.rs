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
    New(NewPluginArgs),
    Build(BuildPluginArgs),
    Watch(WatchPluginArgs),
}

fn read_manifest(path: &Path) -> anyhow::Result<PluginManifest> {
    let content = std::fs::read_to_string(path).context("Failed to read plugin.toml")?;
    toml::from_str(&content).context("Failed to parse plugin.toml")
}
