use std::io::{self, Write};

use anyhow::{Context, bail};
use clap::Args;
use console::style;

use broccoli_cli_core::client::Client;
use broccoli_cli_core::{config, resolve};

#[derive(Args)]
pub struct ClarificationsArgs {
    #[command(subcommand)]
    pub command: Option<ClarificationsCommand>,
}

#[derive(clap::Subcommand)]
pub enum ClarificationsCommand {
    /// List clarifications for a contest
    #[command(visible_alias = "ls")]
    List(ListArgs),
    /// Ask a clarification for a contest
    #[command(visible_alias = "a", alias = "new")]
    Ask(AskArgs),
}

#[derive(Args)]
pub struct ListArgs {
    /// Contest ID or name (e.g. 3 or "Spring Round")
    pub contest: String,
}

#[derive(Args)]
pub struct AskArgs {
    /// Contest ID or name (e.g. 3 or "Spring Round")
    pub contest: String,
    /// The question to ask (prompted interactively if omitted)
    pub message: Option<String>,
}

pub fn run(args: ClarificationsArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);

    match args.command {
        Some(ClarificationsCommand::List(a)) => {
            let cid = resolve::contest_id(&client, &a.contest)?;
            list(&client, &cid)
        }
        Some(ClarificationsCommand::Ask(a)) => {
            let cid = resolve::contest_id(&client, &a.contest)?;
            ask(&client, &cid, a.message)
        }
        None => {
            anyhow::bail!(
                "Usage: broccoli clarifications list <contest> | ask <contest> [message]"
            );
        }
    }
}

fn list(client: &Client, contest: &str) -> anyhow::Result<()> {
    println!(
        "{}  Clarifications for contest {}:\n",
        style("→").blue().bold(),
        contest
    );

    let resp = client
        .list_clarifications(contest)
        .context("Failed to fetch clarifications")?;

    if resp.data.is_empty() {
        println!("  No clarifications yet.");
        return Ok(());
    }

    for c in &resp.data {
        let kind = match c.clarification_type.as_str() {
            "announcement" => style("[announcement]").yellow().bold(),
            "question" => style("[question]").cyan(),
            other => style(other).dim(),
        };
        let resolved = if c.resolved {
            style(" ✓ answered").green()
        } else {
            style(" • open").dim()
        };
        println!(
            "  {} {}{}",
            style(format!("#{}", c.id)).bold(),
            kind,
            resolved
        );
        println!("    {}: {}", style(&c.author_name).dim(), c.content);
        if let Some(reply) = &c.reply_content
            && !reply.is_empty()
        {
            let who = c.reply_author_name.as_deref().unwrap_or("staff");
            println!(
                "    {} {}: {}",
                style("↳").green(),
                style(who).green(),
                reply
            );
        }
        println!();
    }
    Ok(())
}

fn ask(client: &Client, contest: &str, message: Option<String>) -> anyhow::Result<()> {
    let content = match message {
        Some(m) => m,
        None => {
            print!("{} Your question: ", style("?").cyan().bold());
            io::stdout().flush().ok();
            let mut line = String::new();
            io::stdin()
                .read_line(&mut line)
                .context("Failed to read question")?;
            line.trim().to_string()
        }
    };

    if content.is_empty() {
        bail!("No question provided.");
    }

    let created = client
        .create_clarification(contest, &content)
        .context("Failed to submit clarification")?;

    println!(
        "{}  Clarification submitted (#{}). Watch for a reply with `broccoli clarifications list {}`.",
        style("✓").green().bold(),
        created.id,
        contest
    );
    Ok(())
}
