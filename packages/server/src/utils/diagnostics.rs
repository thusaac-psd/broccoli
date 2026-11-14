//! Who sees a system error's diagnostics.
//!
//! The error message of a judgement or code run that ended as a system error,
//! and the output and checker message of a test case judged `SystemError`,
//! describe the platform (task ids, worker paths, settings), not the
//! submission. They are for operators (`system:view`) and the logs. Anyone
//! else sees the verdict and the judgement id, which the web app shows with a
//! generic explanation to quote to staff.

use broccoli_server_sdk::permissions as perm;
use common::{SubmissionStatus, Verdict};

use crate::extractors::auth::AuthUser;

/// Whether a response carries system-error diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Diagnostics {
    Shown,
    Hidden,
}

impl Diagnostics {
    pub fn shown_if(shown: bool) -> Self {
        if shown { Self::Shown } else { Self::Hidden }
    }

    /// What `viewer` sees.
    pub fn for_viewer(viewer: &AuthUser) -> Self {
        Self::shown_if(viewer.has_permission(perm::SYSTEM_VIEW))
    }

    /// `text`, unless it is a hidden diagnostic of an outcome that
    /// `is_system_error`.
    pub fn filter<T>(self, is_system_error: bool, text: Option<T>) -> Option<T> {
        match self {
            Self::Hidden if is_system_error => None,
            Self::Hidden | Self::Shown => text,
        }
    }

    /// SQL: the output columns of the result row `table` may be shown.
    pub fn sql_result_shown(self, table: &str) -> String {
        match self {
            Self::Shown => "TRUE".into(),
            Self::Hidden => format!(
                "{table}.verdict IS DISTINCT FROM '{}'",
                Verdict::SystemError
            ),
        }
    }
}

/// Whether a judged outcome is a system error, in either shape: a
/// `SystemError` status, or a plugin's `SystemError` verdict.
pub fn is_system_error(status: &SubmissionStatus, verdict: Option<&Verdict>) -> bool {
    *status == SubmissionStatus::SystemError || verdict == Some(&Verdict::SystemError)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_system_errors_text_is_hidden() {
        assert_eq!(Diagnostics::Hidden.filter(true, Some("worker path")), None);
        assert_eq!(
            Diagnostics::Hidden.filter(false, Some("wrong answer")),
            Some("wrong answer")
        );
        assert_eq!(
            Diagnostics::Shown.filter(true, Some("worker path")),
            Some("worker path")
        );
    }

    #[test]
    fn both_shapes_of_a_system_error_count() {
        assert!(is_system_error(&SubmissionStatus::SystemError, None));
        assert!(is_system_error(
            &SubmissionStatus::Judged,
            Some(&Verdict::SystemError)
        ));
        assert!(!is_system_error(
            &SubmissionStatus::Judged,
            Some(&Verdict::WrongAnswer)
        ));
    }
}
