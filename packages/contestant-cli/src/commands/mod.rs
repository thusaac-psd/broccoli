pub mod clarifications;
pub mod config;
pub mod contest;
pub mod context;
pub mod login;
pub mod logout;
pub mod prewarm;
pub mod status;
pub mod submit;
pub mod test;
pub mod watch;
pub mod whoami;

use self::clarifications::ClarificationsArgs;
use self::config::ConfigArgs;
use self::contest::ContestArgs;
use self::login::LoginArgs;
use self::status::StatusArgs;
use self::submit::SubmitArgs;
use self::test::TestArgs;
use self::watch::WatchArgs;
use std::sync::OnceLock;

use broccoli_cli_core::server::ServerUrl;
use broccoli_cli_core::session::Session;
use clap::{Args, Subcommand};

/// Options every command takes.
#[derive(Args, Clone, Default)]
pub struct GlobalArgs {
    /// Server URL. Defaults to the server you last logged in to, then
    /// `server` in config.toml
    #[arg(long, short = 's', global = true, env = "BROCCOLI_URL")]
    pub server: Option<ServerUrl>,

    /// Access token to use instead of the saved login (never refreshed)
    #[arg(long, global = true, env = "BROCCOLI_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
}

static GLOBALS: OnceLock<GlobalArgs> = OnceLock::new();

/// Record the global options; `main` calls this once, before any command.
pub fn init(globals: GlobalArgs) {
    let _ = GLOBALS.set(globals);
}

fn globals() -> &'static GlobalArgs {
    GLOBALS.get_or_init(GlobalArgs::default)
}

/// The session a command uses: `--server`/`--token` when given, else the
/// saved login (see [`broccoli_cli_core::config::resolve_credentials`]).
pub fn credentials() -> anyhow::Result<Session> {
    let g = globals();
    broccoli_cli_core::config::resolve_credentials(g.server.as_ref(), g.token.as_deref())
}

/// The `--server` option, if given.
pub fn server_option() -> Option<&'static ServerUrl> {
    globals().server.as_ref()
}

/// The command did its work and the answer is no: a verdict other than
/// Accepted, or a sample whose output differs. It is reported on the way out
/// so scripts can tell it from an error; `broccoli` exits with
/// [`Rejected::EXIT_CODE`], while an error exits with 1 and a usage error
/// with 2.
#[derive(Debug)]
pub struct Rejected(pub String);

impl Rejected {
    pub const EXIT_CODE: u8 = 3;
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Rejected {}

// `status` never gets `s` so a fat-finger can't turn a status check into a submit.
#[derive(Subcommand)]
pub enum Command {
    /// Log in to a Broccoli contest server
    #[command(alias = "li")]
    Login(LoginArgs),

    /// Show who you're logged in as
    #[command(visible_alias = "me")]
    Whoami,

    /// Forget the saved login for a server (the active one by default)
    Logout,

    /// Submit a solution to a problem
    #[command(visible_alias = "s", alias = "sub")]
    Submit(SubmitArgs),

    /// Test a solution against sample cases (remote or local)
    #[command(visible_alias = "t", alias = "tst")]
    Test(TestArgs),

    /// Manage contests (list, info, register, unregister, problems)
    #[command(visible_alias = "c", alias = "con")]
    Contest(ContestArgs),

    /// Query submission status (interactive picker when no id is given)
    #[command(visible_alias = "st", aliases = ["ss", "stat"])]
    Status(StatusArgs),

    /// List or ask clarifications for a contest
    #[command(visible_alias = "clar", aliases = ["cl", "clarification"])]
    Clarifications(ClarificationsArgs),

    /// Show or modify CLI configuration
    #[command(visible_alias = "cfg", alias = "conf")]
    Config(ConfigArgs),

    /// Watch a contest in real-time (TUI dashboard)
    #[command(visible_alias = "w", alias = "dash")]
    Watch(WatchArgs),

    /// Generate a shell completion script (bash, zsh, fish, powershell, elvish)
    Completions(CompletionsArgs),

    /// Warm DNS/TLS + the binary cache so the first real command is snappy
    Prewarm,
}

#[derive(Args)]
pub struct CompletionsArgs {
    /// Shell to generate a completion script for
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}
