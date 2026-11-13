use std::io::IsTerminal;

use anyhow::Context;
use broccoli_cli_core::api::submission::{SubmissionListItem, SubmissionResponse};
use broccoli_cli_core::client::Client;
use broccoli_cli_core::model::{StatusDisplay, SubmissionStatus, Verdict, VerdictDisplay};
use broccoli_cli_core::{config, fmt};
use clap::Args;
use console::{Style, StyledObject, style};
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Args)]
pub struct StatusArgs {
    /// Submission ID to query. Omit to pick from your recent submissions.
    pub id: Option<String>,

    /// Print the recent-submissions table (no interactive picker)
    #[arg(long)]
    pub recent: bool,
}

pub fn run(args: StatusArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);

    if let Some(id) = args.id {
        let sub = client.get_submission(&id)?;
        print_submission(&sub);
        return Ok(());
    }

    let cid = super::context::discover_context()
        .and_then(|c| c.contest)
        .or_else(|| config::load_user_config().contest);
    let Some(cid) = cid else {
        anyhow::bail!(
            "No submission id and no contest context. Pass an id (`broccoli status <id>`), \
             run from a directory with a .broccoli file, or set one with \
             `broccoli config set contest <id>`."
        );
    };

    // scope to own submissions; never fall back to unscoped (would leak others' rows)
    let mut self_uid = None;
    for attempt in 0..2 {
        match client.me() {
            Ok(m) => {
                self_uid = Some(m.id.to_string());
                break;
            }
            Err(_) if attempt < 1 => std::thread::sleep(std::time::Duration::from_millis(250)),
            Err(e) => {
                return Err(e).context(
                    "Could not determine your account (GET /auth/me failed). \
                     Check your connection and try again.",
                );
            }
        }
    }
    let self_uid = self_uid.expect("loop sets uid or returns early");
    let subs = client.list_contest_submissions(&cid, None, Some(&self_uid), Some(1), Some(50))?;
    if subs.data.is_empty() {
        println!("  No submissions yet.");
        return Ok(());
    }

    // interactive picker on a tty; table otherwise so pipes get output
    let interactive =
        !args.recent && std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    if interactive {
        let items: Vec<String> = subs.data.iter().map(picker_row).collect();
        let choice = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("Select a submission (Esc to cancel)")
            .items(&items)
            .default(0)
            .max_length(15)
            .interact_opt()?;
        if let Some(i) = choice {
            let sub = client.get_submission(&subs.data[i].id.to_string())?;
            print_submission(&sub);
        }
    } else {
        print_recent_table(&subs.data);
    }
    Ok(())
}

// display-column widths shared by picker and table so rows line up under CJK
const W_ID: usize = 7;
const W_PROBLEM: usize = 22;
const W_VERDICT: usize = 18;
const W_SCORE: usize = 9;
const W_TIME: usize = 9;
const W_MEM: usize = 9;

/// One picker row, plain text (no ANSI, for dialoguer theme safety).
fn picker_row(s: &SubmissionListItem) -> String {
    let verdict = match s.verdict.as_ref() {
        Some(v) => v.human().to_string(),
        None => s.status.human().to_string(),
    };
    let score = s.score.map(fmt::score).unwrap_or_else(|| "—".into());
    let time = s
        .time_used
        .map(|t| fmt::time_ms(t as i64))
        .unwrap_or_default();
    format!(
        "{} {} {} {} {}",
        pad(&format!("#{}", s.id), W_ID),
        pad(&s.problem_title, W_PROBLEM),
        pad(&verdict, W_VERDICT),
        pad(&score, W_SCORE),
        pad(&time, W_TIME),
    )
}

fn print_recent_table(subs: &[SubmissionListItem]) {
    println!();
    println!(
        "  {} {} {} {} {} {}",
        style(pad("ID", W_ID)).bold(),
        style(pad("Problem", W_PROBLEM)).bold(),
        style(pad("Verdict", W_VERDICT)).bold(),
        style(pad("Score", W_SCORE)).bold(),
        style(pad("Time", W_TIME)).bold(),
        style(pad("Memory", W_MEM)).bold()
    );
    println!("  {}", "─".repeat(76));
    for s in subs {
        let pts = s.score.map(fmt::score).unwrap_or_else(|| "—".into());
        let t = s
            .time_used
            .map(|t| fmt::time_ms(t as i64))
            .unwrap_or_else(|| "—".into());
        let mem = s
            .memory_used
            .map(|m| fmt::memory_kb(m as i64))
            .unwrap_or_else(|| "—".into());
        // judged verdict if present, else lifecycle status; pad before colouring
        let verdict_plain = match s.verdict.as_ref() {
            Some(v) => v.human().to_string(),
            None => s.status.human().to_string(),
        };
        let verdict_cell =
            cell_style(s.verdict.as_ref(), &s.status).apply_to(pad(&verdict_plain, W_VERDICT));
        println!(
            "  {} {} {} {} {} {}",
            style(pad(&s.id.to_string(), W_ID)).cyan(),
            pad(&s.problem_title, W_PROBLEM),
            verdict_cell,
            pad(&pts, W_SCORE),
            pad(&t, W_TIME),
            pad(&mem, W_MEM)
        );
    }
    println!();
}

