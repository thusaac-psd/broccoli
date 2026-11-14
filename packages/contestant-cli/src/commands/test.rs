use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use clap::Args;
use console::style;

use broccoli_cli_core::api::code_run::{CodeRunResponse, CustomTestCaseInput};
use broccoli_cli_core::api::submission::SubmissionFileDto;
use broccoli_cli_core::client::Client;
use broccoli_cli_core::config::{self, load_user_config};
use broccoli_cli_core::model::{StatusDisplay, SubmissionStatus, VerdictDisplay};
use broccoli_cli_core::resolve;

use super::context;

#[derive(Args)]
pub struct TestArgs {
    /// Source files to test
    #[arg(required = true)]
    pub files: Vec<String>,

    /// Problem label, position, title or #<id> (e.g. A, 1, "Two Sum" or #42)
    #[arg(short = 'p', long)]
    pub problem: Option<String>,

    /// Contest ID or name (e.g. 3 or "Spring Round")
    #[arg(short = 'c', long)]
    pub contest: Option<String>,

    /// Programming language (auto-detected from file extensions if omitted)
    #[arg(short = 'l', long)]
    pub language: Option<String>,

    /// Input file for local-only testing
    #[arg(short = 'i', long)]
    pub input: Option<String>,

    /// Run tests locally instead of against the server
    #[arg(long, default_value = "false")]
    pub local: bool,
}

