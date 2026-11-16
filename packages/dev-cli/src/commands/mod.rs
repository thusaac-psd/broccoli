pub mod login;
pub mod plugin;

use clap::Subcommand;

use self::login::LoginArgs;
use self::plugin::PluginArgs;

#[derive(Subcommand)]
pub enum Command {
    /// Create, build and live-reload plugins
    Plugin(PluginArgs),
    /// Sign in to a Broccoli server to upload plugins to
    Login(LoginArgs),
}
