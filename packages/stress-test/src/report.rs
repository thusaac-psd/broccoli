use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::correctness::CorrectnessOutcome;
use crate::load::LoadOutcome;
use crate::mixed::MixedOutcome;

pub const JSON_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Default)]
pub struct RunSummary {
    pub target_url: String,
    pub duration: Duration,
    pub bootstrap_error: Option<String>,
    pub correctness: Option<CorrectnessOutcome>,
    pub load: Option<LoadSummary>,
    pub mixed: Option<MixedSummary>,
    pub passthrough: PassthroughSummary,
    pub cleanup_warnings: Vec<String>,
    pub log_file: Option<PathBuf>,
    pub dlq_delta: Option<DlqDelta>,
}

#[derive(Debug, Clone)]
pub struct DlqDelta {
    pub baseline_unresolved: u64,
    pub final_unresolved: u64,
    pub new_by_error_code: Vec<(String, u64)>,
}

#[derive(Debug)]
pub struct LoadSummary {
    pub total: u64,
    pub completed: u64,
    pub passed: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
    pub p95_budget_ms: u64,
    pub passed_budget: bool,
    pub error_count: usize,
    pub error_samples: Vec<String>,
    pub passed_overall: bool,
}

#[derive(Debug)]
pub struct MixedSummary {
    pub total: u64,
    pub completed: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    pub max_ms: u64,
    pub error_count: usize,
    pub error_samples: Vec<String>,
    pub by_action: Vec<MixedActionSummary>,
    pub passed_overall: bool,
}

#[derive(Debug)]
pub struct MixedActionSummary {
    pub action: String,
    pub ok: u64,
    pub error: u64,
}

impl LoadSummary {
    pub fn from_outcome(outcome: &LoadOutcome, total: u64, p95_budget_ms: u64) -> Self {
        let error_samples = outcome
            .errors
            .iter()
            .take(5)
            .map(|(_, msg)| msg.clone())
            .collect();
        Self {
            total,
            completed: outcome.completed,
            passed: outcome.passed,
            p50_ms: outcome.histogram.value_at_quantile(0.50),
            p95_ms: outcome.histogram.value_at_quantile(0.95),
            p99_ms: outcome.histogram.value_at_quantile(0.99),
            max_ms: outcome.histogram.max(),
            p95_budget_ms,
            passed_budget: outcome.passed_budget,
            error_count: outcome.errors.len(),
            error_samples,
            passed_overall: outcome.passed_overall,
        }
    }
}

impl MixedSummary {
    pub fn from_outcome(outcome: &MixedOutcome) -> Self {
        let error_samples = outcome
            .errors
            .iter()
            .take(5)
            .map(|(_, msg)| msg.clone())
            .collect();
        let mut by_action: Vec<MixedActionSummary> = outcome
            .by_action
            .iter()
            .map(|(action, stats)| MixedActionSummary {
                action: action.label().to_string(),
                ok: stats.ok,
                error: stats.error,
            })
            .collect();
        by_action.sort_by(|a, b| a.action.cmp(&b.action));

        Self {
            total: outcome.total,
            completed: outcome.completed,
            p50_ms: outcome.histogram.value_at_quantile(0.50),
            p95_ms: outcome.histogram.value_at_quantile(0.95),
            p99_ms: outcome.histogram.value_at_quantile(0.99),
            max_ms: outcome.histogram.max(),
            error_count: outcome.errors.len(),
            error_samples,
            by_action,
            passed_overall: outcome.passed_overall,
        }
    }
}

#[derive(Debug, Default)]
pub enum PassthroughSummary {
    #[default]
    NotRun,
    Skipped {
        reason: String,
    },
    Completed {
        ok: bool,
        count: usize,
    },
}

impl RunSummary {
    pub fn passed(&self) -> bool {
        if self.bootstrap_error.is_some() {
            return false;
        }
        let correctness_ok = self
            .correctness
            .as_ref()
            .is_none_or(|c| c.failed_scenarios.is_empty());
        let load_ok = self.load.as_ref().is_none_or(|l| l.passed_overall);
        let mixed_ok = self.mixed.as_ref().is_none_or(|m| m.passed_overall);
        let passthrough_ok = matches!(
            self.passthrough,
            PassthroughSummary::NotRun
                | PassthroughSummary::Skipped { .. }
                | PassthroughSummary::Completed { ok: true, .. }
        );
        correctness_ok && load_ok && mixed_ok && passthrough_ok
    }