pub fn run(args: TestArgs) -> anyhow::Result<()> {
    let creds = config::resolve_credentials(None, None)?;
    let client = Client::new(creds);
    let user_config = load_user_config();
    let ctx = context::discover_context();

    let language = context::resolve_language(&args.files, args.language.as_deref(), || {
        context::server_languages(&client)
    })?;

    let mut files = Vec::new();
    for path in &args.files {
        let content =
            fs::read_to_string(path).with_context(|| format!("Failed to read: {}", path))?;
        let filename = std::path::Path::new(path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        files.push(SubmissionFileDto { filename, content });
    }

    if let Some(ref input_path) = args.input {
        let input = fs::read_to_string(input_path)
            .with_context(|| format!("Failed to read input file: {}", input_path))?;
        println!("{}  Running locally...", style("→").blue().bold());
        let output = run_local(&files, &language, &input, &user_config)?;
        println!("  Output:\n{}", output);
        return Ok(());
    }

    let contest_ref = args
        .contest
        .as_deref()
        .or(ctx.as_ref().and_then(|c| c.contest.as_deref()))
        .or(user_config.contest.as_deref())
        .context("No contest specified. Use -c <id|name>.")?;
    let problem_ref = args
        .problem
        .as_deref()
        .or(ctx.as_ref().and_then(|c| c.problem.as_deref()))
        .context("No problem specified. Use -p <id|label> (e.g. -p A).")?;

    // --local works offline from the cached samples: a contest id and a
    // `#<id>` problem reference need no server to resolve.
    let resolved = resolve::contest_id(&client, contest_ref).and_then(|c| {
        let p = resolve::problem(&client, &c, problem_ref)?;
        println!("{}  Problem {}", style("→").blue().bold(), style(&p).cyan());
        Ok((c, p.id.to_string()))
    });
    let (contest_id, problem_id) = match resolved {
        Ok(ids) => ids,
        Err(e) if args.local => {
            let offline = contest_ref
                .trim()
                .parse::<i64>()
                .ok()
                .zip(resolve::explicit_problem_id(problem_ref));
            match offline {
                Some((c, p)) => (c.to_string(), p.to_string()),
                None => {
                    return Err(e.context(
                        "Offline, --local needs a numeric contest id and a #<id> problem reference",
                    ));
                }
            }
        }
        Err(e) => return Err(e),
    };
    let contest_id = contest_id.as_str();
    let problem_id = problem_id.as_str();

    println!(
        "{}  Fetching sample test cases...",
        style("→").blue().bold()
    );
    let samples = match client.get_contest_problem_samples(contest_id, problem_id) {
        Ok(resp) => resp.samples,
        Err(e) if args.local => match load_cached_samples(contest_id, problem_id) {
            Some(cached) => {
                println!("  {}", style("(offline — using cached samples)").yellow());
                cached
            }
            None => {
                return Err(e).context(
                    "Could not reach the server and no cached samples found. \
                     Run `broccoli contest problems <contest> -p <problem>` while online first.",
                );
            }
        },
        Err(e) => return Err(e).context("Failed to fetch sample test cases"),
    };

    if samples.is_empty() {
        bail!("No sample test cases are available for this problem.");
    }

    if args.local {
        println!(
            "{}  Running {} sample(s) locally...",
            style("→").blue().bold(),
            samples.len()
        );
        run_samples_locally(&files, &language, &samples, &user_config)?;
        return Ok(());
    }

    // Server hard-caps custom_test_cases at 10 (code_run.rs); cap the remote path
    // to match and warn. `--local` (above) runs all samples.
    const MAX_REMOTE_SAMPLES: usize = 10;
    let selected: &[_] = if samples.len() > MAX_REMOTE_SAMPLES {
        println!(
            "{}  This problem has {} samples; the server accepts at most {} per remote run. Testing the first {} — use --local to run all.",
            style("!").yellow().bold(),
            samples.len(),
            MAX_REMOTE_SAMPLES,
            MAX_REMOTE_SAMPLES
        );
        &samples[..MAX_REMOTE_SAMPLES]
    } else {
        &samples
    };

    println!(
        "{}  Running {} sample(s) on the server...",
        style("→").blue().bold(),
        selected.len()
    );
    let custom: Vec<CustomTestCaseInput> = selected
        .iter()
        .map(|s| CustomTestCaseInput {
            input: s.input.clone(),
            expected_output: Some(s.output.clone()),
        })
        .collect();

    let created = client
        .run_contest_code(contest_id, problem_id, files, &language.id, custom)
        .context("Failed to start code run")?;
    let result = poll_code_run(&client, created.id)?;
    print_remote_result(&result)?;

    Ok(())
}

/// Load sample test cases from the offline cache; `None` if absent.
fn load_cached_samples(
    contest_id: &str,
    problem_id: &str,
) -> Option<Vec<broccoli_cli_core::api::problem::SampleCase>> {
    let samples_dir = config::problem_cache_dir(contest_id, problem_id).join("samples");
    let mut cases = Vec::new();
    for i in 1.. {
        let in_path = samples_dir.join(format!("{}.in", i));
        let out_path = samples_dir.join(format!("{}.out", i));
        match (fs::read_to_string(&in_path), fs::read_to_string(&out_path)) {
            (Ok(input), Ok(output)) => {
                let description = fs::read_to_string(samples_dir.join(format!("{}.md", i))).ok();
                cases.push(broccoli_cli_core::api::problem::SampleCase {
                    input,
                    output,
                    description,
                })
            }
            _ => break,
        }
    }
    if cases.is_empty() { None } else { Some(cases) }
}

fn poll_code_run(client: &Client, run_id: i32) -> anyhow::Result<CodeRunResponse> {
    use std::io::Write;
    use std::time::{Duration, Instant};
    let mut spinner = broccoli_cli_core::tui::spinner::Spinner::new();
    let start = Instant::now();

    loop {
        let cr = client.get_code_run(run_id)?;
        let status = cr.status;
        if !status.is_in_progress() {
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
            return Ok(cr);
        }
        if start.elapsed() > Duration::from_secs(120) {
            eprint!("\r\x1b[K");
            std::io::stderr().flush().ok();
            bail!("Timed out waiting for the code run to finish.");
        }
        eprint!(
            "\r\x1b[K  {} Running... {} ({:.0}s)",
            spinner.next(),
            status.human(),
            start.elapsed().as_secs_f64()
        );
        std::io::stderr().flush().ok();
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn run_samples_locally(
    files: &[SubmissionFileDto],
    language: &context::Language,
    samples: &[broccoli_cli_core::api::problem::SampleCase],
    config: &broccoli_cli_core::config::UserConfig,
) -> anyhow::Result<()> {
    let mut passed = 0usize;
    for (i, sample) in samples.iter().enumerate() {
        match run_local(files, language, &sample.input, config) {
            Ok(output) => {
                let got = output.trim_end();
                let want = sample.output.trim_end();
                if got == want {
                    println!("  Sample {}: {} PASSED", i + 1, style("✓").green());
                    passed += 1;
                } else {
                    println!("  Sample {}: {} FAILED", i + 1, style("✗").red());
                    print_diff(want, got);
                }
            }
            Err(e) => {
                println!("  Sample {}: {} ERROR — {}", i + 1, style("✗").red(), e);
            }
        }
    }
    println!("  {}/{} samples passed.", passed, samples.len());
    Ok(())
}

fn print_diff(expected: &str, got: &str) {
    const CAP: usize = 40; // keep big outputs from flooding the terminal
    let exp: Vec<&str> = expected.lines().collect();
    let act: Vec<&str> = got.lines().collect();
    let max = exp.len().max(act.len());

    println!(
        "    diff ({} / {}):",
        style("- expected").red(),
        style("+ got").green()
    );
    let mut shown = 0usize;
    for i in 0..max {
        if shown >= CAP {
            // count rendered rows (differing pair = two rows) so the hint isn't an undercount
            let remaining: usize = (i..max)
                .map(|j| match (exp.get(j), act.get(j)) {
                    (Some(e), Some(a)) if e == a => 1,
                    (Some(_), Some(_)) => 2,
                    _ => 1,
                })
                .sum();
            println!(
                "    {}",
                style(format!("… {} more line(s)", remaining)).dim()
            );
            break;
        }
        match (exp.get(i), act.get(i)) {
            (Some(e), Some(a)) if e == a => {
                println!("      {}", style(format!("  {}", e)).dim());
                shown += 1;
            }
            (e, a) => {
                if let Some(e) = e {
                    println!("    {}", style(format!("- {}", e)).red());
                    shown += 1;
                }
                if let Some(a) = a {
                    println!("    {}", style(format!("+ {}", a)).green());
                    shown += 1;
                }
            }
        }
    }
    if exp.len() != act.len() {
        println!(
            "    {}",
            style(format!(
                "(expected {} line(s), got {})",
                exp.len(),
                act.len()
            ))
            .dim()
        );
    }
}

/// Temp working dir removed on drop, so runs never leak files on early return.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> anyhow::Result<Self> {
        // unpredictable name avoids collisions on a shared temp dir
        let nonce: u64 = rand::random();
        let path = std::env::temp_dir().join(format!(
            "broccoli-test-{}-{:016x}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&path)
            .with_context(|| format!("Failed to create temp dir: {}", path.display()))?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn run_local(
    files: &[SubmissionFileDto],
    language: &context::Language,
    input: &str,
    config: &broccoli_cli_core::config::UserConfig,
) -> anyhow::Result<String> {
    // template is trusted user config (may contain `compile && run`), so run via shell;
    // stdin is piped directly to keep untrusted paths/filenames out of the command string
    let cmd_template = config
        .runtimes
        .get(&language.id)
        .cloned()
        .unwrap_or_else(|| default_runtime(&language.id));

    let main_file = files
        .iter()
        .find(|file| language.claims(&file.filename))
        .or_else(|| files.first())
        .map(|f| &f.filename)
        .context("No source files provided")?;

    let scratch = ScratchDir::new()?;
    let tmp = scratch.path();

    for file in files {
        fs::write(tmp.join(&file.filename), &file.content)?;
    }

    // shell-quote substituted paths; {file} derives from a filename that could
    // contain shell metacharacters (e.g. `a;rm -rf ~.cpp`)
    let cmd_str = cmd_template
        .replace(
            "{file}",
            &shell_quote(&tmp.join(main_file).to_string_lossy()),
        )
        .replace("{outdir}", &shell_quote(&tmp.to_string_lossy()));

    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", &cmd_str]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", &cmd_str]);
        c
    };

    let mut child = command
        .current_dir(tmp)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to execute. Is the runtime installed?")?;

    // feed stdin on a separate thread to avoid deadlock if the program fills the stdout pipe
    if let Some(mut stdin) = child.stdin.take() {
        let input_bytes = input.as_bytes().to_vec();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input_bytes);
        });
    }

    let output = child
        .wait_with_output()
        .context("Failed to wait for program")?;

    if !output.status.success() {
        bail!(
            "Runtime error:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Quote a path for safe interpolation into a shell command string.
fn shell_quote(s: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", s.replace('"', ""))
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn default_runtime(lang: &str) -> String {
    match lang {
        "python3" | "python" => "python3 {file}".into(),
        "cpp" => "g++ -O2 {file} -o {outdir}/a.out && {outdir}/a.out".into(),
        "c" => "gcc -O2 {file} -o {outdir}/a.out && {outdir}/a.out".into(),
        "java" => "javac {file} -d {outdir} && java -cp {outdir} Main".into(),
        "rust" => "rustc -O {file} -o {outdir}/a.out && {outdir}/a.out".into(),
        "go" => "go run {file}".into(),
        "javascript" => "node {file}".into(),
        _ => "python3 {file}".into(),
    }
}

fn print_remote_result(run: &CodeRunResponse) -> anyhow::Result<()> {
    let judge = run.result.as_ref();

    if let Some(compile) = judge.and_then(|r| r.compile_output.as_deref())
        && !compile.trim().is_empty()
    {
        println!("  {} Compilation output:", style("!").yellow());
        for line in compile.lines() {
            println!("    {}", style(line).dim());
        }
    }

    if run.status == SubmissionStatus::CompilationError {
        bail!("Compilation failed.");
    }

    let cases = judge.map(|r| r.test_case_results.as_slice()).unwrap_or(&[]);
    if cases.is_empty() {
        // A run that stopped without producing results is an error, not a success:
        // exiting 0 here would tell the contestant "finished" with zero information.
        bail!(
            "Code run ended with status {} but returned no results.",
            run.status.human()
        );
    }
    let mut passed = 0usize;
    for (i, case) in cases.iter().enumerate() {
        if case.verdict.is_accepted() {
            println!("  Sample {}: {} PASSED", i + 1, style("\u{2713}").green());
            passed += 1;
        } else {
            println!(
                "  Sample {}: {} {} — got {:?}",
                i + 1,
                style("\u{2717}").red(),
                case.verdict.human(),
                case.stdout.as_deref().unwrap_or("").trim()
            );
        }
    }
    println!("  {}/{} passed.", passed, cases.len());

    Ok(())
}