fn print_submission(sub: &SubmissionResponse) {
    println!();
    println!(
        "  {} {} — {}",
        style("Submission").bold(),
        style(format!("#{}", sub.id)).bold(),
        sub.problem_title
    );
    println!("  Status: {}", status_style(&sub.status));
    if let Some(ref result) = sub.result {
        if let Some(ref v) = result.verdict {
            println!("  Verdict: {}", verdict_style(v));
        }
        if let Some(s) = result.score {
            println!("  Score: {}", fmt::score(s));
        }
        // always show Time/Memory so a gap reads as "no data"
        println!(
            "  Time: {}",
            result
                .time_used
                .map(|t| fmt::time_ms(t as i64))
                .unwrap_or_else(|| "—".into())
        );
        println!(
            "  Memory: {}",
            result
                .memory_used
                .map(|m| fmt::memory_kb(m as i64))
                .unwrap_or_else(|| "—".into())
        );
        if let Some(msg) = result.error_message.as_deref().filter(|m| !m.is_empty()) {
            println!("  {}", style("Error:").red().bold());
            for line in msg.lines() {
                println!("    {}", style(line).red());
            }
        }
        if let Some(co) = result.compile_output.as_deref().filter(|c| !c.is_empty()) {
            println!("  Compile output:");
            for line in co.lines() {
                println!("    {}", style(line).dim());
            }
        }
    }
    println!();
}

/// Truncate (with ellipsis) and right-pad to exactly `width` display columns.
fn pad(s: &str, width: usize) -> String {
    let total = UnicodeWidthStr::width(s);
    let (mut out, used) = if total > width {
        let budget = width.saturating_sub(1);
        let mut acc = String::new();
        let mut w = 0usize;
        for ch in s.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if w + cw > budget {
                break;
            }
            acc.push(ch);
            w += cw;
        }
        acc.push('…');
        (acc, w + 1)
    } else {
        (s.to_string(), total)
    };
    if used < width {
        out.push_str(&" ".repeat(width - used));
    }
    out
}

/// Console style for a submission's verdict/status cell.
fn cell_style(verdict: Option<&Verdict>, status: &SubmissionStatus) -> Style {
    match verdict {
        Some(v) if v.is_accepted() => Style::new().green().bold(),
        Some(_) => Style::new().red().bold(),
        None if status.is_error() => Style::new().red(),
        None if status.is_in_progress() => Style::new().yellow(),
        None => Style::new(),
    }
}

fn status_style(s: &SubmissionStatus) -> StyledObject<String> {
    let label = s.human().to_string();
    if s.is_error() {
        style(label).red()
    } else if s.is_in_progress() {
        style(label).yellow()
    } else {
        style(label)
    }
}

fn verdict_style(v: &Verdict) -> StyledObject<String> {
    let s = Style::new().bold();
    if v.is_accepted() {
        s.green().apply_to(v.human().to_string())
    } else {
        s.red().apply_to(v.human().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::pad;
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn pad_fills_to_display_width() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(UnicodeWidthStr::width(pad("abc", 5).as_str()), 5);
    }

    #[test]
    fn pad_counts_cjk_as_two_columns() {
        let out = pad("你好世界", 10); // 4 chars = 8 columns, pad 2 spaces
        assert_eq!(UnicodeWidthStr::width(out.as_str()), 10);
        assert!(out.ends_with("  "));
    }

    #[test]
    fn pad_truncates_with_ellipsis_by_columns() {
        let out = pad("你好世界abc", 6); // 8+3 cols fit in 6 incl. ellipsis
        assert_eq!(UnicodeWidthStr::width(out.as_str()), 6);
        assert!(out.contains('…'));
    }
}
