use broccoli_server_sdk::types::TestCaseRow;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoringMode {
    MaxSubmission,
    SumBestSubtask,
    BestTokenedOrLast,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackLevel {
    Full,
    SubtaskScores,
    TotalOnly,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreboardVisibility {
    AdminsOnly,
    AllContestViewers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreboardTiebreaker {
    EqualRank,
    SumScoreTime,
    MaxScoreTime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenMode {
    None,
    FixedBudget,
    Regenerating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskScoringMethod {
    GroupMin,
    Sum,
    GroupMul,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenConfig {
    pub mode: TokenMode,
    pub initial: u32,
    pub max: u32,
    pub regen_interval_min: u32,
}

/// The `contest` config namespace. Its defaults and bounds live in the
/// namespace's schema in `plugin.toml`; the host fills them in, so every
/// field is present here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContestConfig {
    pub scoring_mode: ScoringMode,
    pub feedback_level: FeedbackLevel,
    pub scoreboard_visibility: ScoreboardVisibility,
    pub scoreboard_tiebreaker: ScoreboardTiebreaker,
    pub tokens: TokenConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtaskDef {
    pub name: String,
    pub scoring_method: SubtaskScoringMethod,
    pub max_score: f64,
    /// Test case labels identifying which test cases belong to this subtask.
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

/// The `task` config namespace, schema-filled like [`ContestConfig`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskConfig {
    pub subtasks: Vec<SubtaskDef>,
}

/// Round a score to 2 decimal places (centipunto precision).
pub fn round_score(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
impl ContestConfig {
    /// The config of a contest that stores none: the schema's defaults.
    pub(crate) fn schema_defaults() -> Self {
        broccoli_server_sdk::Host::mock()
            .config
            .get_contest(0, "contest")
            .and_then(|c| c.parse())
            .expect("the schema defaults are a complete contest config")
    }
}

#[cfg(test)]
impl TaskConfig {
    /// The config of a task that stores none: the schema's defaults.
    pub(crate) fn schema_defaults() -> Self {
        broccoli_server_sdk::Host::mock()
            .config
            .get_contest_problem(0, 0, "task")
            .and_then(|c| c.parse())
            .expect("the schema defaults are a complete task config")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use broccoli_server_sdk::Host;
    use serde_json::json;

    #[test]
    fn a_partial_contest_config_reads_with_the_schema_defaults() {
        let host = Host::mock();
        host.config.seed(
            "contest",
            "3",
            "contest",
            json!({"scoring_mode": "sum_best_subtask", "tokens": {"mode": "fixed_budget"}}),
        );
        let config: ContestConfig = host
            .config
            .get_contest(3, "contest")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(config.scoring_mode, ScoringMode::SumBestSubtask);
        assert_eq!(config.feedback_level, FeedbackLevel::Full);
        assert_eq!(
            config.scoreboard_visibility,
            ScoreboardVisibility::AdminsOnly
        );
        assert_eq!(
            config.scoreboard_tiebreaker,
            ScoreboardTiebreaker::MaxScoreTime
        );
        assert_eq!(config.tokens.mode, TokenMode::FixedBudget);
        assert_eq!(
            (
                config.tokens.initial,
                config.tokens.max,
                config.tokens.regen_interval_min
            ),
            (2, 5, 30)
        );
    }

    #[test]
    fn an_unknown_scoring_mode_is_an_error() {
        let host = Host::mock();
        host.config
            .seed("contest", "3", "contest", json!({"scoring_mode": "bogus"}));
        let result = host
            .config
            .get_contest(3, "contest")
            .unwrap()
            .parse::<ContestConfig>();
        assert!(result.is_err(), "{result:?}");
    }

    #[test]
    fn a_subtask_reads_with_the_schema_defaults() {
        let host = Host::mock();
        host.config.seed(
            "contest_problem",
            "3:7",
            "task",
            json!({"subtasks": [{"test_cases": ["sample_01", "test_02"]}]}),
        );
        let task: TaskConfig = host
            .config
            .get_contest_problem(3, 7, "task")
            .unwrap()
            .parse()
            .unwrap();
        let def = &task.subtasks[0];
        assert_eq!(def.test_cases, ["sample_01", "test_02"]);
        assert_eq!(def.name, "Subtask");
        assert_eq!(def.scoring_method, SubtaskScoringMethod::GroupMin);
        assert_eq!(def.max_score, 100.0);
        let empty: TaskConfig = host
            .config
            .get_contest_problem(3, 8, "task")
            .unwrap()
            .parse()
            .unwrap();
        assert!(empty.subtasks.is_empty());
    }
}
