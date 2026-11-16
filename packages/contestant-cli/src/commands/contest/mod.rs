pub mod info;
pub mod list;
pub mod problems;
pub mod register;

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct ContestArgs {
    #[command(subcommand)]
    pub command: ContestCommand,
}

#[derive(Subcommand)]
pub enum ContestCommand {
    /// List available contests
    #[command(visible_alias = "ls")]
    List,
    /// Show contest details and your registration status
    #[command(visible_alias = "i", alias = "show")]
    Info(InfoArgs),
    /// Register for a contest
    #[command(visible_alias = "reg")]
    Register(RegisterArgs),
    /// Unregister from a contest
    #[command(visible_alias = "unreg")]
    Unregister(RegisterArgs),
    /// List or download contest problems
    #[command(visible_alias = "p", alias = "probs")]
    Problems(ProblemsArgs),
}

pub use info::InfoArgs;
pub use problems::ProblemsArgs;
pub use register::RegisterArgs;

use broccoli_cli_core::api::contest::ContestPhase;
use chrono::{DateTime, TimeDelta, Utc};

/// How a contest's phase reads in the terminal.
pub fn phase_label(phase: ContestPhase) -> &'static str {
    match phase {
        ContestPhase::Unactivated => "Not yet visible",
        ContestPhase::Upcoming => "Upcoming",
        ContestPhase::Running => "Running",
        ContestPhase::Ended => "Ended",
        ContestPhase::Deactivated => "Deactivated",
    }
}

/// Where a contest's timeline stands at `now`, the server's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Countdown {
    /// Time left until the start.
    StartsIn(TimeDelta),
    /// Time left until the end.
    EndsIn(TimeDelta),
    Ended,
}

impl Countdown {
    pub fn at(start: DateTime<Utc>, end: DateTime<Utc>, now: DateTime<Utc>) -> Self {
        if now < start {
            Self::StartsIn(start - now)
        } else if now < end {
            Self::EndsIn(end - now)
        } else {
            Self::Ended
        }
    }
}

/// A timestamp in this machine's time zone.
pub fn local_time(ts: &DateTime<Utc>) -> String {
    ts.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

/// `1d 2h 3m`, `2h 3m` or `3m`: a span to the minute.
pub fn span_minutes(span: TimeDelta) -> String {
    let (days, hours, minutes) = (
        span.num_days(),
        span.num_hours() % 24,
        span.num_minutes() % 60,
    );
    if days > 0 {
        format!("{days}d {hours}h {minutes}m")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else {
        format!("{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn the_countdown_targets_the_start_then_the_end() {
        let (start, end) = (ts("2030-01-01T10:00:00Z"), ts("2030-01-01T15:00:00Z"));
        assert_eq!(
            Countdown::at(start, end, ts("2030-01-01T09:00:00Z")),
            Countdown::StartsIn(TimeDelta::hours(1))
        );
        assert_eq!(
            Countdown::at(start, end, start),
            Countdown::EndsIn(TimeDelta::hours(5))
        );
        assert_eq!(Countdown::at(start, end, end), Countdown::Ended);
    }

    #[test]
    fn spans_read_to_the_minute() {
        assert_eq!(span_minutes(TimeDelta::minutes(3)), "3m");
        assert_eq!(span_minutes(TimeDelta::minutes(123)), "2h 3m");
        assert_eq!(span_minutes(TimeDelta::minutes(24 * 60 + 61)), "1d 1h 1m");
    }
}
