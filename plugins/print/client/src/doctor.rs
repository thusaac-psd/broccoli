//! `print-client doctor`. Checks each server and printer, optionally test-prints.

use std::io::{self, Write};

use anyhow::Result;
use dialoguer::Confirm;

use crate::api::ServerClient;
use crate::config::Config;
use crate::print;
use crate::render::{self, DocMeta, RenderConfig};

const TEST_SOURCE: &str = r#"// broccoli print-client test page
#include <iostream>
int main() {
    std::cout << "If you can read this, printing works!" << std::endl;
    for (int i = 0; i < 3; i++) std::cout << i << " ";
    return 0;
}
"#;

pub fn run_doctor(cfg: &Config) -> Result<()> {
    println!("\n  print-client doctor\n  ───────────────────");
    println!("  station: {}", cfg.station);
    if let Some(loc) = &cfg.location {
        println!("  location: {loc}");
    }

    println!("\n  Servers:");
    if cfg.servers.is_empty() {
        println!("    ! none configured");
    }
    let printer_names = cfg.printer_names();
    for server in &cfg.servers {
        print!("    Checking {} ... ", server.url);
        io::stdout().flush().ok();
        let client = ServerClient::new(server);
        let result = client.heartbeat(&printer_names, cfg.location.as_deref(), 0);
        // Wipe the loading line before printing the result.
        print!("\r\x1b[K");
        match result {
            Ok(()) => println!("    ✓ {} — reachable, token accepted", server.url),
            Err(e) => println!("    ✗ {} — {e}", server.url),
        }
    }

    println!("\n  Printers:");
    let detected = print::enumerate_printers();
    if cfg.printers.is_empty() {
        println!("    ! none configured");
    }
    for printer in &cfg.printers {
        let how = if let Some(cmd) = &printer.command {
            format!("command `{cmd}`")
        } else if let Some(os_id) = &printer.os_id {
            let known = detected.iter().any(|d| d == os_id);
            format!(
                "OS queue '{os_id}'{}",
                if known {
                    ""
                } else {
                    " (not found by lpstat/Get-Printer!)"
                }
            )
        } else {
            "system default".to_string()
        };
        println!("    • {} → {how}", printer.name);
    }
    if let Some(status) = print::silent_helper_status() {
        println!("    • silent print helper: {status}");
    }

    let send_test = !cfg.printers.is_empty()
        && Confirm::new()
            .with_prompt("\n  Send a test page to each printer?")
            .default(false)
            .interact()
            .unwrap_or(false);
    if send_test {
        let rendered = render::render(
            TEST_SOURCE,
            "cpp",
            &DocMeta {
                banner: cfg.banner.clone(),
                problem_label: Some("TEST".into()),
                who: cfg.station.clone(),
                filename: "test-page.cpp".into(),
                when: String::new(),
                job_id: 0,
            },
            &RenderConfig {
                font_size: cfg.font_size,
                paper: cfg.paper.clone(),
            },
        )?;
        let path = std::env::temp_dir().join("broccoli-print-test.pdf");
        std::fs::write(&path, &rendered.bytes)?;
        for printer in &cfg.printers {
            match print::print_pdf(printer, &path) {
                Ok(()) => println!("    ✓ test page sent to '{}'", printer.name),
                Err(e) => println!("    ✗ '{}' — {e}", printer.name),
            }
        }
        let _ = std::fs::remove_file(&path);
    }

    println!();
    Ok(())
}
