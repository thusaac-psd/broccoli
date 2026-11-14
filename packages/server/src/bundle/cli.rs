//! `broccoli-server bundle export|import`: [`super::export`] and
//! [`super::import`] with this server's configuration (`config/config.toml`
//! and `BROCCOLI__*` variables), so they reach the database and blob storage
//! exactly as the server does. Progress and the summary go to stderr; with
//! no file argument the archive goes to stdout or comes from stdin, so the
//! command also runs through `docker exec -i` with no shared directory.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use common::db::{PgPoolSpec, PgSession};

use super::{ExportOptions, ImportOptions, Selection};
use crate::config::AppConfig;

#[derive(Parser)]
#[command(
    name = "broccoli-server bundle",
    about = "Export or import a problem or contest bundle"
)]
struct Args {
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
        /// Keep the accounts' password hashes. Without it every password in
        /// the bundle is blank.
        #[arg(long, requires = "contest")]
        with_secrets: bool,
        /// Write the archive here instead of stdout.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Restore a bundle onto this server's database and storage.
    Import {
        /// The archive to read. Default: stdin.
        archive: Option<PathBuf>,
        /// Replace the problems the database already has.
        #[arg(long)]
        replace_problems: bool,
        /// With --replace-problems, also delete the submissions, code runs
        /// and contest links that reference them.
        #[arg(long, requires = "replace_problems")]
        wipe_dependents: bool,
    },
}

/// Runs `bundle <args>` and returns the process exit code.
pub async fn run(config: &AppConfig, args: &[String]) -> i32 {
    let args = match Args::try_parse_from(
        std::iter::once("broccoli-server bundle".to_string()).chain(args.iter().cloned()),
    ) {
        Ok(args) => args,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { 2 } else { 0 };
        }
    };
    match execute(config, args.command).await {
        Ok(summary) => {
            eprintln!("{summary}");
            0
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            1
        }
    }
}

async fn execute(config: &AppConfig, command: Command) -> anyhow::Result<super::Summary> {
    // Two connections: one transaction, and the database blob store beside it.
    let db = common::db::connect(
        &config.database.url,
        &PgPoolSpec::new(2, 1),
        &PgSession::unbounded("broccoli-bundle"),
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
                    let result = super::export(&db, &*store, &options, BufWriter::new(file)).await;
                    if result.is_err() {
                        let _ = std::fs::remove_file(&path);
                    }
                    result
                }
                None => {
                    let stdout: Box<dyn Write> = Box::new(std::io::stdout().lock());
                    super::export(&db, &*store, &options, BufWriter::new(stdout)).await
                }
            }
        }
        Command::Import {
            archive,
            replace_problems,
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
                wipe_dependents,
            };
            super::import(&db, store, options, input).await
        }
    }
}
