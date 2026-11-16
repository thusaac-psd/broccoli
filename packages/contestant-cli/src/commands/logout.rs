use broccoli_cli_core::client::{Client, ClientError};
use broccoli_cli_core::credentials::CredentialsFile;
use broccoli_cli_core::server::ServerUrl;
use console::style;

/// Log out of `--server`, or of the active server: the server ends the
/// session, so no copy of the token pair keeps working, and the pair is
/// removed from this machine. When the server cannot end it, the local entry
/// is removed all the same and the user is told the session may stay valid
/// until it expires.
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
    log_out(&store, &server)?;
    if let Some((next, _)) = store.active() {
        println!("   Still logged in to {next}; `broccoli logout -s {next}` logs out of it too.");
    }
    Ok(())
}

fn log_out(store: &CredentialsFile, server: &ServerUrl) -> anyhow::Result<()> {
    let Some(tokens) = store.read(server) else {
        println!("{}  Not logged in to {}.", style("✓").green(), server);
        return Ok(());
    };
    let ended = Client::anonymous(server).logout(&tokens.refresh_token);
    store.lock()?.remove(server)?;
    match ended {
        // A refresh token the server refuses belongs to no live session.
        Ok(()) => println!(
            "{}  Logged out of {}: the server ended the session.",
            style("✓").green().bold(),
            style(server).underlined(),
        ),
        Err(ClientError::Api(e)) if e.rejects_token() => println!(
            "{}  Logged out of {}: the session had already ended.",
            style("✓").green().bold(),
            style(server).underlined(),
        ),
        Err(e) => println!(
            "{}  Logged out on this machine only: {e}\n   \
             The session on {} may stay valid until it expires.",
            style("!").yellow().bold(),
            style(server).underlined(),
        ),
    }
    println!("   Removed from {}", store.path().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use broccoli_cli_core::credentials::StoredTokens;

    use super::*;

    #[test]
    fn a_server_that_cannot_be_reached_still_loses_the_local_login() {
        // A port that refuses connections: bound, then released.
        let addr = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let server: ServerUrl = format!("http://{addr}").parse().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialsFile::at(dir.path().join("credentials.json"));
        let tokens = StoredTokens {
            token: "access".into(),
            refresh_token: "refresh".into(),
        };
        store.lock().unwrap().save_login(&server, &tokens).unwrap();

        log_out(&store, &server).unwrap();
        assert!(store.read(&server).is_none());
    }
}
