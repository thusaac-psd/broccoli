use anyhow::Context;
use console::Term;
use console::style;

use broccoli_cli_core::api::contest::ContestPhase;
use broccoli_cli_core::client::Client;

use super::{local_time, phase_label};

pub fn run() -> anyhow::Result<()> {
    let creds = crate::commands::credentials()?;
    let client = Client::new(creds);

    println!("{}  Fetching contests...", style("→").blue().bold());

    let contests = client
        .contests(None)
        .context("Failed to fetch contest list")?;

    if contests.is_empty() {
        println!("  No contests available.");
        return Ok(());
    }

    let term_width = Term::stdout().size().1 as usize;

    // The table needs its full width (and its indent); a narrower terminal
    // gets one card per contest.
    if term_width >= TABLE_WIDTH + 2 {
        print_table(&contests);
    } else {
        print_cards(&contests);
    }

    Ok(())
}

fn status_label(phase: ContestPhase) -> console::StyledObject<&'static str> {
    let label = phase_label(phase);
    match phase {
        ContestPhase::Running => style(label).green().bold(),
        ContestPhase::Upcoming => style(label).cyan(),
        ContestPhase::Unactivated | ContestPhase::Ended | ContestPhase::Deactivated => {
            style(label).dim()
        }
    }
}

/// The table's columns and the spaces between them; a time with its zone
/// is 26 wide.
const TABLE_WIDTH: usize = 6 + 40 + 15 + 26 + 26 + 4;

fn print_table(contests: &[broccoli_cli_core::api::contest::ContestListItem]) {
    println!(
        "  {:<6} {:<40} {:<15} {:<26} {:<26}",
        style("ID").bold(),
        style("Name").bold(),
        style("Status").bold(),
        style("Start").bold(),
        style("End").bold(),
    );
    println!("  {}", style("-".repeat(TABLE_WIDTH)).dim());

    for c in contests {
        let styled_status = status_label(c.phase);
        println!(
            "  {:<6} {:<40} {:<15} {:<26} {:<26}",
            style(&c.id).yellow(),
            truncate(&c.title, 38),
            styled_status,
            local_time(&c.start_time),
            local_time(&c.end_time),
        );
    }
}

fn print_cards(contests: &[broccoli_cli_core::api::contest::ContestListItem]) {
    for (i, c) in contests.iter().enumerate() {
        let styled_status = status_label(c.phase);
        println!("  {} {}", style("#").dim(), i + 1);
        println!("    ID:     {}", style(&c.id).yellow());
        println!("    Name:   {}", c.title);
        println!("    Status: {}", styled_status);
        println!("    Start:  {}", local_time(&c.start_time));
        println!("    End:    {}", local_time(&c.end_time));
        println!();
    }
}

fn truncate(s: &str, max: usize) -> String {
    // Count and slice by characters, never bytes: byte slicing panics when the
    // cut falls inside a multi-byte UTF-8 sequence (e.g. a Chinese contest
    // title), and byte length is the wrong measure for a display width cap.
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use super::truncate;

    #[test]
    fn truncate_does_not_panic_on_multibyte_titles() {
        // A long non-ASCII title must truncate on a char boundary, not panic.
        let title = "北京大学程序设计竞赛决赛第一场周末专题";
        let out = truncate(title, 10);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), 10);
    }

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("abc", 10), "abc");
        assert_eq!(truncate("北京", 10), "北京");
    }
}
