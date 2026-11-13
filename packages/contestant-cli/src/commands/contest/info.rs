use anyhow::Context;
use chrono::Utc;
use clap::Args;
use console::style;

use broccoli_cli_core::client::Client;
use broccoli_cli_core::{config, resolve};

#[derive(Args)]
pub struct InfoArgs {
    /// Contest ID or name (e.g. 3 or "Spring Round")
    #[arg(required = true)]
    pub contest: String,
}

pub fn run(args: InfoArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);
    let cid = resolve::contest_id(&client, &args.contest)?;

    println!("{}  Fetching contest info...", style("→").blue().bold());

    let contest = client
        .get_contest(&cid)
        .context("Failed to fetch contest details")?;

    let my_info = client
        .get_contest_my_info(&cid)
        .context("Failed to fetch registration status")?;

    let problems = client
        .list_contest_problems(&cid)
        .context("Failed to fetch contest problems")?;

    println!();
    println!("  {}", style(&contest.title).bold().underlined());
    println!();

    let now = Utc::now();
    let end = contest.end_time;

    if now > end {
        println!(
            "  {} {}",
            style("\u{2713}").green(),
            style("Contest has ended.").dim()
        );
    } else {
        let remaining = end - now;
        let days = remaining.num_days();
        let hours = remaining.num_hours() % 24;
        let minutes = remaining.num_minutes() % 60;

        let remaining_str = if days > 0 {
            format!("{}d {}h {}m remaining", days, hours, minutes)
        } else if hours > 0 {
            format!("{}h {}m remaining", hours, minutes)
        } else {
            format!("{}m remaining", minutes)
        };
        println!("  {}  {}", style("\u{23F1}").cyan(), remaining_str);
    }

    if my_info.is_registered {
        println!("  {}  Registered", style("\u{2713}").green().bold());
    } else {
        println!("  {}  Not registered", style("\u{2717}").red());
    }

    if !problems.is_empty() {
        println!();
        println!("  {}", style("Problems:").bold());
        for p in &problems {
            println!(
                "    {}  {} — {}",
                style(&p.label).yellow().bold(),
                style("\u{2022}").dim(),
                p.problem_title
            );
        }
    }

    Ok(())
}
