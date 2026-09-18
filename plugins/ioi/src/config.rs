use broccoli_server_sdk::types::TestCaseRow;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringMode {
    #[default]
    MaxSubmission,
    SumBestSubtask,
    BestTokenedOrLast,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackLevel {
    #[default]
    Full,
    SubtaskScores,
    TotalOnly,
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreboardVisibility {
    #[default]
    AdminsOnly,
    AllContestViewers,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreboardTiebreaker {
    EqualRank,
    SumScoreTime,
    #[default]
    MaxScoreTime,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenMode {
    #[default]
    None,
    FixedBudget,
    Regenerating,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskScoringMethod {
    #[default]
    GroupMin,
    Sum,
    GroupMul,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenConfig {
    #[serde(default)]
    pub mode: TokenMode,
    #[serde(default)]
    pub initial: u32,
    #[serde(default)]
    pub max: u32,
    #[serde(default)]
    pub regen_interval_min: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContestConfig {
    #[serde(default)]
    pub scoring_mode: ScoringMode,
    #[serde(default)]
    pub feedback_level: FeedbackLevel,
    #[serde(default)]
    pub scoreboard_visibility: ScoreboardVisibility,
    #[serde(default)]
    pub scoreboard_tiebreaker: ScoreboardTiebreaker,
    #[serde(default)]
    pub tokens: TokenConfig,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubtaskDef {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub scoring_method: SubtaskScoringMethod,
    #[serde(default)]
    pub max_score: f64,
    /// Test case labels identifying which test cases belong to this subtask.
    #[serde(default)]
    pub test_cases: Vec<String>,
}

/// Resolve a test case's display label. Returns the explicit label if set,
/// otherwise falls back to the string representation of the ID.
pub fn resolve_tc_label(tc: &TestCaseRow) -> String {
    tc.label
        .as_deref()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .unwrap_or_else(|| tc.id.to_string())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskConfig {
    #[serde(default)]
    pub subtasks: Vec<SubtaskDef>,
}

/// Round a score to 2 decimal places (centipunto precision).
pub fn round_score(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_scoring_mode() {
        let config: ContestConfig =
            serde_json::from_str(r#"{"scoring_mode": "sum_best_subtask"}"#).unwrap();
        assert_eq!(config.scoring_mode, ScoringMode::SumBestSubtask);
    }

    #[test]
    fn deserialize_scoreboard_visibility() {
        let config: ContestConfig =
            serde_json::from_str(r#"{"scoreboard_visibility": "all_contest_viewers"}"#).unwrap();
        assert_eq!(
            config.scoreboard_visibility,
            ScoreboardVisibility::AllContestViewers
        );
    }

    #[test]
    fn deserialize_scoreboard_tiebreaker() {
        let config: ContestConfig =
            serde_json::from_str(r#"{"scoreboard_tiebreaker": "sum_score_time"}"#).unwrap();
        assert_eq!(
            config.scoreboard_tiebreaker,
            ScoreboardTiebreaker::SumScoreTime
        );
    }

    #[test]
    fn deserialize_subtask_with_string_ids() {
        let json = r#"{"test_cases": ["sample_01", "test_02"]}"#;
        let def: SubtaskDef = serde_json::from_str(json).unwrap();
        assert_eq!(def.test_cases, vec!["sample_01", "test_02"]);
    }
}
