//! broccoli print-client. Polls servers for jobs, renders highlighted PDFs,
//! and prints them at a station.

// A print station stamps job sheets with its own local time.
#![allow(clippy::disallowed_methods)]

mod api;
mod config;
mod doctor;
mod print;
mod render;
mod run;
mod setup;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

use config::Config;
use render::{DocMeta, RenderConfig};

#[derive(Parser)]
#[command(
    name = "print-client",
    version,
    about = "broccoli contest print station client"
)]
struct Cli {
    /// Path to print-client.toml (default: ./print-client.toml or $PRINT_CLIENT_CONFIG)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Interactive setup wizard (detects printers, writes config).
    Setup,
    /// Poll servers and print jobs.
    Run {
        /// Run a single poll pass, then exit.
        #[arg(long)]
        once: bool,
    },
    /// Verify connectivity and printers, optionally send a test page.
    Doctor,
    /// Render (and optionally print) a local file to verify output.
    TestPrint {
        /// File to render.
        file: PathBuf,
        /// Printer name from the config (defaults to the first).
        #[arg(long)]
        printer: Option<String>,
        /// Write the rendered PDF here instead of printing it.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn infer_language(path: &std::path::Path) -> String {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "cpp" | "cc" | "cxx" | "hpp" | "h" => "cpp",
        "c" => "c",
        "py" => "python3",
        "java" => "java",
        "js" | "mjs" => "javascript",
        "ts" => "typescript",
        "rs" => "rust",
        "go" => "go",
        "kt" => "kotlin",
        _ => "text",
    }
    .to_string()
}

fn test_print(
    cfg: &Config,
    file: &std::path::Path,
    printer: Option<String>,
    out: Option<PathBuf>,
) -> Result<()> {
    let source =
        std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
    let filename = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file.txt".to_string());

    let rendered = render::render(
        &source,
        &infer_language(file),
        &DocMeta {
            banner: cfg.banner.clone(),
            problem_label: None,
            who: cfg.station.clone(),
            filename,
            when: chrono::Local::now().format("%m-%d %H:%M").to_string(),
            job_id: 0,
        },
        &RenderConfig {
            font_size: cfg.font_size,
            paper: cfg.paper.clone(),
        },
    )?;
    println!("rendered {} page(s)", rendered.pages);

    if let Some(out) = out {
        std::fs::write(&out, &rendered.bytes)?;
        println!("wrote {}", out.display());
        return Ok(());
    }

    let chosen = match printer {
        Some(name) => cfg.printers.iter().find(|p| p.name == name),
        None => cfg.printers.first(),
    }
    .with_context(|| "no matching printer in config")?;

    let path = std::env::temp_dir().join("broccoli-print-test-local.pdf");
    std::fs::write(&path, &rendered.bytes)?;
    print::print_pdf(chosen, &path)?;
    let _ = std::fs::remove_file(&path);
    println!("sent to printer '{}'", chosen.name);
    Ok(())
}

fn load_config_or_exit(path: &std::path::Path) -> Result<Config> {
    if !path.exists() {
        eprintln!(
            "No config file found at '{}'.\n\
             \n  Run this first:\n\
             \n    print-client setup\n\
             \n  Or create it manually — see the README for the format.\n",
            path.display()
        );
        std::process::exit(1);
    }
    Config::load(path)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(config::default_config_path);

    // Surface the active config path, but setup may still be creating it.
    if !matches!(cli.command, Commands::Setup) {
        let _ = load_config_or_exit(&path)?;
        let display = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        eprintln!("config: {}", display.display());
    }

    match cli.command {
        Commands::Setup => setup::run_setup(&path),
        Commands::Run { once } => run::run(&Config::load(&path)?, once),
        Commands::Doctor => doctor::run_doctor(&Config::load(&path)?),
        Commands::TestPrint { file, printer, out } => {
            test_print(&Config::load(&path)?, &file, printer, out)
        }
    }
}
