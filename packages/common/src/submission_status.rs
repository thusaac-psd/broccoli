use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

#[cfg(feature = "sea-orm")]
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[cfg_attr(
    feature = "sea-orm",
    derive(DeriveValueType),
    sea_orm(value_type = "String")
)]
#[serde(rename_all = "PascalCase")]
pub enum SubmissionStatus {
    /// Durable accept state: submission has been persisted by the API but
    /// not yet claimed by a server's claim fiber for dispatch. Set by
    /// UP#37 (`INSERT ... status='Queued'` on POST). The claim fiber
    /// transitions to `Pending` once it acquires the row. Distinguishing
    /// `Queued` from `Pending` lets per-state stuck-detector thresholds
    /// (UP#43) apply different timeouts and avoids false-positive 5-min
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

impl SubmissionStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Judged | Self::CompilationError | Self::SystemError
        )
    }

    pub fn is_judged(&self) -> bool {
        matches!(self, Self::Judged)
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Self::CompilationError | Self::SystemError)
    }

    pub const ALL: &'static [SubmissionStatus] = &[
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

impl fmt::Display for SubmissionStatus {
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
            SubmissionStatus::ALL
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for ParseStatusError {}

impl FromStr for SubmissionStatus {
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

#[derive(Clone, Debug, PartialEq, Eq, Hash, utoipa::ToSchema, Default)]
#[cfg_attr(
    feature = "sea-orm",
    derive(DeriveValueType),
    sea_orm(value_type = "String")
)]
pub enum Verdict {
    Accepted,
    WrongAnswer,
    TimeLimitExceeded,
    MemoryLimitExceeded,
    RuntimeError,
    #[default]
    SystemError,
    Skipped,
    Cancelled,
    Other(String),
}

impl Verdict {
    pub fn is_accepted(&self) -> bool {
        matches!(self, Self::Accepted)
    }

    pub fn is_skipped(&self) -> bool {
        matches!(self, Self::Skipped)
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }

    pub fn is_skipped_or_cancelled(&self) -> bool {
        matches!(self, Self::Skipped | Self::Cancelled)
    }

    pub const ALL: &'static [Verdict] = &[
        Self::Accepted,
        Self::WrongAnswer,
        Self::TimeLimitExceeded,
        Self::MemoryLimitExceeded,
        Self::RuntimeError,
        Self::SystemError,
        Self::Skipped,
        Self::Cancelled,
    ];

    pub fn as_str(&self) -> &str {
        match self {
            Self::Accepted => "Accepted",
            Self::WrongAnswer => "WrongAnswer",
            Self::TimeLimitExceeded => "TimeLimitExceeded",
            Self::MemoryLimitExceeded => "MemoryLimitExceeded",
            Self::RuntimeError => "RuntimeError",
            Self::SystemError => "SystemError",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
            Self::Other(custom) => custom.as_str(),
        }
    }

    /// Delegates to the canonical severity table in `broccoli_types` so the
    /// host and the WASM guest SDK can never disagree on aggregation order
    /// again (they did: this copy ranked `Other` at 255 while the SDK copy
    /// ranked it 5, putting a plugin custom verdict above everything on one
    /// side and below `CompileError` — tied with `SystemError` — on the
    /// other). See `broccoli_types::types::Verdict::severity` for the
    /// ordering contract. This enum has no `CompileError` variant because
    /// compile failures are represented as
    /// `SubmissionStatus::CompilationError` with a NULL verdict column.
    pub fn severity(&self) -> u8 {
        use broccoli_types::types::Verdict as Sdk;
        match self {
            Self::Accepted => Sdk::Accepted.severity(),
            Self::WrongAnswer => Sdk::WrongAnswer.severity(),
            Self::TimeLimitExceeded => Sdk::TimeLimitExceeded.severity(),
            Self::MemoryLimitExceeded => Sdk::MemoryLimitExceeded.severity(),
            Self::RuntimeError => Sdk::RuntimeError.severity(),
            Self::SystemError => Sdk::SystemError.severity(),
            Self::Skipped => Sdk::Skipped.severity(),
            Self::Cancelled => Sdk::Cancelled.severity(),
            // String::new() does not allocate; this is a zero-cost probe of
            // the canonical table.
            Self::Other(_) => Sdk::Other(String::new()).severity(),
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseVerdictError {
    invalid: String,
}

impl fmt::Display for ParseVerdictError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Invalid verdict '{}'. Valid values: {} or Other(<custom>)",
            self.invalid,
            Verdict::ALL
                .iter()
                .map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for ParseVerdictError {}

impl FromStr for Verdict {
    type Err = ParseVerdictError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(ParseVerdictError {
                invalid: s.to_string(),
            });
        }

        match s {
            "Accepted" => Ok(Self::Accepted),
            "WrongAnswer" => Ok(Self::WrongAnswer),
            "TimeLimitExceeded" => Ok(Self::TimeLimitExceeded),
            "MemoryLimitExceeded" => Ok(Self::MemoryLimitExceeded),
            "RuntimeError" => Ok(Self::RuntimeError),
            "SystemError" => Ok(Self::SystemError),
            "Skipped" => Ok(Self::Skipped),
            "Cancelled" => Ok(Self::Cancelled),
            _ => {
                if let Some(custom) = s
                    .strip_prefix("Other(")
                    .and_then(|v| v.strip_suffix(')'))
                    .filter(|v| !v.trim().is_empty())
                {
                    return Ok(Self::Other(custom.to_string()));
                }

                Ok(Self::Other(s.to_string()))
            }
        }
    }
}

