use std::io::Write;
use std::time::{Duration, Instant};

use console::style;

use broccoli_cli_core::config;

/// Best-effort: warm the binary page cache and prime DNS+TCP+TLS to the server.
pub fn run() -> anyhow::Result<()> {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::read(&exe);
    }

    let server = resolve_server();
    print!(
        "{}  Warming up {} … ",
        style("→").blue().bold(),
        style(&server).cyan()
    );
    std::io::stdout().flush().ok();

    // any HTTP response means the path is warm; only a transport error is unreachable
    let agent = broccoli_cli_core::tls::build_agent(
        Some(Duration::from_secs(4)),
        Some(Duration::from_secs(8)),
    );

    let start = Instant::now();
    let reachable = agent.get(&server).call().is_ok();
    let ms = start.elapsed().as_millis();

    if reachable {
        println!("{} ({} ms)", style("ready").green().bold(), ms);
    } else {
        println!(
            "{} — couldn't reach the server (binary cache still warmed)",
            style("offline").yellow()
        );
    }
    Ok(())
}

/// The server [`config::login_server`] picks.
fn resolve_server() -> String {
    config::login_server(None)
        .map(|server| server.join("/"))
        .unwrap_or_else(|_| "http://localhost:3000/".into())
}
