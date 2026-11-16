use std::time::Duration;

use anyhow::Context;
use server::config::AppConfig;
use server::runtime::ServerRuntime;

// Use jemalloc instead of the system (glibc) allocator. Under concurrent judging
// of large-testcase problems glibc retains freed mmap'd allocations and inflates
// RSS far past the live working set; jemalloc returns memory to the OS
// aggressively, keeping RSS close to the actual in-use set. (See the dep note in
// Cargo.toml; `profiling` feature enables `_RJEM_MALLOC_CONF=prof:true` diagnosis.)
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// `broccoli-server`: the API server, and the one-shot commands that share
/// its configuration. With no command it serves.
#[derive(clap::Parser)]
#[command(
    name = "broccoli-server",
    bin_name = "broccoli-server",
    version,
    about = "The Broccoli API server",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// Print the OpenAPI document and exit. Reads no configuration.
    #[arg(long, exclusive = true)]
    openapi: bool,
    /// Print the plugin group role the server derives for DATABASE and exit,
    /// so an installer can admit the plugin roles in pg_hba.conf. Reads no
    /// configuration.
    #[arg(long, value_name = "DATABASE", exclusive = true)]
    plugin_group_role: Option<String>,
    /// Probe this server's `/healthz` and exit 0 when it answers 2xx.
    #[arg(long, exclusive = true)]
    healthcheck: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Run the server (the default).
    Serve,
    /// Export or import a problem or contest bundle.
    Bundle(server::bundle::cli::BundleArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = <Cli as clap::Parser>::parse();
    if cli.openapi {
        println!("{}", server::openapi_json());
        return Ok(());
    }
    if let Some(database) = &cli.plugin_group_role {
        println!("{}", server::plugin_db::schema::group_role(database));
        return Ok(());
    }

    // Before anything opens sockets or files. See `common::rlimit` for why the
    // default soft limit of 1024 is not enough for a server.
    let nofile = common::rlimit::raise_nofile_limit();

    let app_config = AppConfig::load().context("Failed to load configuration")?;

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("broccoli-server")
        .build()
        .context("Failed to build tokio runtime")?;

    if cli.healthcheck {
        let exit_code = tokio_runtime.block_on(run_healthcheck(&app_config));
        std::process::exit(exit_code);
    }
    match cli.command.unwrap_or(Command::Serve) {
        Command::Bundle(args) => {
            let code = tokio_runtime.block_on(server::bundle::cli::run(&app_config, args));
            std::process::exit(code);
        }
        Command::Serve => {}
    }

    let result = tokio_runtime.block_on(async move {
        let server_runtime = ServerRuntime::build(app_config).await?;
        nofile.log();
        server_runtime.serve().await
    });
    // `serve` has drained HTTP, the dispatcher and plugin calls, released this
    // server's in-flight work and flushed telemetry. Exit without tearing the
    // runtime down: a plugin call that outlived the drain would otherwise
    // panic inside its runtime bridge, turning a normal stop into a crash
    // for the supervisor.
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("{e:?}");
            std::process::exit(1)
        }
    }
}

/// Hits the local `/healthz` endpoint and returns a process exit code:
/// `0` on a 2xx response, `1` on any other status or transport error.
///
/// Used by the Docker `HEALTHCHECK` directive in `Dockerfile.server`. Skips
/// observability initialization so the probe stays fast and silent.
async fn run_healthcheck(app_config: &AppConfig) -> i32 {
    let url = format!("http://127.0.0.1:{}/healthz", app_config.server.port);

    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("healthcheck failed: {e}");
            return 1;
        }
    };

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => 0,
        Ok(resp) => {
            eprintln!("healthcheck failed: status {}", resp.status());
            1
        }
        Err(e) => {
            eprintln!("healthcheck failed: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser, error::ErrorKind};

    use super::{Cli, Command};

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("broccoli-server").chain(args.iter().copied()))
    }

    #[test]
    fn the_command_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_arguments_serve() {
        let cli = parse(&[]).unwrap();
        assert!(cli.command.is_none() && !cli.openapi && !cli.healthcheck);
        assert!(matches!(
            parse(&["serve"]).unwrap().command,
            Some(Command::Serve)
        ));
    }

    #[test]
    fn help_and_version_print_instead_of_serving() {
        assert_eq!(
            parse(&["--help"]).err().map(|e| e.kind()),
            Some(ErrorKind::DisplayHelp)
        );
        assert_eq!(
            parse(&["--version"]).err().map(|e| e.kind()),
            Some(ErrorKind::DisplayVersion)
        );
        assert_eq!(
            parse(&["bundle", "--help"]).err().map(|e| e.kind()),
            Some(ErrorKind::DisplayHelp)
        );
    }

    #[test]
    fn unknown_arguments_are_refused() {
        for args in [&["--frobnicate"][..], &["frobnicate"], &["serve", "extra"]] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_one_shot_flags_take_no_command() {
        assert!(parse(&["--openapi"]).unwrap().openapi);
        assert_eq!(
            parse(&["--plugin-group-role", "broccoli"])
                .unwrap()
                .plugin_group_role
                .as_deref(),
            Some("broccoli")
        );
        assert!(parse(&["--healthcheck", "serve"]).is_err());
        assert!(parse(&["--plugin-group-role"]).is_err());
    }

    #[test]
    fn bundle_parses_its_subcommands() {
        assert!(matches!(
            parse(&["bundle", "import", "--replace-problems"])
                .unwrap()
                .command,
            Some(Command::Bundle(_))
        ));
        assert!(parse(&["bundle", "import", "--wipe-dependents"]).is_err());
    }
}
