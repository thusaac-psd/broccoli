use anyhow::Result;
use clap::Parser;

use broccoli_dev_cli::commands::Command;
use broccoli_dev_cli::commands::plugin::PluginCommand;

#[derive(Parser)]
#[command(
    name = "broccoli-dev",
    about = "Broccoli plugin developer CLI",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Plugin(args) => match args.command {
            PluginCommand::New(new_args) => broccoli_dev_cli::commands::plugin::new::run(new_args),
            PluginCommand::Build(build_args) => {
                broccoli_dev_cli::commands::plugin::build::run(build_args)
            }
            PluginCommand::Watch(watch_args) => {
                broccoli_dev_cli::commands::plugin::watch::run(watch_args)
            }
        },
        Command::Login(login_args) => broccoli_dev_cli::commands::login::run(login_args),
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
