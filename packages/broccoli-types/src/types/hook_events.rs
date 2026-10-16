use serde::{Deserialize, Serialize};

pub trait HookEvent: Serialize + for<'de> Deserialize<'de> {
    const TOPIC: &'static str;
}

/// The host fixes these attributes before dispatch. Hooks may gate submission
/// and the contest owner may attach admission effects, but cannot rewrite the
/// request that will be inserted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeforeSubmissionEvent {
    pub user_id: i32,
    pub problem_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contest_id: Option<i32>,
    pub language: String,
    pub file_count: usize,
    pub contest_type: String,
}

impl HookEvent for BeforeSubmissionEvent {
    const TOPIC: &'static str = "before_submission";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AfterSubmissionEvent {
    pub submission_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contest_id: Option<i32>,
    pub language: String,
}

impl HookEvent for AfterSubmissionEvent {
    const TOPIC: &'static str = "after_submission";
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AfterJudgingEvent {
    pub submission_id: i32,
    pub user_id: i32,
    pub problem_id: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contest_id: Option<i32>,
    pub verdict: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
}

impl HookEvent for AfterJudgingEvent {
    const TOPIC: &'static str = "after_judging";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_submission_event_serializes_and_deserializes_with_all_fields() {
        let event = BeforeSubmissionEvent {
            user_id: 5,
            problem_id: 12,
            contest_id: Some(3),
            language: "cpp".into(),
            contest_type: "example".into(),
            file_count: 1,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["user_id"], 5);
        assert_eq!(json["contest_id"], 3);
        let back: BeforeSubmissionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back.user_id, 5);
        assert_eq!(back.contest_id, Some(3));
    }

    #[test]
    fn before_submission_event_omits_contest_id_when_none() {
        let event = BeforeSubmissionEvent {
            user_id: 1,
            problem_id: 2,
            contest_id: None,
            language: "py".into(),
            contest_type: "example".into(),
            file_count: 2,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert!(json.get("contest_id").is_none());
        let back: BeforeSubmissionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back.contest_id, None);
    }

    #[test]
    fn after_judging_event_serializes_and_deserializes_with_all_fields() {
        let event = AfterJudgingEvent {
            submission_id: 42,
            user_id: 5,
            problem_id: 12,
            contest_id: None,
            verdict: "Accepted".into(),
            score: Some(100.0),
        };
        let json = serde_json::to_value(&event).unwrap();
        let back: AfterJudgingEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back.verdict, "Accepted");
        assert_eq!(back.score, Some(100.0));
    }
}
