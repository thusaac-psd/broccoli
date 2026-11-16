//! Terminal presentation of the API's enums. The enums themselves come from
//! [`broccoli_api_types`]; this module only adds human labels and colours.

use crate::tui::theme::THEME;
use broccoli_api_types::submission::{SubmissionListItem, SubmissionResponse};
use broccoli_api_types::visibility::{PluginOutage, Withheld};
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

/// Where a submission stands for its viewer: one reading of the response's
/// `status`, `withheld` and `plugin_outage`, which together say whether
/// anything is still to come. Every command that shows or waits on a
/// submission goes through it, so they agree on what is final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The server reports the status.
    Status(SubmissionStatus),
    /// The contest's plugin cannot judge or decide right now: the outcome is
    /// still to come, once the plugin is back.
    WaitingForPlugin(PluginOutage),
    /// The contest's plugin hides the outcome from this viewer (a freeze,
    /// results released later). Final for now: polling changes nothing.
    Hidden,
}

impl Standing {
    pub fn new(
        status: Option<SubmissionStatus>,
        withheld: Option<&Withheld>,
        plugin_outage: Option<&PluginOutage>,
    ) -> Self {
        match (status, withheld, plugin_outage) {
            (_, Some(Withheld::Hidden), _) => Self::Hidden,
            (_, Some(Withheld::PluginOutage { outage }), _) => {
                Self::WaitingForPlugin(outage.clone())
            }
            (Some(status), None, Some(outage)) if !status.is_terminal() => {
                Self::WaitingForPlugin(outage.clone())
            }
            (Some(status), None, _) => Self::Status(status),
            // A status left out without a reason is left out by the plugin.
            (None, None, _) => Self::Hidden,
        }
    }

    pub fn of_submission(sub: &SubmissionResponse) -> Self {
        Self::new(
            sub.status,
            sub.withheld.as_ref(),
            sub.plugin_outage.as_ref(),
        )
    }

    pub fn of_item(item: &SubmissionListItem) -> Self {
        Self::new(
            item.status,
            item.withheld.as_ref(),
            item.plugin_outage.as_ref(),
        )
    }

    /// What the submission waits for, as a sentence for a wait line; `None`
    /// once nothing more is coming.
    pub fn waiting_for(&self) -> Option<String> {
        match self {
            Self::Status(SubmissionStatus::Queued) => {
                Some("Queued: waiting for a judge to take it".into())
            }
            Self::Status(status) if status.is_in_progress() => {
                Some(format!("Judging: {}", status.human()))
            }
            Self::Status(_) | Self::Hidden => None,
            Self::WaitingForPlugin(PluginOutage::Disabled { plugin_id }) => Some(format!(
                "Waiting for the contest's plugin: '{plugin_id}' is disabled until an \
                 administrator enables it"
            )),
            Self::WaitingForPlugin(PluginOutage::Unavailable) => {
                Some("Waiting for the contest's plugin, which cannot answer right now".into())
            }
        }
    }
}

impl StatusDisplay for Standing {
    fn human(&self) -> &str {
        match self {
            Self::Status(status) => status.human(),
            Self::WaitingForPlugin(_) => "Waiting for plugin",
            Self::Hidden => "Hidden",
        }
    }

    fn is_in_progress(&self) -> bool {
        match self {
            Self::Status(status) => status.is_in_progress(),
            Self::WaitingForPlugin(_) => true,
            Self::Hidden => false,
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Status(status) => status.color(),
            Self::WaitingForPlugin(_) => THEME.warning,
            Self::Hidden => THEME.muted,
        }
    }
}

/// A contest problem row's title, or what stands in for it while the
/// contest's plugin withholds it.
pub fn problem_title(p: &broccoli_api_types::contest::ContestProblemResponse) -> &str {
    p.problem_title.as_deref().unwrap_or("(unavailable)")
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
    fn only_a_hidden_outcome_is_final_among_withheld_ones() {
        let hidden = Standing::new(None, Some(&Withheld::Hidden), None);
        assert_eq!(hidden, Standing::Hidden);
        assert!(!hidden.is_in_progress());
        assert_eq!(hidden.waiting_for(), None);

        let outage = Withheld::PluginOutage {
            outage: PluginOutage::Unavailable,
        };
        let waiting = Standing::new(None, Some(&outage), None);
        assert_eq!(
            waiting,
            Standing::WaitingForPlugin(PluginOutage::Unavailable)
        );
        assert!(waiting.is_in_progress());
        assert!(waiting.waiting_for().is_some());
    }

    #[test]
    fn a_queued_submission_whose_plugin_is_disabled_waits_for_the_plugin() {
        let disabled = PluginOutage::Disabled {
            plugin_id: "ioi".into(),
        };
        let standing = Standing::new(Some(SubmissionStatus::Queued), None, Some(&disabled));
        assert_eq!(standing, Standing::WaitingForPlugin(disabled.clone()));
        assert!(
            standing
                .waiting_for()
                .unwrap()
                .contains("'ioi' is disabled")
        );
        // A finished submission is finished whatever its plugin does now.
        let judged = Standing::new(Some(SubmissionStatus::Judged), None, Some(&disabled));
        assert_eq!(judged, Standing::Status(SubmissionStatus::Judged));
        assert!(!judged.is_in_progress());
    }

    #[test]
    fn a_queued_submission_says_it_waits_for_a_judge() {
        let queued = Standing::Status(SubmissionStatus::Queued);
        assert!(
            queued
                .waiting_for()
                .unwrap()
                .contains("waiting for a judge")
        );
        let running = Standing::Status(SubmissionStatus::Running);
        assert_eq!(running.waiting_for().as_deref(), Some("Judging: Running"));
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
