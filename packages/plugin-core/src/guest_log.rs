//! Every log line a plugin causes, through one sink: the lines a guest logs
//! (extism's `log_*` imports, linked into every instance, and the `logger`
//! capability's `log_info`), and every host line that carries a plugin's
//! text (a failed call's reason, a route's server-error body: [`about`]).
//!
//! A plugin cannot flood the host's log or speak as the host:
//!
//! - each line names the plugin (`plugin_id`), the Broccoli id of the call
//!   running on the thread, not extism's instance id;
//! - a plugin's text draws nothing of its own: every control character
//!   (a line break, a carriage return, an ANSI escape) is written as its
//!   escape, so the text and pretty log formats show one line the host
//!   wrote ([`plugin_text`]);
//! - each line is at most [`MAX_LINE_BYTES`], cut at a character boundary
//!   with the number of bytes left out;
//! - each plugin has a budget of [`BURST`] lines, refilled at [`PER_SECOND`]
//!   a second: a line over it is dropped, the first drop says so once, and
//!   the next line admitted says how many were dropped. A host line about a
//!   plugin (a failed call, a route's server error: [`about`]) has a budget
//!   of its own, so a plugin flooding its own lines never hides them;
//! - a guest's lines go to the [`TARGET`] target, at most at `WARN`: a
//!   guest's `error` is logged as a `WARN` with `guest_level = "error"`, so
//!   no plugin writes a line that reads as the host's own error.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use tracing::Level;

/// The longest line a plugin causes, in bytes.
pub const MAX_LINE_BYTES: usize = 2048;
/// The lines a plugin may log at once before its rate applies.
pub const BURST: u32 = 100;
/// The lines a second a plugin may log beyond its burst.
pub const PER_SECOND: u32 = 20;
/// The tracing target of every line a guest logs.
pub const TARGET: &str = "plugin";

/// Send the lines guests log through extism's `log_*` imports here. Every
/// instance's builder calls it; the first call installs the sink.
pub(crate) fn install() {
    extism::set_guest_log(from_extism);
}

fn from_extism(level: Level, message: &str) {
    let plugin_id = crate::executor::current_plugin_id();
    emit(plugin_id.as_deref().unwrap_or("<unknown>"), level, message);
}

/// Log `message`, which `plugin_id`'s guest logged at `level`.
pub fn emit(plugin_id: &str, level: Level, message: &str) {
    LIMITS.emit(plugin_id, level, message, Instant::now());
}

/// Log, as the host, `what` happened to `plugin_id`, with `text` the plugin
/// gave (a call's failure reason, a route's answer): a warning to the
/// [`TARGET`] target naming the plugin, with `text` as [`plugin_text`]
/// makes it, under the plugin's budget for host lines. Every host line that
/// carries a plugin's text goes through here, so none reads as the host's
/// own error or floods the log.
pub fn about(plugin_id: &str, what: &str, text: &str) {
    LIMITS.about(plugin_id, what, text, Instant::now());
}

/// Log that `plugin_id`'s call of `func_name` (at call depth `depth`)
/// failed for `reason`, already [`plugin_text`]: a host line about the
/// plugin, under its budget for those.
pub(crate) fn failed_call(plugin_id: &str, func_name: &str, depth: u8, reason: &str) {
    if LIMITS.admit(plugin_id, Source::Host, Instant::now()) {
        tracing::warn!(
            target: TARGET,
            plugin_id,
            func_name,
            depth,
            reason,
            "Plugin call failed"
        );
    }
}

/// `message` as a log line may carry a plugin's text: every control
/// character escaped (`\n`, `\r`, `\t`, `\u{1b}` and the like), then cut
/// to at most [`MAX_LINE_BYTES`] at a character boundary, saying how many
/// bytes were left out.
pub fn plugin_text(message: &str) -> Cow<'_, str> {
    let escaped = if message.chars().any(char::is_control) {
        Cow::Owned(message.chars().fold(String::new(), |mut out, c| {
            match c {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c.is_control() => out.push_str(&c.escape_unicode().to_string()),
                c => out.push(c),
            }
            out
        }))
    } else {
        Cow::Borrowed(message)
    };
    if escaped.len() <= MAX_LINE_BYTES {
        return escaped;
    }
    let mut end = MAX_LINE_BYTES;
    while !escaped.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!(
        "{}... [{} more bytes]",
        &escaped[..end],
        escaped.len() - end
    ))
}

static LIMITS: LazyLock<Limits> = LazyLock::new(Limits::default);

