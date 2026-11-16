use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

#[tokio::main(flavor = "multi_thread")]
async fn main() -> ExitCode {
    let cli = stress_test::cli::Cli::parse();
    if let Err(e) = cli.validate() {
        eprintln!("error: {e}");
        return ExitCode::from(64);
    }

    // Fault scenarios need their own tracing init because the stress runner
    // installs subscribers tied to its TUI/plain renderer state. Run them
    // through a separate dispatch path with a vanilla fmt subscriber.
    if let Some(stress_test::cli::Command::Fault(args)) = cli.command {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .try_init();
        let code = stress_test::fault::runner::run(args).await;
        return ExitCode::from(code);
    }

    if let Some(stress_test::cli::Command::Perf(args)) = cli.command {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")),
            )
            .try_init();
        return ExitCode::from(stress_test::perf::run(args).await);
    }

    let code = stress_test::runner::run(cli).await;
    ExitCode::from(code)
}
