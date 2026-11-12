//! Terminal presentation of the API's enums. The enums themselves come from
//! [`broccoli_api_types`]; this module only adds human labels and colours.

use crate::tui::theme::THEME;
pub use broccoli_api_types::{SubmissionStatus, Verdict};
use ratatui::style::Color;
use std::fmt;
use std::str::FromStr;

/// Human labels and colours for a [`Verdict`].
pub trait VerdictDisplay {
    fn human(&self) -> &str;
    fn color(&self) -> Color;
}

impl VerdictDisplay for Verdict {
    fn human(&self) -> &str {
        match self {
            Self::Accepted => "Accepted",
            Self::WrongAnswer => "Wrong Answer",
            Self::TimeLimitExceeded => "Time Limit Exceeded",
            Self::MemoryLimitExceeded => "Memory Limit Exceeded",
            Self::RuntimeError => "Runtime Error",
            Self::SystemError => "System Error",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
            Self::Other(s) => s,
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Accepted => THEME.success,
            Self::Skipped | Self::Cancelled => THEME.muted,
            _ => THEME.error,
        }
    }
}

/// Human labels, progress and colours for a [`SubmissionStatus`].
pub trait StatusDisplay {
    fn human(&self) -> &str;
    /// Whether judging is still ongoing (the status is not terminal).
    fn is_in_progress(&self) -> bool;
    /// Colour when no verdict is available.
    fn color(&self) -> Color;
}

impl StatusDisplay for SubmissionStatus {
    fn human(&self) -> &str {
        match self {
            Self::CompilationError => "Compilation Error",
            Self::SystemError => "System Error",
            other => other.as_str(),
        }
    }

    fn is_in_progress(&self) -> bool {
        !self.is_terminal()
    }

    fn color(&self) -> Color {
        if self.is_error() {
            THEME.error
        } else if self.is_in_progress() {
            THEME.warning
        } else {
            THEME.fg
        }
    }
}

/// The `clarification_type` string of a clarification.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ClarificationKind {
    Announcement,
    Question,
    DirectMessage,
    Other(String),
}

impl ClarificationKind {
    pub fn human(&self) -> &str {
        match self {
            Self::Announcement => "Announcement",
            Self::Question => "Question",
            Self::DirectMessage => "Direct message",
            Self::Other(s) => s,
        }
    }
}

impl FromStr for ClarificationKind {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "announcement" => Self::Announcement,
            "question" => Self::Question,
            "direct_message" => Self::DirectMessage,
            other => Self::Other(other.to_string()),
        })
    }
}

impl fmt::Display for ClarificationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.human())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_have_spaced_labels_and_custom_verdicts_render_verbatim() {
        assert_eq!(Verdict::WrongAnswer.human(), "Wrong Answer");
        assert_eq!(
            Verdict::Other("PresentationError".into()).human(),
            "PresentationError"
        );
    }

    #[test]
    fn only_terminal_statuses_stop_progress() {
        assert!(SubmissionStatus::Queued.is_in_progress());
        assert!(SubmissionStatus::Running.is_in_progress());
        assert!(!SubmissionStatus::Judged.is_in_progress());
        assert!(!SubmissionStatus::SystemError.is_in_progress());
        assert_eq!(
            SubmissionStatus::CompilationError.human(),
            "Compilation Error"
        );
    }

    #[test]
    fn clarification_kind_parses_wire_names() {
        assert_eq!(
            "direct_message"
                .parse::<ClarificationKind>()
                .unwrap()
                .human(),
            "Direct message"
        );
    }
}