/// Whose line a plugin's budget is spent on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Source {
    /// A line the guest logged.
    Guest,
    /// A host line about the plugin ([`about`]).
    Host,
}

/// Each plugin's line budgets, one per [`Source`].
#[derive(Default)]
struct Limits(Mutex<HashMap<(String, Source), Budget>>);

struct Budget {
    /// Lines the plugin may log now, up to [`BURST`].
    lines: f64,
    /// When `lines` was last refilled.
    refilled: Instant,
    /// Lines dropped since the last one admitted.
    dropped: u64,
}

/// What [`Limits::take`] decided for one line.
#[derive(Debug, PartialEq, Eq)]
enum Taken {
    /// Log it, after saying how many lines were dropped before it.
    Line { dropped_before: u64 },
    /// Drop it; `first` when it is the first since a line was admitted.
    Dropped { first: bool },
}

impl Limits {
    fn take(&self, plugin_id: &str, source: Source, now: Instant) -> Taken {
        let mut budgets = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let budget = budgets
            .entry((plugin_id.to_string(), source))
            .or_insert_with(|| Budget {
                lines: f64::from(BURST),
                refilled: now,
                dropped: 0,
            });
        let elapsed = now.saturating_duration_since(budget.refilled).as_secs_f64();
        budget.lines = (budget.lines + elapsed * f64::from(PER_SECOND)).min(f64::from(BURST));
        budget.refilled = budget.refilled.max(now);
        if budget.lines >= 1.0 {
            budget.lines -= 1.0;
            Taken::Line {
                dropped_before: std::mem::take(&mut budget.dropped),
            }
        } else {
            budget.dropped += 1;
            Taken::Dropped {
                first: budget.dropped == 1,
            }
        }
    }

    fn admit(&self, plugin_id: &str, source: Source, now: Instant) -> bool {
        match self.take(plugin_id, source, now) {
            Taken::Line { dropped_before } => {
                if dropped_before > 0 {
                    tracing::warn!(
                        target: TARGET,
                        plugin_id,
                        dropped = dropped_before,
                        "Dropped lines this plugin logged over its rate"
                    );
                }
                true
            }
            Taken::Dropped { first: true } => {
                tracing::warn!(
                    target: TARGET,
                    plugin_id,
                    burst = BURST,
                    per_second = PER_SECOND,
                    "This plugin logs over its rate; its lines are dropped until it slows down"
                );
                false
            }
            Taken::Dropped { first: false } => false,
        }
    }

    fn about(&self, plugin_id: &str, what: &str, text: &str, now: Instant) {
        if !self.admit(plugin_id, Source::Host, now) {
            return;
        }
        let text = plugin_text(text);
        let text = text.as_ref();
        tracing::warn!(target: TARGET, plugin_id, text, "{what}");
    }

