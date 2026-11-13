use broccoli_cli_core::client::Client;
use broccoli_cli_core::config;
use console::style;

/// Show who you're logged in as, and against which server.
pub fn run() -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let server = creds.server().to_string();
    let client = Client::new(creds);
    let me = client.me()?;
    println!(
        "{} {} (id {}) on {}",
        style("✓").green().bold(),
        style(&me.username).bold(),
        me.id,
        style(&server).underlined(),
    );
    Ok(())
}
