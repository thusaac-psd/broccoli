use anyhow::Context;
use chrono::{DateTime, Utc};
use console::Term;
use console::style;

use broccoli_cli_core::client::Client;

pub fn run() -> anyhow::Result<()> {
    let creds = crate::commands::credentials()?;
    let client = Client::new(creds);

    println!("{}  Fetching contests...", style("→").blue().bold());

    let resp = client
        .list_contests()
        .context("Failed to fetch contest list")?;

    if resp.data.is_empty() {
        println!("  No contests available.");
        return Ok(());
    }

    let term_width = Term::stdout().size().1 as usize;

    if term_width >= 80 {
        print_table(&resp.data);
    } else {
        print_cards(&resp.data);
    }

    Ok(())
}

fn status_label(
    start: &DateTime<Utc>,
    end: &DateTime<Utc>,
) -> (String, console::StyledObject<&'static str>) {
    let now = Utc::now();
    if now < *start {
        ("Upcoming".into(), style("Upcoming").cyan())
    } else if now > *end {
        ("Finished".into(), style("Finished").dim())
    } else {
        ("Running".into(), style("Running").green().bold())
    }
}

fn format_time(ts: &DateTime<Utc>) -> String {
    ts.format("%Y-%m-%d %H:%M").to_string()
}

fn print_table(contests: &[broccoli_cli_core::api::contest::ContestListItem]) {
    println!(
        "  {:<6} {:<40} {:<12} {:<20} {:<20}",
        style("ID").bold(),
        style("Name").bold(),
        style("Status").bold(),
        style("Start").bold(),
        style("End").bold(),
    );
    println!("  {}", style("-".repeat(98)).dim());

    for c in contests {
        let (_status_str, styled_status) = status_label(&c.start_time, &c.end_time);
        println!(
            "  {:<6} {:<40} {}   {:<20} {:<20}",
            style(&c.id).yellow(),
            truncate(&c.title, 38),
            styled_status,
            format_time(&c.start_time),
            format_time(&c.end_time),
        );
    }
}

fn print_cards(contests: &[broccoli_cli_core::api::contest::ContestListItem]) {
    for (i, c) in contests.iter().enumerate() {
        let (_status_str, styled_status) = status_label(&c.start_time, &c.end_time);
        println!("  {} {}", style("#").dim(), i + 1);
        println!("    ID:     {}", style(&c.id).yellow());
        println!("    Name:   {}", c.title);
        println!("    Status: {}", styled_status);
        println!("    Start:  {}", format_time(&c.start_time));
        println!("    End:    {}", format_time(&c.end_time));
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