    fn emit(&self, plugin_id: &str, level: Level, message: &str, now: Instant) {
        if !self.admit(plugin_id, Source::Guest, now) {
            return;
        }
        let message = plugin_text(message);
        let message = message.as_ref();
        let guest_level = level.as_str().to_ascii_lowercase();
        let guest_level = guest_level.as_str();
        match level {
            Level::ERROR | Level::WARN => {
                tracing::warn!(target: TARGET, plugin_id, guest_level, "{message}")
            }
            Level::INFO => tracing::info!(target: TARGET, plugin_id, guest_level, "{message}"),
            Level::DEBUG => tracing::debug!(target: TARGET, plugin_id, guest_level, "{message}"),
            Level::TRACE => tracing::trace!(target: TARGET, plugin_id, guest_level, "{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_line_is_cut_at_a_character_boundary_and_says_how_much_was_left_out() {
        assert_eq!(plugin_text("short"), "short");
        let long = "é".repeat(MAX_LINE_BYTES); // two bytes each
        let cut = plugin_text(&long);
        assert!(cut.len() < MAX_LINE_BYTES + 32, "{}", cut.len());
        assert!(
            cut.ends_with(&format!("... [{} more bytes]", MAX_LINE_BYTES)),
            "{}",
            &cut[cut.len() - 40..]
        );
    }

    #[test]
    fn a_plugins_text_cannot_draw_lines_or_colours_of_its_own() {
        assert_eq!(
            plugin_text("one\nINFO server: forged\r\x1b[31mred\x1b[0m\tok\u{85}"),
            "one\\nINFO server: forged\\r\\u{1b}[31mred\\u{1b}[0m\\tok\\u{85}"
        );
        // Escaping happens before the cut, so the line stays within its
        // bound however many escapes the text needs.
        let breaks = "\n".repeat(MAX_LINE_BYTES);
        let cut = plugin_text(&breaks);
        assert!(cut.len() < MAX_LINE_BYTES + 32, "{}", cut.len());
        assert!(!cut.contains('\n'));
    }

    /// A host line about a plugin (its call failed, its route answered a
    /// server error) has a budget of its own, so a plugin that floods its
    /// own lines cannot hide the host's lines about it.
    #[test]
    fn a_plugin_over_its_rate_still_has_the_hosts_lines_about_it_logged() {
        let limits = Limits::default();
        let start = Instant::now();
        for _ in 0..BURST {
            assert_eq!(
                limits.take("p", Source::Guest, start),
                Taken::Line { dropped_before: 0 }
            );
        }
        assert_eq!(
            limits.take("p", Source::Guest, start),
            Taken::Dropped { first: true }
        );
        assert_eq!(
            limits.take("p", Source::Host, start),
            Taken::Line { dropped_before: 0 }
        );
    }

    #[test]
    fn a_host_line_about_a_plugin_is_a_bounded_warning_naming_it() {
        let logs = crate::test_logs::captured();
        let limits = Limits::default();
        limits.about(
            "about-me",
            "A plugin route answered a server error",
            &format!("{}\nERROR forged", "x".repeat(4000)),
            Instant::now(),
        );
        let logs = logs.lock();
        let line = logs
            .iter()
            .find(|l| l.text.contains("plugin_id=about-me"))
            .expect("the line is logged");
        assert_eq!(line.level, Level::WARN);
        assert_eq!(line.target, TARGET);
        assert!(line.text.contains("A plugin route answered a server error"));
        assert!(line.text.contains("more bytes]"), "{}", line.text);
        assert!(!line.text.contains('\n'), "{}", line.text);
    }

    #[test]
    fn a_plugin_logs_its_burst_then_at_its_rate_and_hears_once_that_lines_dropped() {
        let limits = Limits::default();
        let start = Instant::now();
        for _ in 0..BURST {
            assert_eq!(
                limits.take("p", Source::Guest, start),
                Taken::Line { dropped_before: 0 }
            );
        }
        assert_eq!(
            limits.take("p", Source::Guest, start),
            Taken::Dropped { first: true }
        );
        assert_eq!(
            limits.take("p", Source::Guest, start),
            Taken::Dropped { first: false }
        );
        // Another plugin has a budget of its own.
        assert_eq!(
            limits.take("q", Source::Guest, start),
            Taken::Line { dropped_before: 0 }
        );
        // One line's worth of time later, one more line, which reports the
        // two dropped before it.
        let later = start + Duration::from_secs(1) / PER_SECOND;
        assert_eq!(
            limits.take("p", Source::Guest, later),
            Taken::Line { dropped_before: 2 }
        );
        assert_eq!(
            limits.take("p", Source::Guest, later),
            Taken::Dropped { first: true }
        );
        // A quiet plugin refills only up to its burst.
        let much_later = later + Duration::from_secs(3600);
        for _ in 0..BURST {
            assert!(matches!(
                limits.take("p", Source::Guest, much_later),
                Taken::Line { .. }
            ));
        }
        assert_eq!(
            limits.take("p", Source::Guest, much_later),
            Taken::Dropped { first: true }
        );
    }

    #[test]
    fn a_plugin_over_its_rate_is_told_once_and_then_how_many_lines_it_lost() {
        let logs = crate::test_logs::captured();
        let limits = Limits::default();
        let start = Instant::now();
        for _ in 0..BURST + 2 {
            limits.emit("rated", Level::INFO, "a line", start);
        }
        limits.emit(
            "rated",
            Level::INFO,
            "a line",
            start + Duration::from_secs(1) / PER_SECOND,
        );
        let logs = logs.lock();
        let rated: Vec<_> = logs
            .iter()
            .filter(|l| l.text.contains("plugin_id=rated"))
            .collect();
        let count = |what: &str| rated.iter().filter(|l| l.text.contains(what)).count();
        assert_eq!(count("message=a line"), BURST as usize + 1);
        assert_eq!(count("logs over its rate"), 1);
        let lost: Vec<_> = rated
            .iter()
            .filter(|l| l.text.contains("Dropped lines"))
            .collect();
        assert_eq!(lost.len(), 1, "{lost:#?}");
        assert!(lost[0].text.contains("dropped=2"), "{}", lost[0].text);
        assert!(rated.iter().all(|l| l.target == TARGET));
    }
}
