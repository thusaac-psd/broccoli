use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Args;
use console::style;

use broccoli_cli_core::client::{Client, ClientError, persist_session};
use broccoli_cli_core::config;
use broccoli_cli_core::server::ServerUrl;
use broccoli_cli_core::session::Session;

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(120);
const SPIN_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Args)]
pub struct LoginArgs {
    /// Broccoli contest server URL. Falls back to $BROCCOLI_URL, the server
    /// signed in to last, `server` in config.toml, then http://localhost:3000.
    #[arg(short, long, env = "BROCCOLI_URL")]
    pub server: Option<ServerUrl>,

    /// Username for direct login.
    #[arg(short, long)]
    pub username: Option<String>,

    /// Password for direct login.
    #[arg(short, long)]
    pub password: Option<String>,

    /// Skip the browser callback and go straight to paste mode.
    #[arg(long)]
    pub no_browser: bool,
}

pub fn run(args: LoginArgs) -> Result<()> {
    let server = config::login_server(args.server.as_ref())?;

    if let (Some(username), Some(password)) = (&args.username, &args.password) {
        return login_direct(&server, username, password);
    }

    let no_browser = args.no_browser || is_headless();

    if !no_browser {
        match login_via_callback(&server) {
            Ok(msg) => return Ok(msg),
            Err(e) => {
                eprintln!("{}", style(format!("Browser login failed: {}", e)).yellow());
                eprintln!("{}", style("Falling back to paste mode...").dim());
            }
        }
    }

    login_via_paste(&server)
}

fn login_direct(server: &ServerUrl, username: &str, password: &str) -> Result<()> {
    let client = Client::anonymous(server);
    let resp = client.login(username, password).context("Login failed")?;

    persist_session(server, &resp.token)?;
    print_logged_in(&resp.username, server);
    Ok(())
}

fn login_via_callback(server: &ServerUrl) -> Result<()> {
    let listener =
        TcpListener::bind("127.0.0.1:0").context("Failed to bind local address for callback")?;
    let local_port = listener.local_addr()?.port();
    // IPv4 literal, not "localhost", so the callback can't resolve to ::1 while we listen on 127.0.0.1
    let callback_url = format!("http://127.0.0.1:{}/callback", local_port);

    // CSRF nonce: listener accepts any localhost connection, so reject callbacks with a mismatched state
    let state = generate_state();

    let auth_url = format!(
        "{}?redirect_uri={}&state={}",
        server.join("/auth/cli"),
        callback_url,
        state,
    );

    if let Err(e) = open::that(&auth_url) {
        bail!(
            "Failed to open browser: {}\n\nOpen this URL manually:\n{}",
            e,
            auth_url
        );
    }

    println!("{} Opened browser for authentication...", style("➜").cyan());
    println!("{} If the browser doesn't open, visit:", style("  ").dim());
    println!("  {}", style(&auth_url).underlined());

    listener
        .set_nonblocking(true)
        .context("Failed to set non-blocking mode")?;

    let start = std::time::Instant::now();
    let mut spinner = broccoli_cli_core::tui::spinner::Spinner::new();
    let mut token: Option<String> = None;

    while start.elapsed() < CALLBACK_TIMEOUT {
        // \x1b[K clears stale trailing chars
        print!(
            "\r\x1b[K{}  Waiting for browser authentication... ({}s)",
            style(spinner.next()).cyan(),
            start.elapsed().as_secs()
        );
        std::io::stdout().flush().ok();

        match listener.accept() {
            Ok((stream, _)) => {
                token = handle_callback_connection(stream, &state);
                if token.is_some() {
                    break;
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(SPIN_INTERVAL);
            }
            Err(e) => {
                bail!("Error accepting callback connection: {}", e);
            }
        }
    }

    print!("\r\x1b[K");
    std::io::stdout().flush().ok();

    let token = match token {
        Some(t) => t,
        None => bail!(
            "Timed out after {} seconds waiting for browser authentication.",
            CALLBACK_TIMEOUT.as_secs()
        ),
    };

    finalize_login(server, &token)
}

/// Random 128-bit hex CSRF nonce.
fn generate_state() -> String {
    let hi: u64 = rand::random();
    let lo: u64 = rand::random();
    format!("{:016x}{:016x}", hi, lo)
}

/// Parsed query params from a `/callback` HTTP request.
struct CallbackParams {
    token: String,
    state: Option<String>,
}

/// Request target from an HTTP request line (the path+query after the method).
fn request_target(request_line: &str) -> Option<&str> {
    let mut parts = request_line.split_whitespace();
    let _method = parts.next()?;
    parts.next()
}

/// Parse `token`/`state` from a `/callback` request line; `None` unless path is `/callback` with a non-empty token.
fn parse_callback_params(request_line: &str) -> Option<CallbackParams> {
    let target = request_target(request_line)?;
    let (path, query) = target.split_once('?')?;
    if path != "/callback" {
        return None;
    }

    let mut token: Option<String> = None;
    let mut state: Option<String> = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        let decoded = urlencoding_decode(value).unwrap_or_else(|| value.to_string());
        match key {
            "token" => token = Some(decoded),
            "state" => state = Some(decoded),
            _ => {}
        }
    }

    let token = token.filter(|t| !t.is_empty())?;
    Some(CallbackParams { token, state })
}

