use broccoli_cli_core::client::Client;
use console::style;

/// Show who you're logged in as, and against which server.
pub fn run() -> anyhow::Result<()> {
    let creds = crate::commands::credentials()?;
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
