use anyhow::Context;
use clap::Args;
use console::style;

use broccoli_cli_core::client::Client;
use broccoli_cli_core::resolve;

use super::{Countdown, local_time, phase_label, span_minutes};

#[derive(Args)]
pub struct InfoArgs {
    /// Contest ID or name (e.g. 3 or "Spring Round"); defaults to the
    /// .broccoli file's, then `contest` in config.toml
    pub contest: Option<String>,
}

pub fn run(args: InfoArgs) -> anyhow::Result<()> {
    let creds = crate::commands::credentials()?;
    let client = Client::new(creds);
    let cid = resolve::contest_id(
        &client,
        &crate::commands::context::contest_ref(args.contest.as_deref())?,
    )?;

    println!("{}  Fetching contest info...", style("→").blue().bold());

    let contest = client
        .get_contest(cid)
        .context("Failed to fetch contest details")?;

    let my_info = client
        .get_contest_my_info(cid)
        .context("Failed to fetch registration status")?;

    // Problems are listed once the contest has started.
    let problems = match contest.phase.has_started() {
        true => client
            .list_contest_problems(cid)
            .context("Failed to fetch contest problems")?,
        false => Vec::new(),
    };

    println!();
    println!("  {}", style(&contest.title).bold().underlined());
    println!();
    println!("  Status: {}", style(phase_label(contest.phase)).bold());
    println!("  Start:  {}", local_time(&contest.start_time));
    println!("  End:    {}", local_time(&contest.end_time));

    // Counted on the server's clock; without one, the times above stand.
    if let Some(now) = client.clock().now() {
        let countdown = match Countdown::at(contest.start_time, contest.end_time, now) {
            Countdown::StartsIn(span) => Some(format!("starts in {}", span_minutes(span))),
            Countdown::EndsIn(span) => Some(format!("{} remaining", span_minutes(span))),
            Countdown::Ended => None,
        };
        if let Some(countdown) = countdown {
            println!("  {}  {}", style("\u{23F1}").cyan(), countdown);
        }
    }

    if my_info.is_registered {
        println!("  {}  Registered", style("\u{2713}").green().bold());
    } else if contest.phase.has_ended() {
        println!("  {}  Not registered", style("\u{2717}").red());
    } else {
        println!(
            "  {}  Not registered: `broccoli contest register {}` registers you",
            style("\u{2717}").red(),
            cid
        );
    }

    if !problems.is_empty() {
        println!();
        println!("  {}", style("Problems:").bold());
        for p in &problems {
            println!(
                "    {}  {} — {}",
                style(&p.label).yellow().bold(),
                style("\u{2022}").dim(),
                broccoli_cli_core::model::problem_title(p)
            );
        }
    }

    Ok(())
}
