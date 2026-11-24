//! Terminal presentation of the API's enums. The enums themselves come from
//! [`broccoli_api_types`]; this module only adds human labels and colours.

use crate::tui::theme::THEME;
use broccoli_api_types::submission::{SubmissionListItem, SubmissionResponse};
use broccoli_api_types::visibility::{ClosedBy, PluginOutage, Provided, Withheld};
pub use broccoli_api_types::{SubmissionStatus, Verdict};
use ratatui::style::Color;
use std::cell::OnceCell;
use std::collections::HashMap;
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
/// `status`, `withheld` and `parked_by`, which together say whether
/// anything is still to come. Every command that shows or waits on a
/// submission goes through it, so they agree on what is final.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// The server reports the status.
    Status(SubmissionStatus),
    /// The contest's plugin cannot judge or decide right now: the outcome is
    /// still to come, once the plugin is back.
    WaitingForPlugin(PluginOutage),
    /// Queued while a plugin it needs is disabled (`parked_by`): judging
    /// starts once an administrator enables it.
    Parked(ClosedBy),
    /// The contest's plugin hides the outcome from this viewer (a freeze,
    /// results released later). Final for now: polling changes nothing.
    Hidden,
}

impl Standing {
    pub fn new(
        status: Option<SubmissionStatus>,
        withheld: Option<&Withheld>,
        parked_by: Option<&ClosedBy>,
    ) -> Self {
        match (status, withheld, parked_by) {
            (_, Some(Withheld::Hidden), _) => Self::Hidden,
            (_, Some(Withheld::PluginOutage { outage }), _) => {
                Self::WaitingForPlugin(outage.clone())
            }
            (Some(status), None, Some(closed)) if !status.is_terminal() => {
                Self::Parked(closed.clone())
            }
            (Some(status), None, _) => Self::Status(status),
            // A status left out without a reason is left out by the plugin.
            (None, None, _) => Self::Hidden,
        }
    }

    pub fn of_submission(sub: &SubmissionResponse) -> Self {
        Self::new(sub.status, sub.withheld.as_ref(), sub.parked_by.as_ref())
    }

    pub fn of_item(item: &SubmissionListItem) -> Self {
        Self::new(item.status, item.withheld.as_ref(), item.parked_by.as_ref())
    }

    /// What the submission waits for, as a sentence for a wait line; `None`
    /// once nothing more is coming. A plugin it waits for is named through
    /// `names`, so whoever can enable it knows which one.
    pub fn waiting_for(&self, names: &PluginNames<'_>) -> Option<String> {
        match self {
            Self::Status(SubmissionStatus::Queued) => {
                Some("Queued: waiting for a judge to take it".into())
            }
            Self::Status(status) if status.is_in_progress() => {
                Some(format!("Judging: {}", status.human()))
            }
            Self::Status(_) | Self::Hidden => None,
            Self::WaitingForPlugin(PluginOutage::Disabled { plugin_id }) => Some(format!(
                "Waiting for the contest's plugin {}: disabled until an administrator \
                 enables it",
                names.name(plugin_id)
            )),
            Self::WaitingForPlugin(PluginOutage::Unavailable) => {
                Some("Waiting for the contest's plugin, which cannot answer right now".into())
            }
            Self::Parked(ClosedBy {
                plugin_id,
                provides,
            }) => {
                let role = match provides {
                    Provided::ContestType => "its contest plugin",
                    Provided::ProblemType => "its evaluator plugin",
                    Provided::CheckerFormat => "its checker plugin",
                };
                Some(format!(
                    "Waiting for {role} {}: disabled until an administrator enables it",
                    names.name(plugin_id)
                ))
            }
        }
    }
}

/// Plugins by the names the server's catalog gives them
/// (`plugin.<id>.name`), or by their ids where it gives none. The catalog is
/// read the first time a plugin is named, so a line that names none costs
/// no request.
pub struct PluginNames<'a> {
    catalog: OnceCell<HashMap<String, String>>,
    read: Box<dyn Fn() -> HashMap<String, String> + 'a>,
}

