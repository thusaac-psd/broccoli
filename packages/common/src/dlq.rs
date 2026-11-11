use serde::{Deserialize, Serialize};

use crate::retry::RetryAttempt;

pub struct SubmissionDlqErrorCode;

impl SubmissionDlqErrorCode {
    pub const STUCK_JOB: &'static str = "STUCK_JOB";
    pub const DISPATCH_RETRY_EXHAUSTED: &'static str = "DISPATCH_RETRY_EXHAUSTED";
    /// A judgement still in flight when a newer judgement replaced it as
    /// current (rejudge, DLQ retry, lease steal, applying another judgement).
    pub const SUPERSEDED: &'static str = "SUPERSEDED";
    /// A contest-type plugin refused the submission deterministically
    /// (`OnSubmissionOutput::refused`). Terminal: never re-judged
    /// automatically.
    pub const PLUGIN_REFUSED: &'static str = "PLUGIN_REFUSED";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DlqErrorCode {
    MaxRetriesExceeded,
    DeserializationError,
    StuckJob,
    DispatchRetryExhausted,
}

impl DlqErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MaxRetriesExceeded => "MAX_RETRIES_EXCEEDED",
            Self::DeserializationError => "DESERIALIZATION_ERROR",
            Self::StuckJob => "STUCK_JOB",
            Self::DispatchRetryExhausted => "DISPATCH_RETRY_EXHAUSTED",
        }
    }
}

impl std::fmt::Display for DlqErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlqMessageType {
    OperationTask,
    StuckSubmission,
    StuckCodeRun,
    StuckSubmissionJudgement,
}

impl DlqMessageType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OperationTask => "operation_task",
            Self::StuckSubmission => "stuck_submission",
            Self::StuckCodeRun => "stuck_code_run",
            Self::StuckSubmissionJudgement => "stuck_submission_judgement",
        }
    }
}

impl std::fmt::Display for DlqMessageType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl std::str::FromStr for DlqMessageType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "operation_task" => Ok(Self::OperationTask),
            "stuck_submission" => Ok(Self::StuckSubmission),
            "stuck_code_run" => Ok(Self::StuckCodeRun),
            "stuck_submission_judgement" => Ok(Self::StuckSubmissionJudgement),
            _ => Err(format!(
                "Invalid message_type '{}'. Must be 'operation_task', 'stuck_submission', 'stuck_code_run', or 'stuck_submission_judgement'",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlqEnvelope {
    pub message_id: String,
    pub message_type: DlqMessageType,
    pub submission_id: Option<i32>,
    pub payload: serde_json::Value,
    pub error_code: DlqErrorCode,
    pub error_message: String,
    pub retry_history: Vec<RetryAttempt>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dlq_message_type_round_trips_all_public_values() {
        let cases = [
            (DlqMessageType::OperationTask, "operation_task"),
            (DlqMessageType::StuckSubmission, "stuck_submission"),
            (DlqMessageType::StuckCodeRun, "stuck_code_run"),
            (
                DlqMessageType::StuckSubmissionJudgement,
                "stuck_submission_judgement",
            ),
        ];

        for (message_type, value) in cases {
            assert_eq!(message_type.as_str(), value);
            assert_eq!(message_type.to_string(), value);
            assert_eq!(value.parse::<DlqMessageType>().unwrap(), message_type);
        }
    }

    #[test]
    fn dlq_message_type_validation_names_all_allowed_values() {
        let err = "invalid".parse::<DlqMessageType>().unwrap_err();

        assert!(err.contains("operation_task"));
        assert!(err.contains("stuck_submission"));
        assert!(err.contains("stuck_code_run"));
        assert!(err.contains("stuck_submission_judgement"));
    }
}
