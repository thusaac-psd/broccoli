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

/// Server URL from saved credentials, then $BROCCOLI_URL, then config, else default.
fn resolve_server() -> String {
    if let Ok(creds) = config::resolve_credentials(None, None) {
        return creds.server().to_string();
    }
    if let Ok(url) = std::env::var("BROCCOLI_URL")
        && !url.trim().is_empty()
    {
        return url;
    }
    if let Some(server) = config::load_user_config().server {
        return server;
    }
    "http://localhost:3000".to_string()
}