impl<'a> PluginNames<'a> {
    /// Names from the catalog `read` returns, read once, when first needed.
    pub fn new(read: impl Fn() -> HashMap<String, String> + 'a) -> Self {
        Self {
            catalog: OnceCell::new(),
            read: Box::new(read),
        }
    }

    /// No catalog: every plugin by its id.
    pub fn ids() -> Self {
        Self::new(HashMap::new)
    }

    /// What `plugin_id` is called: its catalog name, else its id.
    pub fn name(&self, plugin_id: &str) -> String {
        self.catalog
            .get_or_init(|| (self.read)())
            .get(&format!("plugin.{plugin_id}.name"))
            .cloned()
            .unwrap_or_else(|| plugin_id.to_string())
    }
}

impl StatusDisplay for Standing {
    fn human(&self) -> &str {
        match self {
            Self::Status(status) => status.human(),
            Self::WaitingForPlugin(_) | Self::Parked(_) => "Waiting for plugin",
            Self::Hidden => "Hidden",
        }
    }

    fn is_in_progress(&self) -> bool {
        match self {
            Self::Status(status) => status.is_in_progress(),
            Self::WaitingForPlugin(_) | Self::Parked(_) => true,
            Self::Hidden => false,
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Status(status) => status.color(),
            Self::WaitingForPlugin(_) | Self::Parked(_) => THEME.warning,
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
        assert_eq!(hidden.waiting_for(&PluginNames::ids()), None);

        let outage = Withheld::PluginOutage {
            outage: PluginOutage::Unavailable,
        };
        let waiting = Standing::new(None, Some(&outage), None);
        assert_eq!(
            waiting,
            Standing::WaitingForPlugin(PluginOutage::Unavailable)
        );
        assert!(waiting.is_in_progress());
        assert!(waiting.waiting_for(&PluginNames::ids()).is_some());
    }

    #[test]
    fn a_parked_submission_names_its_plugin_by_the_catalog_and_its_role() {
        let names = PluginNames::new(|| {
            [("plugin.ioi.name".to_string(), "IOI".to_string())]
                .into_iter()
                .collect()
        });
        let parked = |plugin_id: &str, provides| {
            Standing::new(
                Some(SubmissionStatus::Queued),
                None,
                Some(&ClosedBy {
                    plugin_id: plugin_id.into(),
                    provides,
                }),
            )
        };
        assert_eq!(
            parked("ioi", Provided::ContestType)
                .waiting_for(&names)
                .as_deref(),
            Some("Waiting for its contest plugin IOI: disabled until an administrator enables it")
        );
        // A plugin the catalog does not name is named by its id.
        assert_eq!(
            parked("batch", Provided::ProblemType)
                .waiting_for(&names)
                .as_deref(),
            Some(
                "Waiting for its evaluator plugin batch: disabled until an administrator \
                 enables it"
            )
        );
        assert!(
            parked("standard-checkers", Provided::CheckerFormat)
                .waiting_for(&names)
                .unwrap()
                .starts_with("Waiting for its checker plugin standard-checkers:")
        );
        // A finished submission is finished whatever its plugin does now.
        let judged = Standing::new(
            Some(SubmissionStatus::Judged),
            None,
            Some(&ClosedBy {
                plugin_id: "ioi".into(),
                provides: Provided::ContestType,
            }),
        );
        assert_eq!(judged, Standing::Status(SubmissionStatus::Judged));
        assert!(!judged.is_in_progress());
    }

    #[test]
    fn a_line_that_names_no_plugin_reads_no_catalog() {
        let names = PluginNames::new(|| panic!("the catalog is read only to name a plugin"));
        assert!(
            Standing::Status(SubmissionStatus::Queued)
                .waiting_for(&names)
                .is_some()
        );
        assert_eq!(Standing::Hidden.waiting_for(&names), None);
    }

    #[test]
    fn a_queued_submission_says_it_waits_for_a_judge() {
        let queued = Standing::Status(SubmissionStatus::Queued);
        assert!(
            queued
                .waiting_for(&PluginNames::ids())
                .unwrap()
                .contains("waiting for a judge")
        );
        let running = Standing::Status(SubmissionStatus::Running);
        assert_eq!(
            running.waiting_for(&PluginNames::ids()).as_deref(),
            Some("Judging: Running")
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
