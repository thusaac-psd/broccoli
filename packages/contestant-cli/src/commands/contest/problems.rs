use anyhow::Context;
use clap::Args;
use console::style;

use broccoli_cli_core::client::Client;
use broccoli_cli_core::{config, resolve};

#[derive(Args)]
pub struct ProblemsArgs {
    /// Contest ID or name (e.g. 3 or "Spring Round")
    #[arg(required = true)]
    pub contest: String,

    /// Problem ID, label, index, or title to download (statement + samples)
    #[arg(short = 'p', long)]
    pub problem: Option<String>,
}

pub fn run(args: ProblemsArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);
    let cid = resolve::contest_id(&client, &args.contest)?;

    if let Some(ref problem_ref) = args.problem {
        let pid = resolve::problem_id(&client, &cid, problem_ref)?;
        return download_problem(&client, &cid, &pid);
    }

    println!(
        "{}  Fetching problems for contest {}...",
        style("→").blue().bold(),
        style(&cid).yellow()
    );

    let problems = client
        .list_contest_problems(&cid)
        .context("Failed to fetch contest problems")?;

    if problems.is_empty() {
        println!("  No problems found for this contest.");
        return Ok(());
    }

    println!();
    println!(
        "  {}  Problems in contest {}:",
        style("\u{2697}").cyan(),
        style(&cid).yellow()
    );
    println!();

    for p in &problems {
        println!(
            "    {}  {}",
            style(&p.label).yellow().bold(),
            p.problem_title
        );
    }

    println!();
    println!(
        "  {}",
        style("Download one for offline work: broccoli contest problems <contest> -p <label>")
            .dim()
    );

    Ok(())
}

fn download_problem(client: &Client, contest: &str, problem_id: &str) -> anyhow::Result<()> {
    println!(
        "{}  Downloading problem {} (contest {})...",
        style("→").blue().bold(),
        style(problem_id).yellow(),
        style(contest).yellow()
    );

    let problem = client
        .get_contest_problem(contest, problem_id)
        .context("Failed to fetch problem statement")?;
    let samples = client
        .get_contest_problem_samples(contest, problem_id)
        .context("Failed to fetch sample test cases")?
        .samples;

    let statement = format!(
        "# {} (Problem {})\n\n\
         - Time limit: {} ms\n- Memory limit: {} KB\n\n---\n\n{}\n",
        problem.title, problem_id, problem.time_limit, problem.memory_limit, problem.content
    );

    // Offline cache, where `broccoli test --local` looks for samples.
    let cache = config::problem_cache_dir(contest, problem_id);
    write_problem(&cache, &statement, &samples).context("Failed to write the offline cache")?;

    let dir_name = sanitize_dir_name(&problem.title, problem_id);
    let local = std::env::current_dir()
        .context("Failed to read the current directory")?
        .join(&dir_name);
    let existed = local.exists();
    write_problem(&local, &statement, &samples)
        .with_context(|| format!("Failed to write to {}", local.display()))?;

    let statement_path = local.join("statement.md");
    println!(
        "{}  {} statement + {} sample(s) {} {}",
        style("✓").green().bold(),
        if existed { "Refreshed" } else { "Saved" },
        samples.len(),
        if existed { "in" } else { "to" },
        style(quote_if_spaces(&format!("./{}", dir_name))).cyan()
    );
    println!(
        "  Read it:  {}",
        style(format!(
            "vim {}",
            quote_if_spaces(&statement_path.to_string_lossy())
        ))
        .dim()
    );
    println!(
        "  Test it:  {}",
        style(format!(
            "broccoli test <file> -c {} -p {} --local",
            contest, problem_id
        ))
        .dim()
    );
    Ok(())
}

fn write_problem(
    dir: &std::path::Path,
    statement: &str,
    samples: &[broccoli_cli_core::api::problem::SampleCase],
) -> anyhow::Result<()> {
    let samples_dir = dir.join("samples");
    std::fs::create_dir_all(&samples_dir)?;
    std::fs::write(dir.join("statement.md"), statement)?;
    for (i, s) in samples.iter().enumerate() {
        std::fs::write(samples_dir.join(format!("{}.in", i + 1)), &s.input)?;
        std::fs::write(samples_dir.join(format!("{}.out", i + 1)), &s.output)?;
        // Sample explanation/note (markdown), if the problem author wrote one.
        if let Some(note) = s
            .description
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
        {
            std::fs::write(samples_dir.join(format!("{}.md", i + 1)), note)?;
        }
    }
    Ok(())
}

fn sanitize_dir_name(title: &str, problem_id: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        format!("problem-{}", problem_id)
    } else {
        cleaned.to_string()
    }
}

/// Quote paths with whitespace so shell hints stay copy-pasteable.
fn quote_if_spaces(s: &str) -> String {
    if s.chars().any(char::is_whitespace) {
        format!("\"{}\"", s)
    } else {
        s.to_string()
    }
}
