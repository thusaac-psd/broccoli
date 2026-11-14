use anyhow::Result;
use clap::{CommandFactory, Parser};

use broccoli_contestant_cli::commands::Command;
use broccoli_contestant_cli::commands::contest::ContestCommand;

#[derive(Parser)]
#[command(
    name = "broccoli",
    about = "Broccoli contestant CLI — submit, test, and compete",
    version,
    after_help = "\
EXAMPLES:
  broccoli login                          Log in (opens your browser)
  broccoli contest list                   See available contests
  broccoli contest info <id|name>         Contest details + your status
  broccoli submit sol.cpp -p A -c <id>    Submit problem A
  broccoli test sol.cpp -p A -c <id>      Run the sample cases
  broccoli status                         Pick a recent submission to inspect
  broccoli watch <id|name>                Live contest dashboard

Most commands accept a contest by id or name and a problem by label (A), \
position (1), title (\"Two Sum\") or id (#42). Run `broccoli completions <shell>` to enable tab-completion."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn main() -> Result<()> {
    configure_colors();
    let cli = Cli::parse();
    match cli.command {
        Command::Completions(args) => {
            let mut cmd = Cli::command();
            clap_complete::generate(args.shell, &mut cmd, "broccoli", &mut std::io::stdout());
            Ok(())
        }
        Command::Prewarm => broccoli_contestant_cli::commands::prewarm::run(),
        Command::Login(args) => broccoli_contestant_cli::commands::login::run(args),
        Command::Whoami => broccoli_contestant_cli::commands::whoami::run(),
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
