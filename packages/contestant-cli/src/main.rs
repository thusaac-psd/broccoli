use anyhow::Result;
use clap::{CommandFactory, Parser};

use std::process::ExitCode;

use broccoli_contestant_cli::commands::contest::ContestCommand;
use broccoli_contestant_cli::commands::{Command, GlobalArgs, Rejected};

#[derive(Parser)]
#[command(
    name = "broccoli",
    about = "Broccoli contestant CLI — submit, test, and compete",
    version,
    after_help = "\
EXAMPLES:
  broccoli login                          Log in (opens your browser)
  broccoli contest list                   See available contests
  broccoli contest info [id|name]         Contest details + your status
  broccoli submit sol.cpp -p A -c <id>    Submit problem A
  broccoli test sol.cpp -p A -c <id>      Run the sample cases
  broccoli status                         Pick a recent submission to inspect
  broccoli watch [id|name]                Live contest dashboard
  broccoli logout                         Forget the saved login

Most commands accept a contest by id or name and a problem by label (A), \
position (1), title (\"Two Sum\") or id (#42). Run `broccoli completions <shell>` to enable tab-completion."
)]
struct Cli {
    #[command(flatten)]
    globals: GlobalArgs,

    #[command(subcommand)]
    command: Command,
}

/// Exit codes: 0 on success, 1 on an error, 2 on a usage error (from clap),
/// and [`Rejected::EXIT_CODE`] when a verdict is not Accepted or a sample
/// fails.
fn main() -> ExitCode {
    configure_colors();
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => match e.downcast_ref::<Rejected>() {
            // The command already printed the verdict or the sample results.
            Some(_) => ExitCode::from(Rejected::EXIT_CODE),
            None => {
                eprintln!("Error: {e}");
                for cause in e.chain().skip(1) {
                    eprintln!("  Caused by: {cause}");
                }
                ExitCode::FAILURE
            }
        },
    }
}

fn run(cli: Cli) -> Result<()> {
    broccoli_contestant_cli::commands::init(cli.globals);
    match cli.command {
        Command::Completions(args) => {
            let mut cmd = Cli::command();
            clap_complete::generate(args.shell, &mut cmd, "broccoli", &mut std::io::stdout());
            Ok(())
        }
        Command::Prewarm => broccoli_contestant_cli::commands::prewarm::run(),
        Command::Login(args) => broccoli_contestant_cli::commands::login::run(args),
        Command::Whoami => broccoli_contestant_cli::commands::whoami::run(),
        Command::Logout => broccoli_contestant_cli::commands::logout::run(),
        Command::Submit(args) => broccoli_contestant_cli::commands::submit::run(args),
        Command::Test(args) => broccoli_contestant_cli::commands::test::run(args),
        Command::Status(args) => broccoli_contestant_cli::commands::status::run(args),
        Command::Clarifications(args) => {
            broccoli_contestant_cli::commands::clarifications::run(args)
        }
        Command::Config(args) => broccoli_contestant_cli::commands::config::run(args),
        Command::Watch(args) => broccoli_contestant_cli::commands::watch::run(args),
        Command::Contest(args) => match args.command {
            ContestCommand::List => broccoli_contestant_cli::commands::contest::list::run(),
            ContestCommand::Info(ia) => broccoli_contestant_cli::commands::contest::info::run(ia),
            ContestCommand::Register(ra) => {
                broccoli_contestant_cli::commands::contest::register::run(ra, false)
            }
            ContestCommand::Unregister(ra) => {
                broccoli_contestant_cli::commands::contest::register::run(ra, true)
            }
            ContestCommand::Problems(pa) => {
                broccoli_contestant_cli::commands::contest::problems::run(pa)
            }
        },
    }
}

/// Honor NO_COLOR / FORCE_COLOR / CLICOLOR_FORCE; else `console` auto-detects.
fn configure_colors() {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
    let force = std::env::var_os("FORCE_COLOR").is_some_and(|v| !v.is_empty())
        || std::env::var_os("CLICOLOR_FORCE").is_some_and(|v| !v.is_empty());
    if no_color {
        console::set_colors_enabled(false);
    } else if force {
        console::set_colors_enabled(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn assert_documented(cmd: &clap::Command, path: &str) {
        for arg in cmd.get_arguments() {
            let id = arg.get_id().as_str();
            if matches!(id, "help" | "version") {
                continue;
            }
            assert!(
                arg.get_help().is_some(),
                "`{path}` option `{id}` has no help"
            );
        }
        for sub in cmd.get_subcommands() {
            let path = format!("{path} {}", sub.get_name());
            assert!(sub.get_about().is_some(), "`{path}` has no help");
            assert_documented(sub, &path);
        }
    }

    #[test]
    fn every_command_and_option_has_help() {
        let cmd = Cli::command();
        cmd.clone().debug_assert();
        assert_documented(&cmd, cmd.get_name());
    }
}