/// Minimal URL-decode (only handles %XX).
fn urlencoding_decode(s: &str) -> Option<String> {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            let hi = chars.next()?.to_digit(16)?;
            let lo = chars.next()?.to_digit(16)?;
            result.push(char::from((hi * 16 + lo) as u8));
        } else {
            result.push(c);
        }
    }
    Some(result)
}

fn handle_callback_connection(mut stream: TcpStream, expected_state: &str) -> Option<String> {
    // bound a slow/malicious client so it can't stall the callback wait
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));

    let mut reader = BufReader::new(&mut stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;

    let params = parse_callback_params(&request_line);

    // reject missing/mismatched CSRF state even if a token is present
    let token = params.and_then(|p| match p.state.as_deref() {
        Some(s) if s == expected_state => Some(p.token),
        _ => None,
    });

    let response = if token.is_some() {
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n\
         <html><body><h1>Authentication successful!</h1>\
         <p>You can close this tab and return to the terminal.</p></body></html>"
    } else {
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\n\r\n\
         <html><body><h1>Authentication failed</h1>\
         <p>Missing token or invalid state in callback URL.</p></body></html>"
    };

    // drain a bounded amount of body so an endless sender can't trap us here
    let mut buf = vec![0u8; 4096];
    let mut drained = 0usize;
    loop {
        match reader.get_mut().read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                drained += n;
                if drained >= 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    stream.write_all(response.as_bytes()).ok()?;
    stream.flush().ok()?;

    token
}

fn login_via_paste(server: &ServerUrl) -> Result<()> {
    let auth_url = server.join("/auth/cli");

    println!(
        "{} Open this URL in your browser to authenticate:",
        style("➜").cyan()
    );
    println!("  {}", style(&auth_url).underlined());
    println!();
    println!("{} Paste the token you receive here:", style("➜").cyan());
    print!("{} ", style("Token:").bold());
    std::io::stdout().flush()?;

    let mut token = String::new();
    std::io::stdin()
        .read_line(&mut token)
        .context("Failed to read token from stdin")?;
    let token = token.trim().to_string();

    if token.is_empty() {
        bail!("No token provided.");
    }

    finalize_login(server, &token)
}

fn finalize_login(server: &ServerUrl, token: &str) -> Result<()> {
    let client = Client::new(Session::explicit(server.clone(), token.to_string()));

    let me = client.me().map_err(|e| match e {
        ClientError::Api(api) if api.rejects_token() => anyhow::anyhow!(
            "{server} did not accept that token. Copy the whole token from the browser page and try again."
        ),
        other => anyhow::Error::new(other).context("Could not check the token with the server"),
    })?;

    persist_session(server, token).context("Failed to save credentials")?;
    print_logged_in(&me.username, server);
    Ok(())
}

fn print_logged_in(username: &str, server: &ServerUrl) {
    println!(
        "{} Logged in as {} (server: {})",
        style("✓").green(),
        style(username).bold(),
        style(server).underlined(),
    );
    println!(
        "  {} {}",
        style("Next:").dim(),
        style("broccoli contest list").cyan()
    );
}

/// True for SSH sessions, which have no browser for the callback flow.
fn is_headless() -> bool {
    std::env::var("SSH_TTY").is_ok() || std::env::var("SSH_CONNECTION").is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_callback_params_token_and_state() {
        let req =
            "GET /callback?token=eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjN9.abc&state=deadbeef HTTP/1.1";
        let params = parse_callback_params(req).expect("should parse");
        assert!(params.token.starts_with("eyJ"));
        assert!(params.token.contains('.'));
        assert_eq!(params.state.as_deref(), Some("deadbeef"));
    }

    #[test]
    fn test_parse_callback_params_state_before_token() {
        let req = "GET /callback?state=abc123&token=mytoken HTTP/1.1";
        let params = parse_callback_params(req).expect("should parse");
        assert_eq!(params.token, "mytoken");
        assert_eq!(params.state.as_deref(), Some("abc123"));
    }

    #[test]
    fn test_parse_callback_params_no_token() {
        let req = "GET /callback?state=abc HTTP/1.1";
        assert!(parse_callback_params(req).is_none());
    }

    #[test]
    fn test_parse_callback_params_empty_token() {
        let req = "GET /callback?token=&state=abc HTTP/1.1";
        assert!(parse_callback_params(req).is_none());
    }

    #[test]
    fn test_parse_callback_params_wrong_path() {
        let req = "GET /other?token=abc&state=xyz HTTP/1.1";
        assert!(parse_callback_params(req).is_none());
    }

    #[test]
    fn test_parse_callback_params_no_query() {
        let req = "GET /callback HTTP/1.1";
        assert!(parse_callback_params(req).is_none());
    }

    #[test]
    fn test_generate_state_is_unique_and_long() {
        let a = generate_state();
        let b = generate_state();
        assert_eq!(a.len(), 32, "128-bit hex nonce");
        assert_ne!(a, b, "nonces should differ");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_urlencoding_decode_simple() {
        assert_eq!(urlencoding_decode("hello").as_deref(), Some("hello"));
    }

    #[test]
    fn test_urlencoding_decode_encoded() {
        assert_eq!(
            urlencoding_decode("hello%20world").as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn test_urlencoding_decode_jwt() {
        let input = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0%2EdozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8w";
        let result = urlencoding_decode(input);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.contains('.'));
        assert!(!r.contains("%2E"));
    }

    #[test]
    fn test_is_headless_no_env_vars() {
        // just verify it doesn't panic
        let _ = is_headless();
    }
}
