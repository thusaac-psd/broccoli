use std::io::IsTerminal;

use anyhow::Context;
use broccoli_cli_core::api::submission::{
    SubmissionListItem, SubmissionListResponse, SubmissionResponse,
};
use broccoli_cli_core::client::Client;
use broccoli_cli_core::model::{Standing, StatusDisplay, Verdict, VerdictDisplay};
use broccoli_cli_core::{fmt, resolve};
use clap::Args;
use console::{Style, StyledObject, style};
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Args)]
pub struct StatusArgs {
    /// Submission ID to query. Omit to pick from your recent submissions.
    pub id: Option<String>,

    /// Contest ID or name (e.g. 3 or "Spring Round") to list your recent
    /// submissions in; defaults to the .broccoli file's or your saved one
    #[arg(short = 'c', long, conflicts_with = "id")]
    pub contest: Option<String>,

    /// Only list submissions to this problem: label, position, title or #ID
    /// (e.g. A, 1, "Two Sum" or #42)
    #[arg(short = 'p', long, conflicts_with = "id")]
    pub problem: Option<String>,

    /// Print the recent-submissions table (no interactive picker)
    #[arg(long)]
    pub recent: bool,
}

pub fn run(args: StatusArgs) -> anyhow::Result<()> {
    let creds = crate::commands::credentials()?;
    let client = Client::new(creds);

    if let Some(id) = args.id {
        let sub = client.get_submission(&id)?;
        print_submission(&sub);
        return Ok(());
    }

    let contest = super::context::contest_ref(args.contest.as_deref()).context(
        "No submission id and no contest. Pass an id (`broccoli status <id>`) or a contest \
         (`-c <id|name>`).",
    )?;
    let subs = own_recent_submissions(&client, &contest, args.problem.as_deref())?;
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

/// The caller's own latest submissions in the contest `contest` refers to
/// (its id or its title), to the problem `problem` refers to when given.
fn own_recent_submissions(
    client: &Client,
    contest: &str,
    problem: Option<&str>,
) -> anyhow::Result<SubmissionListResponse> {
    let contest = resolve::contest_id(client, contest)?;
    let problem = problem
        .map(|p| resolve::problem(client, contest, p).map(|p| p.id))
        .transpose()?;
    // scope to own submissions; never fall back to unscoped (would leak others' rows)
    let mut self_uid = None;
    for attempt in 0..2 {
        match client.me() {
            Ok(m) => {
                self_uid = Some(m.id);
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
    let subs = client.list_contest_submissions(contest, problem, Some(self_uid), None, Some(50))?;
    Ok(subs)
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
        None => Standing::of_item(s).human().to_string(),
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
        let standing = Standing::of_item(s);
        let verdict_plain = match s.verdict.as_ref() {
            Some(v) => v.human().to_string(),
            None => standing.human().to_string(),
        };
        let verdict_cell =
            cell_style(s.verdict.as_ref(), &standing).apply_to(pad(&verdict_plain, W_VERDICT));
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
    let standing = Standing::of_submission(sub);
    println!("  Status: {}", status_style(&standing));
    if let Some(waiting_for) = standing.waiting_for() {
        println!("  {}", style(waiting_for).dim());
    }
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
fn cell_style(verdict: Option<&Verdict>, standing: &Standing) -> Style {
    match verdict {
        Some(v) if v.is_accepted() => Style::new().green().bold(),
        Some(_) => Style::new().red().bold(),
        None => standing_style(standing),
    }
}

fn standing_style(standing: &Standing) -> Style {
    match standing {
        Standing::Status(s) if s.is_error() => Style::new().red(),
        _ if standing.is_in_progress() => Style::new().yellow(),
        _ => Style::new(),
    }
}

fn status_style(standing: &Standing) -> StyledObject<String> {
    standing_style(standing).apply_to(standing.human().to_string())
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
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    use broccoli_cli_core::session::Session;

    use super::*;
    use unicode_width::UnicodeWidthStr;

    /// A server answering each request by its path, recording what it was
    /// asked for.
    fn serve(answer: fn(&str) -> String) -> (Client, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let asked = Arc::new(Mutex::new(Vec::new()));
        let log = asked.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                let path = request.split(' ').nth(1).unwrap_or_default().to_string();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim_end().is_empty() {
                        break;
                    }
                }
                let body = answer(&path);
                log.lock().unwrap().push(path);
                let mut stream = stream;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (Client::new(Session::explicit(url, "token".into())), asked)
    }

    /// The answers of a server holding contest 3 "Spring Round", whose
    /// problem A is problem 42, signed in as user 7 with no submissions.
    fn spring_round(path: &str) -> String {
        let at = "\"2030-01-01T00:00:00Z\"";
        if path == "/api/v1/auth/me" {
            r#"{"id":7,"username":"alice","display_name":"Alice","roles":[],"permissions":[]}"#
                .into()
        } else if path == "/api/v1/contests/3/problems" {
            r#"[{"contest_id":3,"problem_id":42,"label":"A","position":1,
            "problem_title":"Two Sum","withheld":null}]"#
                .into()
        } else if path.starts_with("/api/v1/contests/3/") {
            r#"{"data":[],"pagination":{"per_page":50,"total":null,"total_pages":null,
            "has_more":false,"next_cursor":null}}"#
                .into()
        } else {
            format!(
                r#"{{"data":[{{"id":3,"title":"Spring Round","start_time":{at},
                "end_time":{at},"is_public":true,"submissions_visible":false,
                "show_compile_output":true,"show_participants_list":true,
                "created_at":{at},"updated_at":{at},"phase":"running"}}],
                "pagination":{{"page":1,"per_page":100,"total":1,"total_pages":1}}}}"#
            )
        }
    }

    #[test]
    fn status_finds_the_contest_a_name_refers_to() {
        let (client, asked) = serve(spring_round);
        let subs = own_recent_submissions(&client, "Spring Round", None).unwrap();
        assert!(subs.data.is_empty());
        assert_eq!(
            asked.lock().unwrap().last().unwrap(),
            "/api/v1/contests/3/submissions?user_id=7&per_page=50"
        );
    }

    #[test]
    fn status_lists_only_the_problem_it_is_given() {
        let (client, asked) = serve(spring_round);
        own_recent_submissions(&client, "3", Some("A")).unwrap();
        assert_eq!(
            asked.lock().unwrap().last().unwrap(),
            "/api/v1/contests/3/submissions?problem_id=42&user_id=7&per_page=50"
        );
    }

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
