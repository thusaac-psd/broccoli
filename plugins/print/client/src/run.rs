//! The poll, claim, render, print, report loop across every server and printer.
//! The server's atomic claim serializes concurrent stations.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Result, bail};

use crate::api::{ClaimOutcome, Job, ServerClient};
use crate::config::{Config, PrinterCfg};
use crate::render::{self, DocMeta, RenderConfig};

fn printer_matches(printer: &PrinterCfg, job: &Job) -> bool {
    match job.target_printer.as_deref() {
        None | Some("") => true,
        Some(target) => target == printer.name || printer.os_id.as_deref() == Some(target),
    }
}

fn format_when(epoch: Option<f64>) -> String {
    match epoch {
        Some(secs) => {
            let dt = chrono::DateTime::from_timestamp(secs as i64, 0)
                .map(|dt| dt.with_timezone(&chrono::Local))
                .unwrap_or_default();
            dt.format("%m-%d %H:%M").to_string()
        }
        None => String::new(),
    }
}

fn render_job(cfg: &Config, job: &Job) -> Result<render::Rendered> {
    let meta = DocMeta {
        banner: cfg.banner.clone(),
        problem_label: job.problem_label.clone(),
        who: job
            .display_name
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| job.username.clone()),
        filename: if job.filename.is_empty() {
            "print.txt".to_string()
        } else {
            job.filename.clone()
        },
        when: format_when(job.created_at),
        job_id: job.id,
    };
    render::render(
        &job.source,
        &job.language,
        &meta,
        &RenderConfig {
            font_size: cfg.font_size,
            paper: cfg.paper.clone(),
        },
    )
}

fn temp_pdf_path(job: &Job) -> std::path::PathBuf {
    let safe: String = job
        .filename
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    std::env::temp_dir().join(format!("broccoli-print-{}-{}.pdf", job.id, safe))
}

fn process_job(client: &ServerClient, cfg: &Config, printer: &PrinterCfg, job: &Job) {
    println!(
        "  ▶ job #{} ({}, {}) → printer '{}'",
        job.id, job.username, job.filename, printer.name
    );
    let _ = client.report(job.id, "printing", None, None);

    let rendered = match render_job(cfg, job) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("    ✗ render failed: {e}");
            let _ = client.report(job.id, "failed", None, Some(&format!("render error: {e}")));
            return;
        }
    };

    if rendered.pages as u32 > cfg.max_pages {
        let msg = format!("{} pages exceeds limit {}", rendered.pages, cfg.max_pages);
        eprintln!("    ✗ {msg}");
        let _ = client.report(job.id, "failed", Some(rendered.pages as u32), Some(&msg));
        return;
    }

    let path = temp_pdf_path(job);
    if let Err(e) = std::fs::write(&path, &rendered.bytes) {
        eprintln!("    ✗ could not write temp PDF: {e}");
        let _ = client.report(job.id, "failed", None, Some("temp file error"));
        return;
    }

    match crate::print::print_pdf(printer, &path) {
        Ok(()) => {
            println!("    ✓ printed ({} page(s))", rendered.pages);
            let _ = client.report(job.id, "done", Some(rendered.pages as u32), None);
        }
        Err(e) => {
            eprintln!("    ✗ print failed: {e}");
            let _ = client.report(
                job.id,
                "failed",
                Some(rendered.pages as u32),
                Some(&e.to_string()),
            );
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn tick_server(client: &ServerClient, cfg: &Config, printer_names: &[String]) {
    if let Err(e) = client.heartbeat(printer_names, cfg.location.as_deref(), 0) {
        eprintln!("  ! heartbeat to {} failed: {e}", client.label);
    }

    let limit = (cfg.printers.len() * 3).max(4);
    let jobs = match client.fetch_jobs(cfg.location.as_deref(), limit) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("  ! fetch from {} failed: {e}", client.label);
            return;
        }
    };
    if jobs.is_empty() {
        return;
    }

    // Claim one job per printer first, since claims are just quick HTTP calls.
    let mut claims: Vec<(PrinterCfg, Job)> = Vec::new();
    let mut handled: HashSet<i64> = HashSet::new();
    for printer in &cfg.printers {
        for job in &jobs {
            if handled.contains(&job.id) || !printer_matches(printer, job) {
                continue;
            }
            match client.claim(job.id, &printer.name) {
                Ok(ClaimOutcome::Claimed) => {
                    handled.insert(job.id);
                    claims.push((printer.clone(), job.clone()));
                    break; // one job per printer per tick
                }
                Ok(ClaimOutcome::Taken) => {
                    handled.insert(job.id);
                }
                Err(e) => {
                    eprintln!("  ! claim #{} failed: {e}", job.id);
                }
            }
        }
    }

    // Render and print every claimed job in parallel, one thread per printer.
    if claims.is_empty() {
        return;
    }
    let cfg = Arc::new(cfg.clone());
    let client = Arc::new(client.clone());
    let handles: Vec<std::thread::JoinHandle<()>> = claims
        .into_iter()
        .map(|(printer, job)| {
            let cfg = Arc::clone(&cfg);
            let client = Arc::clone(&client);
            std::thread::spawn(move || {
                process_job(&client, &cfg, &printer, &job);
            })
        })
        .collect();
    for h in handles {
        let _ = h.join();
    }
}

