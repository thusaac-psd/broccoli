use broccoli_cli_core::credentials::CredentialsFile;
use console::style;

/// Forget the saved login for `--server`, or for the active server: the
/// token pair is removed from this machine, so the next person at it cannot
/// use the session.
pub fn run() -> anyhow::Result<()> {
    let store = CredentialsFile::default_location();
    let server = match super::server_option() {
        Some(server) => server.clone(),
        None => match store.active() {
            Some((server, _)) => server,
            None => {
                println!("{}  Not logged in to any server.", style("✓").green());
                return Ok(());
            }
        },
    };
    let removed = store.lock()?.remove(&server)?;
    match removed {
        true => println!(
            "{}  Logged out of {} (removed from {})",
            style("✓").green().bold(),
            style(&server).underlined(),
            store.path().display()
        ),
        false => println!("{}  Not logged in to {}.", style("✓").green(), server),
    }
    if let Some((next, _)) = store.active() {
        println!("   Still logged in to {next}; `broccoli logout -s {next}` forgets it too.");
    }
    Ok(())
}
