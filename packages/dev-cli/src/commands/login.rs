use std::io::Write;
use std::thread;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Args;
use console::style;

use broccoli_cli_core::api::auth::DeviceCodeResponse;
use broccoli_cli_core::client::{Client, PollResponse, persist_session};
use broccoli_cli_core::tls;

#[derive(Args)]
pub struct LoginArgs {
    #[arg(long, default_value = "http://localhost:3000", env = "BROCCOLI_URL")]
    pub server: String,
}

pub fn run(args: LoginArgs) -> anyhow::Result<()> {
    // Non-2xx must come back as Ok so we can read the RFC-8628-style poll
    // errors (authorization_pending / slow_down / expired_token) the server
    // sends with HTTP 400; tls::build_agent also disables status-as-error.
    let agent = tls::build_agent(Some(Duration::from_secs(10)), Some(Duration::from_secs(30)));

    println!(
        "{}  Requesting device code from {}...",
        style("→").blue().bold(),
        style(&args.server).cyan()
    );

    let client = Client::anonymous(&args.server);

    let device_code_resp: DeviceCodeResponse = client
        .request_device_code()
        .context("Failed to connect to server. Is it running?")?;

    println!();
    println!(
        "  Open {} and enter code:",
        style(&device_code_resp.verification_url)
            .underlined()
            .cyan()
    );
    println!();
    println!("    {}", style(&device_code_resp.user_code).bold().yellow());
    println!();

    let _ = open::that(&device_code_resp.verification_url);

    let interval = Duration::from_secs(device_code_resp.interval);
    let max_polls = device_code_resp.expires_in / device_code_resp.interval;

    print!("  Waiting for authorization");
    std::io::stdout().flush().ok();

    for _ in 0..max_polls {
        thread::sleep(interval);
        print!(".");
        std::io::stdout().flush().ok();

        let poll_resp = agent
            .post(&format!("{}/api/v1/auth/device-token", args.server))
            .send_json(serde_json::json!({
                "device_code": device_code_resp.device_code
            }));

        let poll_resp = match poll_resp {
            Ok(r) => r,
            Err(e) => {
                eprintln!("\n  Connection error: {}. Retrying...", e);
                continue;
            }
        };

        let poll: PollResponse = match poll_resp.into_body().read_json() {
            Ok(p) => p,
            Err(_) => continue,
        };

        if let Some(token) = poll.token {
            println!();
            println!();

            persist_session(&args.server, &token)?;

            println!("{}  Logged in successfully!", style("✓").green().bold());
            println!(
                "   Credentials saved to {}",
                style("~/.config/broccoli/credentials.json").dim()
            );

            return Ok(());
        }

        if let Some(ref error) = poll.error {
            match error.as_str() {
                "authorization_pending" => continue,
                "slow_down" => {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
                "expired_token" => {
                    println!();
                    bail!("Device code expired. Run `broccoli-dev login` again to get a new code.");
                }
                other => {
                    println!();
                    bail!("Unexpected error from server: {}", other);
                }
            }
        }
    }

    println!();
    bail!("Timed out waiting for authorization. Run `broccoli-dev login` again.");
}
