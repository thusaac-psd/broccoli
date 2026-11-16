//! Interactive setup wizard that writes `print-client.toml`.

use std::path::Path;

use anyhow::Result;
use dialoguer::{Confirm, Input, Select};

use crate::config::{Config, PrinterCfg, ServerCfg};
use crate::print;

pub fn run_setup(config_path: &Path) -> Result<()> {
    println!("\n  broccoli print-client setup\n  ───────────────────────────\n");

    let mut cfg = if config_path.exists() {
        Config::load(config_path).unwrap_or_default()
    } else {
        Config::default()
    };

    cfg.servers = collect_servers()?;
    cfg.printers = collect_printers()?;

    cfg.max_pages = Input::new()
        .with_prompt("Max pages per job")
        .default(cfg.max_pages)
        .interact_text()?;
    cfg.banner = Input::new()
        .with_prompt("Header banner (e.g. contest name, optional)")
        .allow_empty(true)
        .with_initial_text(cfg.banner.clone())
        .interact_text()?;

    cfg.save(config_path)?;
    println!("\n  ✓ wrote {}", config_path.display());
    println!("  Next: `print-client doctor` to verify, then `print-client run`.\n");
    Ok(())
}

fn collect_servers() -> Result<Vec<ServerCfg>> {
    let mut servers = Vec::new();
    println!("\n  Servers (broccoli deployments to poll):");
    loop {
        let url: String = Input::new()
            .with_prompt("  Server URL (e.g. http://judge.local:3000)")
            .interact_text()?;
        let token: String = Input::new()
            .with_prompt("  Station token")
            .interact_text()?;
        let contest: String = Input::new()
            .with_prompt("  Contest id, if a contest's Printing section lists the token (blank for a deployment-wide station)")
            .allow_empty(true)
            .validate_with(|s: &String| -> Result<(), &str> {
                match s.trim() {
                    "" => Ok(()),
                    id if id.parse::<i32>().is_ok() => Ok(()),
                    _ => Err("a contest id is a number"),
                }
            })
            .interact_text()?;
        servers.push(ServerCfg {
            url: url.trim().trim_end_matches('/').to_string(),
            token: token.trim().to_string(),
            contest: contest.trim().parse().ok(),
        });
        if !Confirm::new()
            .with_prompt("  Add another server?")
            .default(false)
            .interact()?
        {
            break;
        }
    }
    Ok(servers)
}

fn collect_printers() -> Result<Vec<PrinterCfg>> {
    let detected = print::enumerate_printers();
    if detected.is_empty() {
        println!("\n  No system printers detected (is CUPS / a printer installed?).");
    } else {
        println!("\n  Detected printers: {}", detected.join(", "));
    }

    let mut printers = Vec::new();
    loop {
        let mut options: Vec<String> = detected
            .iter()
            .filter(|d| {
                !printers
                    .iter()
                    .any(|p: &PrinterCfg| p.os_id.as_deref() == Some(d.as_str()))
            })
            .cloned()
            .collect();
        let custom_idx = options.len();
        options.push("Custom command / folder sink…".to_string());
        let done_idx = options.len();
        options.push(if printers.is_empty() {
            "Done (no printers — use a folder sink later)".to_string()
        } else {
            "Done".to_string()
        });

        let choice = Select::new()
            .with_prompt("\n  Add a printer")
            .items(&options)
            .default(0)
            .interact()?;

        if choice == done_idx {
            break;
        } else if choice == custom_idx {
            let name: String = Input::new()
                .with_prompt("    Printer name")
                .interact_text()?;
            let command: String = Input::new()
                .with_prompt("    Command template or folder sink (e.g. `lp -d X {file}` or `folder:/tmp/prints`)")
                .interact_text()?;
            printers.push(PrinterCfg {
                name: name.trim().to_string(),
                os_id: None,
                command: Some(command.trim().to_string()),
            });
        } else {
            let os_id = options[choice].clone();
            let name: String = Input::new()
                .with_prompt("    Logical name for this printer")
                .with_initial_text(os_id.clone())
                .interact_text()?;
            printers.push(PrinterCfg {
                name: name.trim().to_string(),
                os_id: Some(os_id),
                command: None,
            });
        }
    }
    Ok(printers)
}
