use super::context;
use anyhow::Context;
use broccoli_cli_core::api::submission::SubmissionFileDto;
use broccoli_cli_core::client::Client;
use broccoli_cli_core::config::{self, load_user_config};
use broccoli_cli_core::fmt;
use broccoli_cli_core::model::{StatusDisplay, VerdictDisplay};
use broccoli_cli_core::resolve;
use clap::Args;
use console::style;
use std::collections::HashMap;
use std::fs;
use std::time::Duration;

#[derive(Args)]
pub struct SubmitArgs {
    /// Source files to submit
    #[arg(required = true)]
    pub files: Vec<String>,

    /// Contest ID or name (e.g. 3 or "Spring Round")
    #[arg(short = 'c', long, conflicts_with = "no_contest")]
    pub contest: Option<String>,

    /// Problem label, position, title or #<id> (e.g. A, 1, "Two Sum" or #42);
    /// with --no-contest, the problem id
    #[arg(short = 'p', long)]
    pub problem: Option<String>,

    /// Submit to a standalone problem instead of a contest
    #[arg(long, default_value_t = false)]
    pub no_contest: bool,

    /// Programming language (auto-detected from file extension if omitted)
    #[arg(short = 'l', long)]
    pub language: Option<String>,

    /// Watch for the verdict after submitting
    #[arg(short = 'w', long, default_value_t = false)]
    pub watch: bool,
}

pub fn run(args: SubmitArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);
    let user_config = load_user_config();
    let ctx = context::discover_context();

    let problem_ref = args
        .problem
        .as_deref()
        .or(ctx.as_ref().and_then(|c| c.problem.as_deref()))
        .context("No problem specified. Use -p <id|label> (e.g. -p A).")?;

    let contest_id: Option<String> = if args.no_contest {
        None
    } else {
        let contest_ref = args
            .contest
            .as_deref()
            .or(ctx.as_ref().and_then(|c| c.contest.as_deref()))
            .or(user_config.contest.as_deref())
            .context(
                "No contest specified. Use -c <id|name>, or --no-contest to submit \
                 to a standalone problem.",
            )?;
        Some(resolve::contest_id(&client, contest_ref)?)
    };
    let problem_id: String = match contest_id.as_deref() {
        Some(cid) => {
            let problem = resolve::problem(&client, cid, problem_ref)?;
            println!(
                "{}  Problem {}",
                style("→").blue().bold(),
                style(&problem).cyan()
            );
            problem.id.to_string()
        }
        // standalone: no contest to resolve labels against, expect an id
        None => resolve::explicit_problem_id(problem_ref)
            .or_else(|| problem_ref.trim().parse().ok())
            .with_context(|| format!("'{problem_ref}' is not a problem id"))?
            .to_string(),
    };

    let mut submission_files = Vec::new();
    for path in &args.files {
        let content =
            fs::read_to_string(path).with_context(|| format!("Failed to read: {}", path))?;
        let filename = std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        submission_files.push(SubmissionFileDto { filename, content });
    }

    let language = context::resolve_language(&args.files, args.language.as_deref())?;

    println!(
        "{}  Language: {}",
        style("→").blue().bold(),
        style(&language).cyan()
    );

    if let [file] = submission_files.as_mut_slice() {
        // Read the problem the same way it is submitted: through its contest
        // when there is one, so another contest holding the same problem
        // cannot hide it here. A problem that cannot be read is sent as-is
        // and the server has the final word on its filename.
        let problem = match contest_id.as_deref() {
            Some(cid) => client.get_contest_problem(cid, &problem_id),
            None => client.get_problem(&problem_id),
        };
        if let Ok(problem) = problem
            && let Some(required) =
                single_required_filename(problem.submission_format.as_ref(), &language)
            && required != file.filename
        {
            println!(
                "{}  Submitting {} as {} (the filename this problem requires)",
                style("→").blue().bold(),
                style(&file.filename).cyan(),
                style(required).cyan()
            );
            file.filename = required.to_string();
        }
    }

    match contest_id.as_deref() {
        Some(c) => println!(
            "{}  Submitting to contest {}...",
            style("→").blue().bold(),
            style(c).cyan()
        ),
        None => println!(
            "{}  Submitting to standalone problem {}...",
            style("→").blue().bold(),
            style(&problem_id).cyan()
        ),
    }

    let submission = match contest_id.as_deref() {
        Some(cid) => client
            .create_contest_submission(cid, &problem_id, submission_files, &language, None)
            .context("Failed to create submission")?,
        None => client
            .create_submission(&problem_id, submission_files, &language, None)
            .context("Failed to create submission")?,
    };

    println!(
        "{}  Submitted! ID: {}",
        style("✓").green().bold(),
        style(submission.id).yellow()
    );

    // persist resolved ids so future runs skip the name/label lookup
    if ctx.is_none() {
        let _ = context::save_context(&context::ProjectContext {
            contest: contest_id.clone(),
            problem: Some(format!("#{problem_id}")),
            language: Some(language),
        });
    }

    if args.watch {
        watch_verdict(&client, submission.id)?;
    } else {
        println!(
            "   Check the verdict any time with: {}",
            style(format!("broccoli status {}", submission.id)).cyan()
        );
    }

    Ok(())
}

