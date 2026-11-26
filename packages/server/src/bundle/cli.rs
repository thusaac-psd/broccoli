//! `broccoli-server bundle export|import`: [`super::export`] and
//! [`super::import`] with this server's configuration (its config file and
//! `BROCCOLI__*` variables), so they reach the database and blob storage
//! exactly as the server does. Progress and the summary go to stderr; with
//! no file argument the archive goes to stdout or comes from stdin, so the
//! command also runs through `docker exec -i` with no shared directory.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::Context;
use clap::{ArgGroup, Args, Subcommand};
use common::db::{PgPoolSpec, PgSession};

use super::{ExportOptions, ImportOptions, Selection};
use crate::config::AppConfig;

/// `bundle`'s arguments, parsed as part of the server's command line.
#[derive(Args)]
pub struct BundleArgs {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Write a bundle of problems, or of a whole contest.
    Export {
        /// The contest to export: its problems, the contest, its roster and
        /// the roster's accounts and roles.
        #[arg(long, conflicts_with = "problems")]
        contest: Option<i32>,
        /// Only these problems (comma-separated ids). Default: every problem
        /// that is not deleted.
        #[arg(long, value_delimiter = ',')]
        problems: Option<Vec<i32>>,
        /// Keep the accounts' password hashes. Without it no account in the
        /// bundle has a password; an admin sets one before it can sign in.
        /// Secret plugin config fields stay out either way.
        #[arg(long, requires = "contest")]
        with_secrets: bool,
        /// Write the archive here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore a bundle onto this server's database and storage.
    #[command(group(ArgGroup::new("replacing").multiple(true).args(["replace_problems", "replace_contest"])))]
    Import {
        /// The archive to read. Default: stdin.
        archive: Option<PathBuf>,
        /// Replace every problem the database already has, those the
        /// bundle does not carry included: they are removed.
        #[arg(long)]
        replace_problems: bool,
        /// Replace the contest the database has with the bundle's contest
        /// id.
        #[arg(long)]
        replace_contest: bool,
        /// With --replace-problems or --replace-contest, also delete the
        /// submissions, code runs, contest links and clarifications that
        /// reference what is replaced.
        #[arg(long, requires = "replacing")]
        wipe_dependents: bool,
    },
}

/// Runs `bundle` and returns the process exit code.
pub async fn run(config: &AppConfig, args: BundleArgs) -> i32 {
    match execute(config, args.command).await {
        Ok(summary) => {
            eprintln!("{summary}");
            0
        }
        Err(e) => {
            eprintln!("error: {}", error_chain(&e));
            1
        }
    }
}

/// `e` and its causes, joined by `: `, leaving out a cause whose text the
/// line already ends with: a wrapping error (sqlx's, for one) often prints
/// its source's message as its own.
fn error_chain(e: &anyhow::Error) -> String {
    let mut line = String::new();
    for cause in e.chain() {
        let text = cause.to_string();
        if line.ends_with(&text) {
            continue;
        }
        if !line.is_empty() {
            line.push_str(": ");
        }
        line.push_str(&text);
    }
    line
}

/// The bundle tool's session: the configured one
/// (`crate::database::app_session`), without a statement timeout, since an
/// export or import runs long statements. It sends a startup option only
/// where the operator allowed one.
pub(crate) fn session(config: &crate::config::DatabaseConfig) -> PgSession {
    PgSession {
        application_name: "broccoli-bundle",
        ..crate::database::app_session(config).unbounded()
    }
}

async fn execute(config: &AppConfig, command: Command) -> anyhow::Result<super::Summary> {
    // Two connections: one transaction, and the database blob store beside it.
    let db = common::db::connect(
        &config.database.url,
        &PgPoolSpec::new(2, 1),
        &session(&config.database),
    )
    .await
    .context("connect to the database")?;
    let store = common::storage::config::create_blob_store(&config.storage, db.clone(), None)
        .await
        .context("open blob storage")?;
    match command {
        Command::Export {
            contest,
            problems,
            with_secrets,
            out,
        } => {
            let selection = match (contest, problems) {
                (Some(id), _) => Selection::Contest(id),
                (None, Some(ids)) => Selection::Problems(ids),
                (None, None) => Selection::AllProblems,
            };
            let options = ExportOptions {
                selection,
                with_secrets,
            };
            match out {
                Some(path) => {
                    let file = File::create(&path)
                        .with_context(|| format!("create {}", path.display()))?;
                    let result = super::export(
                        &db,
                        &*store,
                        &config.plugin.plugins_dir,
                        &options,
                        BufWriter::new(file),
                    )
                    .await;
                    if result.is_err() {
                        let _ = std::fs::remove_file(&path);
                    }
                    result
                }
                None => {
                    let stdout: Box<dyn Write> = Box::new(std::io::stdout().lock());
                    super::export(
                        &db,
                        &*store,
                        &config.plugin.plugins_dir,
                        &options,
                        BufWriter::new(stdout),
                    )
                    .await
                }
            }
        }
        Command::Import {
            archive,
            replace_problems,
            replace_contest,
            wipe_dependents,
        } => {
            let input: Box<dyn Read> = match archive {
                Some(path) => Box::new(BufReader::new(
                    File::open(&path).with_context(|| format!("open {}", path.display()))?,
                )),
                None => Box::new(BufReader::new(std::io::stdin().lock())),
            };
            let options = ImportOptions {
                replace_problems,
                replace_contest,
                wipe_dependents,
            };
            super::import(&db, store, options, input).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::error_chain;

    #[derive(Debug)]
    struct Wrapper(std::io::Error);

    impl std::fmt::Display for Wrapper {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "error returned from database: {}", self.0)
        }
    }

    impl std::error::Error for Wrapper {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn a_cause_its_wrapper_already_printed_appears_once() {
        let inner = std::io::Error::other(r#"relation "seaql_migrations" does not exist"#);
        let e = anyhow::Error::new(Wrapper(inner)).context("read the database's migrations");
        assert_eq!(
            error_chain(&e),
            r#"read the database's migrations: error returned from database: relation "seaql_migrations" does not exist"#
        );
    }

    #[test]
    fn distinct_causes_are_all_kept() {
        let e = anyhow::anyhow!("inner").context("middle").context("outer");
        assert_eq!(error_chain(&e), "outer: middle: inner");
    }
}
