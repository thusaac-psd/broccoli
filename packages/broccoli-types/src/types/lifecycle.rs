use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[cfg(feature = "sea-orm")]
use sea_orm::entity::prelude::*;

// Stored in the submission `status` column and reported by the REST API as
// `SubmissionStatus`. Judge writes use the narrower `SubmissionStatus` of this
// crate in `SubmissionUpdate`.
/// Where a submission is in its lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[cfg_attr(
    feature = "utoipa",
    derive(utoipa::ToSchema),
    schema(as = SubmissionStatus)
)]
#[cfg_attr(
    feature = "sea-orm",
    derive(DeriveValueType),
    sea_orm(value_type = "String")
)]
#[serde(rename_all = "PascalCase")]
pub enum SubmissionLifecycle {
    /// Durable accept state: submission has been persisted by the API but
    /// not yet claimed by a server's claim fiber for dispatch. Set by the
    /// POST handlers (`INSERT ... status='Queued'`). The claim fiber
    /// transitions to `Pending` once it acquires the row. Distinguishing
    /// `Queued` from `Pending` lets per-state stuck-detector thresholds
    /// apply different timeouts and avoids false-positive 5-min
    /// "no owner" alarms on rows that simply haven't been claimed yet.
    Queued,
    #[default]
    Pending,
    Compiling,
    Running,
    Judged,
    CompilationError,
    SystemError,
}

/// Where a status sits in the judging lifecycle. Every status belongs to
/// exactly one phase; [`SubmissionLifecycle::phase`] is the one definition the
/// in-flight and terminal sets (and their SQL forms) are derived from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StatusPhase {
    /// Accepted and waiting for a claim fiber.
    Queued,
    /// Claimed by a server and being dispatched or judged.
    InFlight,
    /// Finished; nothing writes the row again until it is re-opened.
    Terminal,
}

impl StatusPhase {
    /// The statuses in this phase as a SQL list, e.g. `('Pending', 'Running')`,
    /// for statements that are built as text.
    pub fn sql_list(self) -> &'static str {
        static LISTS: std::sync::LazyLock<[String; 3]> = std::sync::LazyLock::new(|| {
            [
                StatusPhase::Queued,
                StatusPhase::InFlight,
                StatusPhase::Terminal,
            ]
            .map(|phase| {
                let quoted: Vec<String> = phase
                    .statuses()
                    .map(|s| format!("'{}'", s.as_str()))
                    .collect();
                format!("({})", quoted.join(", "))
            })
        });
        &LISTS[self as usize]
    }

    /// The statuses in this phase.
    pub fn statuses(self) -> impl Iterator<Item = SubmissionLifecycle> + Clone {
        SubmissionLifecycle::ALL
            .iter()
            .filter(move |s| s.phase() == self)
            .copied()
    }
}

impl SubmissionLifecycle {
    pub fn phase(&self) -> StatusPhase {
        match self {
            Self::Queued => StatusPhase::Queued,
            Self::Pending | Self::Compiling | Self::Running => StatusPhase::InFlight,
            Self::Judged | Self::CompilationError | Self::SystemError => StatusPhase::Terminal,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.phase() == StatusPhase::Terminal
    }

    pub fn is_in_flight(&self) -> bool {
        self.phase() == StatusPhase::InFlight
    }

    pub fn is_judged(&self) -> bool {
        matches!(self, Self::Judged)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::CompilationError | Self::SystemError)
    }

    pub const ALL: &'static [SubmissionLifecycle] = &[
        Self::Queued,
        Self::Pending,
        Self::Compiling,
        Self::Running,
        Self::Judged,
        Self::CompilationError,
        Self::SystemError,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Pending => "Pending",
            Self::Compiling => "Compiling",
            Self::Running => "Running",
            Self::Judged => "Judged",
            Self::CompilationError => "CompilationError",
            Self::SystemError => "SystemError",
        }
    }
}

impl fmt::Display for SubmissionLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseStatusError {
    invalid: String,
}

impl fmt::Display for ParseStatusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid status '{}'. Valid values: {}",
            self.invalid,
            SubmissionLifecycle::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for ParseStatusError {}

impl FromStr for SubmissionLifecycle {
    type Err = ParseStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Queued" => Ok(Self::Queued),
            "Pending" => Ok(Self::Pending),
            "Compiling" => Ok(Self::Compiling),
            "Running" => Ok(Self::Running),
            "Judged" => Ok(Self::Judged),
            "CompilationError" => Ok(Self::CompilationError),
            "SystemError" => Ok(Self::SystemError),
            _ => Err(ParseStatusError {
                invalid: s.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{StatusPhase, SubmissionLifecycle};
    use std::str::FromStr;

    #[test]
    fn phase_sql_lists_name_each_phase_statuses() {
        assert_eq!(StatusPhase::Queued.sql_list(), "('Queued')");
        assert_eq!(
            StatusPhase::InFlight.sql_list(),
            "('Pending', 'Compiling', 'Running')"
        );
        assert_eq!(
            StatusPhase::Terminal.sql_list(),
            "('Judged', 'CompilationError', 'SystemError')"
        );
    }

    #[test]
    fn queued_round_trips_and_is_not_terminal() {
        let raw = serde_json::to_string(&SubmissionLifecycle::Queued).unwrap();
        assert_eq!(raw, "\"Queued\"");
        let parsed: SubmissionLifecycle = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed, SubmissionLifecycle::Queued);
        assert_eq!(SubmissionLifecycle::Queued.as_str(), "Queued");
        assert_eq!(
            SubmissionLifecycle::from_str("Queued").unwrap(),
            SubmissionLifecycle::Queued
        );
        assert!(!SubmissionLifecycle::Queued.is_terminal());
        assert!(!SubmissionLifecycle::Queued.is_judged());
        assert!(!SubmissionLifecycle::Queued.is_error());
        assert!(SubmissionLifecycle::ALL.contains(&SubmissionLifecycle::Queued));
    }

    #[test]
    fn every_state_serializes_as_its_name() {
        for status in SubmissionLifecycle::ALL {
            let wire = serde_json::to_value(status).unwrap();
            assert_eq!(wire, status.as_str());
            assert_eq!(
                SubmissionLifecycle::from_str(status.as_str()).unwrap(),
                *status
            );
        }
    }
}
