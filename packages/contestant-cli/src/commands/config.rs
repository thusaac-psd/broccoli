use broccoli_cli_core::config::{ConfigKey, ConfigLayers};
use broccoli_cli_core::server::ServerUrl;
use clap::{Args, Subcommand};
use console::style;

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: Option<ConfigCommand>,
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show current configuration
    #[command(visible_alias = "sh", aliases = ["get", "ls"])]
    Show,
    /// Set a configuration value
    Set(SetArgs),
    /// Unset a configuration value
    #[command(alias = "rm")]
    Unset(UnsetArgs),
}

#[derive(Args)]
pub struct SetArgs {
    /// Config key: contest, language, server or tls
    pub key: ConfigKey,
    /// Config value
    pub value: String,
}

#[derive(Args)]
pub struct UnsetArgs {
    /// Config key: contest, language, server or tls
    pub key: ConfigKey,
}

pub fn run(args: ConfigArgs) -> anyhow::Result<()> {
    let mut layers = ConfigLayers::load();
    match args.command.unwrap_or(ConfigCommand::Show) {
        ConfigCommand::Show => show(&layers),
        ConfigCommand::Set(a) => {
            if a.key == ConfigKey::Server {
                a.value.parse::<ServerUrl>()?;
            }
            layers.set(a.key, a.value.clone())?;
            println!(
                "{}  {} = {} (in {})",
                style("✓").green(),
                a.key,
                a.value,
                layers.user_path().display()
            );
        }
        ConfigCommand::Unset(a) => {
            let machine = layers.unset(a.key)?.map(str::to_string);
            println!(
                "{}  {} unset in {}",
                style("✓").green(),
                a.key,
                layers.user_path().display()
            );
            if let Some(value) = machine {
                println!(
                    "   {} = {} still applies from {}",
                    a.key,
                    value,
                    layers.system_path().display()
                );
            }
        }
    }
    Ok(())
}

fn show(layers: &ConfigLayers) {
    println!(
        "{}  Configuration ({}, over {}):\n",
        style("→").blue().bold(),
        layers.user_path().display(),
        layers.system_path().display()
    );
    for key in ConfigKey::ALL {
        let shown = match (layers.user.get(key), layers.system.get(key)) {
            (Some(value), _) => value.to_string(),
            (None, Some(value)) => format!("{value} (machine)"),
            (None, None) => "(not set)".to_string(),
        };
        println!("  {:<8} = {}", key.name(), shown);
    }
    let runtimes = layers.merged().runtimes;
    if !runtimes.is_empty() {
        println!("\n  Runtimes:");
        for (ext, cmd) in &runtimes {
            println!("    {} = \"{}\"", ext, cmd);
        }
    }
}
