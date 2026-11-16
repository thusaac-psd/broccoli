use std::io::Write;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use clap::Args;
use console::style;

use broccoli_cli_core::api::auth::DeviceCodeResponse;
use broccoli_cli_core::backoff;
use broccoli_cli_core::client::{Client, PollResponse, persist_session};
use broccoli_cli_core::config;
use broccoli_cli_core::credentials::CredentialsFile;
use broccoli_cli_core::server::ServerUrl;

#[derive(Args)]
pub struct LoginArgs {
    /// Server to sign in to. Falls back to $BROCCOLI_URL, the server signed in
    /// to last, `server` in config.toml, then http://localhost:3000.
    #[arg(long, env = "BROCCOLI_URL")]
    pub server: Option<ServerUrl>,

    /// Save this access token (for example one copied from the web UI)
    /// instead of signing in through the browser
    #[arg(long, env = "BROCCOLI_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
}

pub fn run(args: LoginArgs) -> anyhow::Result<()> {
    let server = config::login_server(args.server.as_ref())?;
    if let Some(token) = args.token {
        persist_session(&server, &token)?;
        print_saved(&server);
        return Ok(());
    }
    println!(
        "{}  Requesting device code from {}...",
        style("→").blue().bold(),
        style(&server).cyan()
    );

    let client = Client::anonymous(&server);

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

    let mut poll = DevicePoll::new(&device_code_resp, Instant::now());

    print!("  Waiting for authorization");
    std::io::stdout().flush().ok();

    while let Some(wait) = poll.next_wait(Instant::now()) {
        thread::sleep(wait);
        print!(".");
        std::io::stdout().flush().ok();

        let response = match client.poll_device_token(&device_code_resp.device_code) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("\n  {e}. Retrying...");
                poll.on_network_error();
                continue;
            }
        };

        match poll.on_response(response) {
            Step::Continue => {}
            Step::Authorized(token) => {
                println!();
                println!();

                persist_session(&server, &token)?;

                print_saved(&server);
                return Ok(());
            }
            Step::Expired => {
                println!();
                bail!("Device code expired. Run `broccoli-dev login` again to get a new code.");
            }
            Step::Denied => {
                println!();
                bail!("Authorization was denied. Run `broccoli-dev login` again.");
            }
            Step::Unexpected(error) => {
                println!();
                bail!("Unexpected error from server: {}", error);
            }
        }
    }

    println!();
    bail!("Timed out waiting for authorization. Run `broccoli-dev login` again.");
}

fn print_saved(server: &ServerUrl) {
    println!(
        "{}  Logged in to {}",
        style("✓").green().bold(),
        style(server).cyan()
    );
    println!(
        "   Credentials saved to {}",
        style(CredentialsFile::default_location().path().display()).dim()
    );
}

/// RFC 8628 3.2: the interval to use when the server gives none.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);
/// Longest wait between polls while the server cannot be reached.
const MAX_INTERVAL: Duration = Duration::from_secs(60);
/// RFC 8628 3.5: each `slow_down` raises the interval by five seconds for
/// every later poll.
const SLOW_DOWN_STEP: Duration = Duration::from_secs(5);

/// Pacing of one device-code poll loop.
struct DevicePoll {
    interval: Duration,
    deadline: Instant,
    /// Consecutive polls that could not reach the server.
    network_errors: u32,
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Continue,
    Authorized(String),
    Expired,
    Denied,
    Unexpected(String),
}

impl DevicePoll {
    fn new(code: &DeviceCodeResponse, now: Instant) -> Self {
        let interval = match code.interval {
            0 => DEFAULT_INTERVAL,
            secs => Duration::from_secs(secs),
        };
        Self {
            interval,
            deadline: now + Duration::from_secs(code.expires_in),
            network_errors: 0,
        }
    }

    /// How long to wait before the next poll, or `None` once the code has
    /// expired.
    fn next_wait(&self, now: Instant) -> Option<Duration> {
        let wait = match self.network_errors {
            0 => self.interval,
            n => backoff::exponential(self.interval, n, MAX_INTERVAL.max(self.interval)),
        };
        (now + wait <= self.deadline).then_some(wait)
    }

    /// RFC 8628 3.5: back off exponentially while the server cannot be reached.
    fn on_network_error(&mut self) {
        self.network_errors += 1;
    }

    fn on_response(&mut self, response: PollResponse) -> Step {
        self.network_errors = 0;
        if let Some(token) = response.token {
            return Step::Authorized(token);
        }
        match response.error.as_deref() {
            Some("authorization_pending") => Step::Continue,
            Some("slow_down") => {
                self.interval += SLOW_DOWN_STEP;
                Step::Continue
            }
            Some("expired_token") => Step::Expired,
            Some("access_denied") => Step::Denied,
            other => Step::Unexpected(other.unwrap_or("no token and no error").to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(interval: u64, expires_in: u64) -> DeviceCodeResponse {
        DeviceCodeResponse {
            device_code: "d".into(),
            user_code: "U".into(),
            verification_url: "http://x/auth/device".into(),
            expires_in,
            interval,
        }
    }

    fn error(e: &str) -> PollResponse {
        PollResponse {
            error: Some(e.into()),
            token: None,
        }
    }

    #[test]
    fn slow_down_raises_the_interval_for_every_later_poll() {
        let t = Instant::now();
        let mut poll = DevicePoll::new(&code(5, 900), t);
        assert_eq!(poll.on_response(error("slow_down")), Step::Continue);
        assert_eq!(poll.next_wait(t), Some(Duration::from_secs(10)));
        assert_eq!(
            poll.on_response(error("authorization_pending")),
            Step::Continue
        );
        assert_eq!(poll.next_wait(t), Some(Duration::from_secs(10)));
        poll.on_response(error("slow_down"));
        assert_eq!(poll.next_wait(t), Some(Duration::from_secs(15)));
    }

    #[test]
    fn a_zero_interval_polls_at_the_default_until_the_code_expires() {
        let t = Instant::now();
        let poll = DevicePoll::new(&code(0, 12), t);
        assert_eq!(poll.next_wait(t), Some(DEFAULT_INTERVAL));
        assert_eq!(
            poll.next_wait(t + Duration::from_secs(7)),
            Some(DEFAULT_INTERVAL)
        );
        assert_eq!(poll.next_wait(t + Duration::from_millis(7001)), None);
    }

    #[test]
    fn network_errors_back_off_until_the_server_answers() {
        let t = Instant::now();
        let mut poll = DevicePoll::new(&code(5, 900), t);
        poll.on_network_error();
        assert_eq!(poll.next_wait(t), Some(Duration::from_secs(10)));
        poll.on_network_error();
        assert_eq!(poll.next_wait(t), Some(Duration::from_secs(20)));
        poll.on_response(error("authorization_pending"));
        assert_eq!(poll.next_wait(t), Some(Duration::from_secs(5)));
    }

    #[test]
    fn terminal_responses_end_the_loop() {
        let mut poll = DevicePoll::new(&code(5, 900), Instant::now());
        let token = PollResponse {
            error: None,
            token: Some("jwt".into()),
        };
        assert_eq!(poll.on_response(token), Step::Authorized("jwt".into()));
        assert_eq!(poll.on_response(error("expired_token")), Step::Expired);
        assert_eq!(poll.on_response(error("access_denied")), Step::Denied);
    }
}