impl Serialize for Verdict {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Verdict {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Ok(Self::from_str(raw.as_str()).unwrap_or(Self::Other(raw)))
    }
}

impl From<broccoli_types::types::Verdict> for Verdict {
    fn from(v: broccoli_types::types::Verdict) -> Self {
        use broccoli_types::types::Verdict as Sdk;
        match v {
            Sdk::Accepted => Self::Accepted,
            Sdk::WrongAnswer => Self::WrongAnswer,
            Sdk::TimeLimitExceeded => Self::TimeLimitExceeded,
            Sdk::MemoryLimitExceeded => Self::MemoryLimitExceeded,
            Sdk::RuntimeError => Self::RuntimeError,
            Sdk::SystemError | Sdk::CompileError => Self::SystemError,
            Sdk::Skipped => Self::Skipped,
            Sdk::Cancelled => Self::Cancelled,
            Sdk::Other(custom) => Self::Other(custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SubmissionStatus, Verdict};
    use std::str::FromStr;

    #[test]
    fn queued_status_round_trip_and_predicates() {
        let raw = serde_json::to_string(&SubmissionStatus::Queued).expect("serialize status");
        assert_eq!(raw, "\"Queued\"");
        let parsed: SubmissionStatus = serde_json::from_str(&raw).expect("deserialize status");
        assert_eq!(parsed, SubmissionStatus::Queued);
        assert_eq!(SubmissionStatus::Queued.as_str(), "Queued");
        assert_eq!(
            SubmissionStatus::from_str("Queued").expect("parse Queued"),
            SubmissionStatus::Queued
        );
        // Queued is pre-Pending: not terminal, not judged, not error.
        assert!(!SubmissionStatus::Queued.is_terminal());
        assert!(!SubmissionStatus::Queued.is_judged());
        assert!(!SubmissionStatus::Queued.is_error());
        // Queued must be in the ALL listing so callers iterating over all
        // statuses (registries, metrics labels) see it.
        assert!(SubmissionStatus::ALL.contains(&SubmissionStatus::Queued));
    }

    #[test]
    fn parse_tagged_other_verdict() {
        let verdict = Verdict::from_str("Other(PluginCustomStatus)").expect("parse verdict");
        assert_eq!(verdict, Verdict::Other("PluginCustomStatus".to_string()));
    }

    #[test]
    fn parse_plain_custom_verdict() {
        let verdict = Verdict::from_str("PluginCustomStatus").expect("parse custom verdict");
        assert_eq!(verdict, Verdict::Other("PluginCustomStatus".to_string()));
    }

    #[test]
    fn reject_empty_verdict() {
        let err = Verdict::from_str("   ").expect_err("should reject empty verdict");
        assert!(err.to_string().contains("Invalid verdict"));
    }

    #[test]
    fn serialize_other_verdict_as_plain_string() {
        let raw = serde_json::to_string(&Verdict::Other("CustomSignal".to_string()))
            .expect("serialize verdict");
        assert_eq!(raw, "\"CustomSignal\"");
    }

    #[test]
    fn deserialize_unknown_verdict_from_plain_string() {
        let verdict: Verdict =
            serde_json::from_str("\"PluginStatus\"").expect("deserialize verdict");
        assert_eq!(verdict, Verdict::Other("PluginStatus".to_string()));
    }

    #[test]
    fn lifecycle_statuses_match_the_sdk_for_every_host_state() {
        for status in SubmissionStatus::ALL {
            let wire = serde_json::to_value(status).unwrap();
            let sdk: broccoli_types::types::SubmissionLifecycle =
                serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(sdk).unwrap(), wire);
        }
    }

    #[test]
    fn severity_matches_canonical_sdk_table_for_all_variants() {
        // Drift pin: every host variant must rank exactly as its SDK
        // counterpart. If this fails, someone forked the severity table
        // again — fix it in broccoli_types, not here.
        use broccoli_types::types::Verdict as Sdk;
        let pairs: &[(Verdict, Sdk)] = &[
            (Verdict::Accepted, Sdk::Accepted),
            (Verdict::WrongAnswer, Sdk::WrongAnswer),
            (Verdict::TimeLimitExceeded, Sdk::TimeLimitExceeded),
            (Verdict::MemoryLimitExceeded, Sdk::MemoryLimitExceeded),
            (Verdict::RuntimeError, Sdk::RuntimeError),
            (Verdict::SystemError, Sdk::SystemError),
            (Verdict::Skipped, Sdk::Skipped),
            (Verdict::Cancelled, Sdk::Cancelled),
            (
                Verdict::Other("PluginStatus".into()),
                Sdk::Other("PluginStatus".into()),
            ),
        ];
        for (host, sdk) in pairs {
            assert_eq!(
                host.severity(),
                sdk.severity(),
                "severity drift for {host:?}"
            );
        }
        // Custom verdicts outrank standard failures on the host too.
        assert!(Verdict::Other("X".into()).severity() > Verdict::SystemError.severity());
    }

    #[test]
    fn from_sdk_verdict_preserves_db_string_representation() {
        // The SDK collapses CompileError into "SystemError" for the DB
        // (compile failures live in SubmissionStatus, not the verdict
        // column); the From impl must agree with that collapse, and every
        // other variant must round-trip its string form unchanged.
        use broccoli_types::types::Verdict as Sdk;
        let cases: &[Sdk] = &[
            Sdk::Accepted,
            Sdk::WrongAnswer,
            Sdk::TimeLimitExceeded,
            Sdk::MemoryLimitExceeded,
            Sdk::RuntimeError,
            Sdk::SystemError,
            Sdk::CompileError,
            Sdk::Skipped,
            Sdk::Cancelled,
            Sdk::Other("PluginStatus".into()),
        ];
        for sdk in cases {
            let host: Verdict = sdk.clone().into();
            assert_eq!(host.as_str(), sdk.to_db_str(), "string drift for {sdk:?}");
        }
    }

    #[test]
    fn cancelled_verdict_round_trip_and_predicates() {
        let raw = serde_json::to_string(&Verdict::Cancelled).expect("serialize verdict");
        assert_eq!(raw, "\"Cancelled\"");
        let parsed: Verdict = serde_json::from_str(&raw).expect("deserialize verdict");
        assert_eq!(parsed, Verdict::Cancelled);
        assert_eq!(parsed.as_str(), "Cancelled");
        assert_eq!(parsed.severity(), 0);
        assert!(parsed.is_cancelled());
        assert!(parsed.is_skipped_or_cancelled());
        assert!(!Verdict::Skipped.is_cancelled());
        assert!(Verdict::Skipped.is_skipped_or_cancelled());
        assert!(!Verdict::Accepted.is_skipped_or_cancelled());
    }
}