/// The one filename `submission_format` requires for `language`, if it
/// requires exactly one distinct non-empty name. `None` when the problem
/// fixes no filenames, says nothing about this language, or asks for several
/// files, since a single file cannot be renamed to fit those.
fn single_required_filename<'a>(
    submission_format: Option<&'a HashMap<String, Vec<String>>>,
    language: &str,
) -> Option<&'a str> {
    let mut names = submission_format?
        .get(language.trim())?
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty());
    let first = names.next()?;
    names.all(|name| name == first).then_some(first)
}

fn watch_verdict(client: &Client, submission_id: i32) -> anyhow::Result<()> {
    use std::io::Write;
    let submission_id = submission_id.to_string();
    let submission_id = submission_id.as_str();
    let mut spinner = broccoli_cli_core::tui::spinner::Spinner::new();
    let start = std::time::Instant::now();

    loop {
        if start.elapsed() > Duration::from_secs(300) {
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
            anyhow::bail!(
                "Timed out waiting for the verdict. Check later with `broccoli status {}`.",
                submission_id
            );
        }

        let sub = client
            .get_submission(submission_id)
            .context("Failed to poll submission status")?;
        let elapsed = start.elapsed();

        if sub.status.is_in_progress() {
            // \x1b[K clears stale chars from a previous longer frame
            eprint!(
                "\r\x1b[K  {} Judging... {} ({:.1}s)",
                spinner.next(),
                sub.status.human(),
                elapsed.as_secs_f64()
            );
            std::io::stderr().flush().ok();
            std::thread::sleep(Duration::from_millis(200));
        } else {
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();

            let accepted = sub
                .result
                .as_ref()
                .and_then(|r| r.verdict.as_ref())
                .is_some_and(|v| v.is_accepted());
            // judged verdict if present, else lifecycle status
            let label = match sub.result.as_ref().and_then(|r| r.verdict.as_ref()) {
                Some(v) => v.human().to_string(),
                None => sub.status.human().to_string(),
            };
            let score = sub.result.as_ref().and_then(|r| r.score);

            {
                if accepted {
                    if let Some(s) = score {
                        println!(
                            "{}  Accepted — {} points! ({:.1}s)",
                            style("✓").green().bold(),
                            fmt::score(s),
                            elapsed.as_secs_f64()
                        );
                    } else {
                        println!(
                            "{}  Accepted! ({:.1}s)",
                            style("✓").green().bold(),
                            elapsed.as_secs_f64()
                        );
                    }
                } else {
                    let sym = style("✗").red().bold();
                    if let Some(s) = score {
                        println!(
                            "{}  {} — {} points ({:.1}s)",
                            sym,
                            label,
                            fmt::score(s),
                            elapsed.as_secs_f64()
                        );
                    } else {
                        println!("{}  {} ({:.1}s)", sym, label, elapsed.as_secs_f64());
                    }
                }
            }

            if let Some(ref result) = sub.result {
                if let Some(msg) = result.error_message.as_deref().filter(|m| !m.is_empty()) {
                    println!("  {}", style("Error:").red().bold());
                    for line in msg.lines() {
                        println!("    {}", style(line).red());
                    }
                }
                if let Some(ref co) = result.compile_output
                    && !co.is_empty()
                {
                    println!("\n  Compilation output:");
                    for line in co.lines() {
                        println!("    {}", style(line).dim());
                    }
                }
            }

            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::single_required_filename;
    use std::collections::HashMap;

    fn format(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(lang, names)| {
                (
                    lang.to_string(),
                    names.iter().map(|n| n.to_string()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn one_required_name_is_chosen() {
        let f = format(&[("cpp", &["solution.cpp"]), ("python3", &["main.py"])]);
        assert_eq!(
            single_required_filename(Some(&f), "cpp"),
            Some("solution.cpp")
        );
        assert_eq!(
            single_required_filename(Some(&f), "python3"),
            Some("main.py")
        );
    }

    #[test]
    fn no_format_or_unlisted_language_has_no_name() {
        let f = format(&[("cpp", &["solution.cpp"])]);
        assert_eq!(single_required_filename(None, "cpp"), None);
        assert_eq!(single_required_filename(Some(&f), "java"), None);
        assert_eq!(single_required_filename(Some(&HashMap::new()), "cpp"), None);
    }

    #[test]
    fn several_distinct_names_have_no_single_name() {
        let f = format(&[("cpp", &["grader.cpp", "solution.cpp"])]);
        assert_eq!(single_required_filename(Some(&f), "cpp"), None);
    }

    #[test]
    fn repeated_blank_and_padded_names_collapse_to_one() {
        let f = format(&[("cpp", &[" solution.cpp", "", "solution.cpp ", "  "])]);
        assert_eq!(
            single_required_filename(Some(&f), " cpp "),
            Some("solution.cpp")
        );
    }

    #[test]
    fn only_blank_names_have_no_name() {
        let f = format(&[("cpp", &["", "  "])]);
        assert_eq!(single_required_filename(Some(&f), "cpp"), None);
    }
}