    pub fn to_json(&self, exit_code: u8) -> Value {
        json!({
            "schema_version": JSON_SCHEMA_VERSION,
            "result": if self.passed() { "pass" } else { "fail" },
            "exit_code": exit_code,
            "target_url": self.target_url,
            "duration_seconds": round_duration(self.duration),
            "bootstrap": json!({
                "ok": self.bootstrap_error.is_none(),
                "error": self.bootstrap_error.clone(),
            }),
            "correctness": correctness_json(self.correctness.as_ref()),
            "load": load_json(self.load.as_ref()),
            "mixed": mixed_json(self.mixed.as_ref()),
            "passthrough": passthrough_json(&self.passthrough),
            "cleanup": json!({
                "warnings": self.cleanup_warnings,
            }),
        })
    }
}

fn mixed_json(m: Option<&MixedSummary>) -> Value {
    match m {
        None => Value::Null,
        Some(m) => json!({
            "total": m.total,
            "completed": m.completed,
            "p50_ms": m.p50_ms,
            "p95_ms": m.p95_ms,
            "p99_ms": m.p99_ms,
            "max_ms": m.max_ms,
            "error_count": m.error_count,
            "error_samples": m.error_samples,
            "by_action": m.by_action.iter().map(|a| json!({
                "action": a.action,
                "ok": a.ok,
                "error": a.error,
            })).collect::<Vec<_>>(),
            "passed_overall": m.passed_overall,
        }),
    }
}

fn round_duration(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round() / 1000.0
}

fn correctness_json(c: Option<&CorrectnessOutcome>) -> Value {
    match c {
        None => Value::Null,
        Some(c) => json!({
            "total": c.total,
            "passed": c.passed,
            "failed_scenarios": c.failed_scenarios,
        }),
    }
}

fn load_json(l: Option<&LoadSummary>) -> Value {
    match l {
        None => Value::Null,
        Some(l) => json!({
            "total": l.total,
            "completed": l.completed,
            "passed": l.passed,
            "p50_ms": l.p50_ms,
            "p95_ms": l.p95_ms,
            "p99_ms": l.p99_ms,
            "max_ms": l.max_ms,
            "p95_budget_ms": l.p95_budget_ms,
            "passed_budget": l.passed_budget,
            "error_count": l.error_count,
            "error_samples": l.error_samples,
            "passed_overall": l.passed_overall,
        }),
    }
}

fn passthrough_json(p: &PassthroughSummary) -> Value {
    match p {
        PassthroughSummary::NotRun => json!({
            "state": "not_run",
            "reason": Value::Null,
            "ok": Value::Null,
            "count": Value::Null,
        }),
        PassthroughSummary::Skipped { reason } => json!({
            "state": "skipped",
            "reason": reason,
            "ok": Value::Null,
            "count": Value::Null,
        }),
        PassthroughSummary::Completed { ok, count } => json!({
            "state": "completed",
            "reason": Value::Null,
            "ok": ok,
            "count": count,
        }),
    }
}

struct Pal {
    color: bool,
}

impl Pal {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color {
            format!("\x1b[{}m{}\x1b[0m", code, text)
        } else {
            text.to_string()
        }
    }
    fn green(&self, t: &str) -> String {
        self.paint("32", t)
    }
    fn red(&self, t: &str) -> String {
        self.paint("31", t)
    }
    fn yellow(&self, t: &str) -> String {
        self.paint("33", t)
    }
    fn cyan(&self, t: &str) -> String {
        self.paint("36", t)
    }
    fn dim(&self, t: &str) -> String {
        self.paint("2", t)
    }
    fn bold(&self, t: &str) -> String {
        self.paint("1", t)
    }
    fn green_bold(&self, t: &str) -> String {
        self.paint("1;32", t)
    }
    fn red_bold(&self, t: &str) -> String {
        self.paint("1;31", t)
    }
}

const BOX_W: usize = 66;

fn ruled_top(p: &Pal) -> String {
    p.dim(&format!("┌{}┐", "─".repeat(BOX_W - 2)))
}
fn ruled_bot(p: &Pal) -> String {
    p.dim(&format!("└{}┘", "─".repeat(BOX_W - 2)))
}