fn sleep_interruptible(secs: u64, running: &AtomicBool) {
    // Deterministic jitter keeps many stations from polling in lockstep.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let base_ms = secs.max(1) * 1000;
    let jitter = (nanos as u64 % (base_ms / 3 + 1)) as i64 - (base_ms / 6) as i64;
    let total_ms = (base_ms as i64 + jitter).max(250) as u64;

    let mut slept = 0;
    while slept < total_ms && running.load(Ordering::Relaxed) {
        let step = (total_ms - slept).min(200);
        std::thread::sleep(Duration::from_millis(step));
        slept += step;
    }
}

pub fn run(cfg: &Config, once: bool) -> Result<()> {
    if cfg.servers.is_empty() {
        bail!("no [[server]] configured — run `print-client setup` first");
    }
    if cfg.printers.is_empty() {
        bail!("no [[printer]] configured — run `print-client setup` first");
    }

    let running = Arc::new(AtomicBool::new(true));
    {
        let r = running.clone();
        let _ = ctrlc::set_handler(move || {
            eprintln!("\nshutting down…");
            r.store(false, Ordering::Relaxed);
        });
    }

    let clients: Vec<ServerClient> = cfg.servers.iter().map(ServerClient::new).collect();
    let printer_names = cfg.printer_names();

    println!(
        "print-client: station '{}' serving {} printer(s) across {} server(s){}",
        cfg.station,
        cfg.printers.len(),
        clients.len(),
        if once { " (single pass)" } else { "" }
    );

    loop {
        for client in &clients {
            if !running.load(Ordering::Relaxed) {
                break;
            }
            tick_server(client, cfg, &printer_names);
        }
        if once || !running.load(Ordering::Relaxed) {
            break;
        }
        sleep_interruptible(cfg.poll_interval_secs, &running);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(target: Option<&str>) -> Job {
        Job {
            id: 1,
            contest_id: None,
            username: "u".into(),
            display_name: None,
            problem_label: None,
            language: "text".into(),
            filename: "a.txt".into(),
            source: "x".into(),
            location: None,
            target_printer: target.map(String::from),
            created_at: None,
        }
    }

    fn printer(name: &str, os_id: Option<&str>) -> PrinterCfg {
        PrinterCfg {
            name: name.into(),
            os_id: os_id.map(String::from),
            command: None,
        }
    }

    #[test]
    fn untargeted_jobs_match_any_printer() {
        assert!(printer_matches(&printer("main", None), &job(None)));
        assert!(printer_matches(&printer("main", None), &job(Some(""))));
    }

    #[test]
    fn targeted_jobs_match_by_name_or_os_id() {
        assert!(printer_matches(
            &printer("main", Some("HP_1")),
            &job(Some("main"))
        ));
        assert!(printer_matches(
            &printer("main", Some("HP_1")),
            &job(Some("HP_1"))
        ));
        assert!(!printer_matches(
            &printer("main", Some("HP_1")),
            &job(Some("other"))
        ));
    }

    #[test]
    fn formats_epoch_to_local_time() {
        let s = format_when(Some(3661.0));
        assert!(s.contains(':'), "expected time with colon, got '{s}'");
        assert!(!s.contains("UTC"), "expected local time, got '{s}'");
        assert_eq!(format_when(None), "");
    }
}
