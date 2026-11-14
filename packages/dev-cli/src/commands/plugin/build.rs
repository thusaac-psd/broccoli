use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, bail};
use clap::Args;
use console::style;

use crate::dev_config;

use super::read_manifest;
use super::wasm::copy_wasm_artifact;

#[derive(Args)]
pub struct BuildPluginArgs {
    /// Plugin directory (the one holding plugin.toml)
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Run the frontend install command (`pnpm install`) even when
    /// node_modules exists; it runs anyway when node_modules is missing
    #[arg(long)]
    pub install: bool,

    /// Build an optimized WASM module
    #[arg(long)]
    pub release: bool,
}

pub fn run(args: BuildPluginArgs) -> anyhow::Result<()> {
    let plugin_dir = args
        .path
        .canonicalize()
        .with_context(|| format!("Cannot find directory '{}'", args.path.display()))?;

    let manifest_path = plugin_dir.join("plugin.toml");
    if !manifest_path.exists() {
        return Err(super::not_a_plugin_dir(&plugin_dir));
    }

    let manifest = read_manifest(&manifest_path)?;

    let plugin_name = manifest.name.as_str();
    let mut built_anything = false;

    if let Some(server) = manifest.server.as_ref() {
        println!(
            "{}  Building backend for {}...",
            style("→").blue().bold(),
            style(plugin_name).cyan()
        );

        let mut cargo_args = vec!["build", "--target", "wasm32-wasip1"];
        if args.release {
            cargo_args.push("--release");
        }

        let status = Command::new("cargo")
            .args(&cargo_args)
            .current_dir(&plugin_dir)
            .status()
            .context("Failed to run cargo build. Is Rust installed?")?;

        if !status.success() {
            bail!("Backend build failed");
        }

        copy_wasm_artifact(&plugin_dir, &server.entry, args.release)?;

        println!("{}  Backend build complete", style("✓").green().bold());
        built_anything = true;
    }

    if manifest.web.is_some() {
        let web_root = manifest.web.as_ref().map(|w| w.root.as_str());
        let dev = dev_config::resolve(&plugin_dir, web_root);

        let fe_dir = dev.frontend_dir.unwrap_or_else(|| plugin_dir.clone());

        if !fe_dir.exists() {
            bail!(
                "Frontend directory '{}' does not exist.\n\
                 Check build.frontend_dir in broccoli.dev.toml.",
                fe_dir.display()
            );
        }

        let node_modules_exists = fe_dir.join("node_modules").exists();
        if !node_modules_exists || args.install {
            let install_cmd_str = dev.frontend_install_cmd.join(" ");

            if args.install {
                println!(
                    "{}  Running '{}' in {}...",
                    style("→").blue().bold(),
                    style(&install_cmd_str).cyan(),
                    fe_dir.display()
                );
            } else {
                println!(
                    "{}  node_modules not found. Auto-running '{}'...",
                    style("!").yellow().bold(),
                    style(&install_cmd_str).cyan()
                );
            }

            let (program, cmd_args) = dev
                .frontend_install_cmd
                .split_first()
                .context("frontend_install_cmd is empty in broccoli.dev.toml")?;

            let status = Command::new(program)
                .args(cmd_args)
                .current_dir(&fe_dir)
                .status()
                .with_context(|| format!("Failed to run '{}'", install_cmd_str))?;

            if !status.success() {
                bail!("Frontend installation failed");
            }
            println!("{}  Dependencies installed", style("✓").green().bold());
        }

        println!(
            "{}  Building frontend for {}...",
            style("→").blue().bold(),
            style(plugin_name).cyan()
        );

        let (program, cmd_args) = dev
            .frontend_build_cmd
            .split_first()
            .context("frontend_build_cmd is empty in broccoli.dev.toml")?;

        let status = Command::new(program)
            .args(cmd_args)
            .current_dir(&fe_dir)
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

        println!("{}  Frontend build complete", style("✓").green().bold());
        built_anything = true;
    }

    if !built_anything {
        println!(
            "{}  plugin.toml has no [server] or [web] section — nothing to build.",
            style("!").yellow().bold()
        );
    }

    Ok(())
}