pub fn format_summary(summary: &RunSummary, use_color: bool) -> String {
    let p = Pal { color: use_color };
    let mut out = String::new();

    let passed = summary.passed();
    let banner = if passed {
        p.green_bold("PASS ✓")
    } else {
        p.red_bold("FAIL ✗")
    };

    let title = p.bold("BROCCOLI STRESS TEST");
    let title_visible = "BROCCOLI STRESS TEST".len();
    let banner_visible = if passed { "PASS ✓" } else { "FAIL ✗" }.chars().count();
    let pad = BOX_W.saturating_sub(4 + title_visible + banner_visible);
    let _ = writeln!(out, "{}", ruled_top(&p));
    let _ = writeln!(
        out,
        "{} {}{}{} {}",
        p.dim("│"),
        title,
        " ".repeat(pad),
        banner,
        p.dim("│"),
    );
    let _ = writeln!(out, "{}", ruled_bot(&p));
    let _ = writeln!(out);

    let label = |s: &str| p.dim(&format!("  {:<14}", s));
    let _ = writeln!(out, "{}{}", label("Target"), p.cyan(&summary.target_url));
    let _ = writeln!(
        out,
        "{}{:.1}s",
        label("Duration"),
        summary.duration.as_secs_f64(),
    );
    let _ = writeln!(out);

    match &summary.correctness {
        None => {
            let _ = writeln!(
                out,
                "{}{} {}",
                label("Correctness"),
                p.dim("—"),
                p.dim("skipped")
            );
        }
        Some(c) if c.failed_scenarios.is_empty() => {
            let _ = writeln!(
                out,
                "{}{} {} scenarios passed",
                label("Correctness"),
                p.green("✓"),
                p.bold(&format!("{}/{}", c.passed, c.total)),
            );
        }
        Some(c) => {
            let _ = writeln!(
                out,
                "{}{} {} passed   {} {}",
                label("Correctness"),
                p.red("✗"),
                p.bold(&format!("{}/{}", c.passed, c.total)),
                p.dim("failed:"),
                p.red(&c.failed_scenarios.join(", ")),
            );
        }
    }

    match &summary.load {
        None => {
            let _ = writeln!(out, "{}{} {}", label("Load"), p.dim("—"), p.dim("skipped"));
        }
        Some(l) => {
            let glyph = if l.passed_overall {
                p.green("✓")
            } else {
                p.red("✗")
            };
            let counts = p.bold(&format!("{}/{}", l.passed, l.total));
            let budget_text = format!("p95 {}ms / {}ms", l.p95_ms, l.p95_budget_ms);
            let budget = if l.passed_budget {
                p.dim(&budget_text)
            } else {
                p.red(&budget_text)
            };
            let _ = writeln!(
                out,
                "{}{} {} submissions   {}",
                label("Load"),
                glyph,
                counts,
                budget,
            );
        }
    }

    if let Some(m) = &summary.mixed {
        let glyph = if m.passed_overall {
            p.green("✓")
        } else {
            p.red("✗")
        };
        let counts = p.bold(&format!(
            "{}/{}",
            m.completed.saturating_sub(m.error_count as u64),
            m.total
        ));
        let _ = writeln!(
            out,
            "{}{} {} ops   {}",
            label("Mixed"),
            glyph,
            counts,
            p.dim(&format!("p95 {}ms", m.p95_ms)),
        );
    }

    match &summary.passthrough {
        PassthroughSummary::NotRun | PassthroughSummary::Skipped { .. } => {
            let reason = match &summary.passthrough {
                PassthroughSummary::Skipped { reason } => reason.clone(),
                _ => "no --contest-id".into(),
            };
            let _ = writeln!(
                out,
                "{}{} {}",
                label("Pass-through"),
                p.dim("—"),
                p.dim(&format!("skipped ({reason})")),
            );
        }
        PassthroughSummary::Completed { ok, count } => {
            let glyph = if *ok { p.green("✓") } else { p.red("✗") };
            let _ = writeln!(
                out,
                "{}{} {} submissions",
                label("Pass-through"),
                glyph,
                p.bold(&count.to_string()),
            );
        }
    }

    if let Some(l) = &summary.load {
        let _ = writeln!(out);
        let acc = if l.completed == 0 {
            0.0
        } else {
            (l.passed as f64) / (l.completed as f64) * 100.0
        };
        let acc_str = format!("{:.1}%", acc);
        let acc_painted = if (acc - 100.0).abs() < 0.05 {
            p.green(&acc_str)
        } else {
            p.yellow(&acc_str)
        };
        let _ = writeln!(out, "{}{}", label("Accuracy"), acc_painted);
        let err_painted = if l.error_count == 0 {
            p.green("0")
        } else {
            p.red(&l.error_count.to_string())
        };
        let _ = writeln!(out, "{}{}", label("Errors"), err_painted);
    }

    if let Some(m) = &summary.mixed {
        let _ = writeln!(out);
        let err_painted = if m.error_count == 0 {
            p.green("0")
        } else {
            p.red(&m.error_count.to_string())
        };
        let _ = writeln!(out, "{}{}", label("Mixed errors"), err_painted);
    }

    let _ = writeln!(out);

    if passed {
        let _ = writeln!(
            out,
            "  {} {}",
            p.green("✓"),
            p.green_bold("System is ready for contest.")
        );
    } else {
        let _ = writeln!(out, "  {} {}", p.red("✗"), p.red_bold("DO NOT RUN CONTEST"));
        let _ = writeln!(out);
        let bullet = p.dim("  •");
        if let Some(err) = &summary.bootstrap_error {
            let _ = writeln!(out, "{} bootstrap failed: {}", bullet, p.red(err));
        }
        if let Some(c) = &summary.correctness {
            for id in &c.failed_scenarios {
                let _ = writeln!(
                    out,
                    "{} correctness scenario {} did not match expectation",
                    bullet,
                    p.red(id)
                );
            }
        }
        if let Some(l) = &summary.load {
            if l.completed != l.total {
                let _ = writeln!(
                    out,
                    "{} only {} of {} load submissions reached terminal status",
                    bullet,
                    p.bold(&l.completed.to_string()),
                    l.total,
                );
            }
            if l.passed != l.completed {
                let _ = writeln!(
                    out,
                    "{} {} of {} completed load submissions had wrong verdict",
                    bullet,
                    p.red(&(l.completed - l.passed).to_string()),
                    l.completed,
                );
            }
            if !l.passed_budget {
                let _ = writeln!(
                    out,
                    "{} p95 latency {}ms exceeds budget {}ms",
                    bullet,
                    p.red(&format!("{}ms", l.p95_ms)),
                    l.p95_budget_ms,
                );
            }
            if l.error_count > 0 {
                let _ = writeln!(
                    out,
                    "{} {} load failures (verdict mismatches + submit/poll errors)",
                    bullet,
                    p.red(&l.error_count.to_string()),
                );
            }
        }
        if let Some(m) = &summary.mixed {
            if m.completed != m.total {
                let _ = writeln!(
                    out,
                    "{} only {} of {} mixed operations completed",
                    bullet,
                    p.bold(&m.completed.to_string()),
                    m.total,
                );
            }
            if m.error_count > 0 {
                let _ = writeln!(
                    out,
                    "{} {} mixed operation failures",
                    bullet,
                    p.red(&m.error_count.to_string()),
                );
            }
        }
        if let PassthroughSummary::Completed { ok: false, count } = summary.passthrough {
            let _ = writeln!(
                out,
                "{} pass-through phase failed across {} submissions",
                bullet, count,
            );
        }

        if let Some(l) = &summary.load
            && !l.error_samples.is_empty()
        {
            let _ = writeln!(out);
            let header = if l.error_count > l.error_samples.len() {
                format!(
                    "First {} of {} load failures:",
                    l.error_samples.len(),
                    l.error_count,
                )
            } else {
                "Load failures:".to_string()
            };
            let _ = writeln!(out, "  {}", p.yellow(&header));
            for sample in &l.error_samples {
                let _ = writeln!(out, "  {} {}", p.dim("•"), p.dim(sample));
            }
        }

        if let Some(m) = &summary.mixed
            && !m.error_samples.is_empty()
        {
            let _ = writeln!(out);
            let header = if m.error_count > m.error_samples.len() {
                format!(
                    "First {} of {} mixed failures:",
                    m.error_samples.len(),
                    m.error_count,
                )
            } else {
                "Mixed failures:".to_string()
            };
            let _ = writeln!(out, "  {}", p.yellow(&header));
            for sample in &m.error_samples {
                let _ = writeln!(out, "  {} {}", p.dim("•"), p.dim(sample));
            }
        }

        if let Some(path) = &summary.log_file {
            let _ = writeln!(out);
            let _ = writeln!(
                out,
                "  {} {}",
                p.dim("Full log:"),
                p.cyan(&path.display().to_string()),
            );
        }
    }

    if let Some(d) = &summary.dlq_delta
        && d.final_unresolved > d.baseline_unresolved
    {
        let new_total = d.final_unresolved - d.baseline_unresolved;
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "  {} {} new dead-letter entries during this run",
            p.yellow("⚠"),
            p.red(&new_total.to_string()),
        );
        for (code, count) in &d.new_by_error_code {
            let _ = writeln!(
                out,
                "  {} {}: {}",
                p.dim("•"),
                p.dim(code),
                p.bold(&count.to_string()),
            );
        }
    }

    if !summary.cleanup_warnings.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "  {}", p.yellow("Cleanup warnings:"));
        for w in &summary.cleanup_warnings {
            let _ = writeln!(out, "  {} {}", p.dim("•"), p.yellow(w));
        }
    }

    let _ = writeln!(out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_summary() -> RunSummary {
        RunSummary {
            target_url: "http://localhost:3000".into(),
            duration: Duration::from_secs_f64(21.7),
            bootstrap_error: None,
            correctness: Some(CorrectnessOutcome {
                total: 9,
                passed: 9,
                failed_scenarios: vec![],
            }),
            load: Some(LoadSummary {
                total: 200,
                completed: 200,
                passed: 200,
                p50_ms: 820,
                p95_ms: 2104,
                p99_ms: 3401,
                max_ms: 4012,
                p95_budget_ms: 15000,
                passed_budget: true,
                error_count: 0,
                error_samples: vec![],
                passed_overall: true,
            }),
            mixed: None,
            passthrough: PassthroughSummary::NotRun,
            cleanup_warnings: vec![],
            log_file: None,
            dlq_delta: None,
        }
    }

    #[test]
    fn to_json_passing_run_has_expected_shape() {
        let s = pass_summary();
        let v = s.to_json(0);
        assert_eq!(v["schema_version"], 1);
        assert_eq!(v["result"], "pass");
        assert_eq!(v["exit_code"], 0);
        assert_eq!(v["target_url"], "http://localhost:3000");
        assert_eq!(v["duration_seconds"], 21.7);
        assert_eq!(v["bootstrap"]["ok"], true);
        assert!(v["bootstrap"]["error"].is_null());
        assert_eq!(v["correctness"]["total"], 9);
        assert_eq!(v["correctness"]["passed"], 9);
        assert_eq!(
            v["correctness"]["failed_scenarios"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
        assert_eq!(v["load"]["p95_ms"], 2104);
        assert_eq!(v["load"]["passed_budget"], true);
        assert_eq!(v["passthrough"]["state"], "not_run");
        assert!(v["passthrough"]["ok"].is_null());
        assert_eq!(v["cleanup"]["warnings"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn to_json_correctness_failure_lists_scenarios() {
        let mut s = pass_summary();
        s.correctness = Some(CorrectnessOutcome {
            total: 9,
            passed: 8,
            failed_scenarios: vec!["abcppmle".into()],
        });
        let v = s.to_json(1);
        assert_eq!(v["result"], "fail");
        assert_eq!(v["exit_code"], 1);
        assert_eq!(v["correctness"]["passed"], 8);
        assert_eq!(v["correctness"]["failed_scenarios"][0], "abcppmle");
    }

    #[test]
    fn to_json_skipped_phases_are_null() {
        let s = RunSummary {
            target_url: "http://x".into(),
            duration: Duration::from_secs(1),
            bootstrap_error: None,
            correctness: None,
            load: None,
            mixed: None,
            passthrough: PassthroughSummary::Skipped {
                reason: "no contestid".into(),
            },
            cleanup_warnings: vec![],
            log_file: None,
            dlq_delta: None,
        };
        let v = s.to_json(0);
        assert!(v["correctness"].is_null());
        assert!(v["load"].is_null());
        assert_eq!(v["passthrough"]["state"], "skipped");
        assert_eq!(v["passthrough"]["reason"], "no contestid");
    }

    #[test]
    fn to_json_passthrough_completed_includes_ok_and_count() {
        let mut s = pass_summary();
        s.passthrough = PassthroughSummary::Completed {
            ok: false,
            count: 20,
        };
        let v = s.to_json(3);
        assert_eq!(v["passthrough"]["state"], "completed");
        assert_eq!(v["passthrough"]["ok"], false);
        assert_eq!(v["passthrough"]["count"], 20);
        assert!(v["passthrough"]["reason"].is_null());
    }

    #[test]
    fn to_json_bootstrap_error_surfaces_in_object() {
        let mut s = pass_summary();
        s.bootstrap_error = Some("network down".into());
        s.correctness = None;
        s.load = None;
        let v = s.to_json(4);
        assert_eq!(v["result"], "fail");
        assert_eq!(v["bootstrap"]["ok"], false);
        assert_eq!(v["bootstrap"]["error"], "network down");
    }

    #[test]
    fn to_json_cleanup_warnings_pass_through() {
        let mut s = pass_summary();
        s.cleanup_warnings = vec!["could not delete problem 17".into()];
        let v = s.to_json(5);
        assert_eq!(v["cleanup"]["warnings"][0], "could not delete problem 17");
    }
}
