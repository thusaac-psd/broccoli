use broccoli_server_sdk::{Host, error::SdkError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContestConfig {
    /// Zero disables the setup notice. The actual problem list comes from the contest.
    pub expected_problem_count: usize,
    pub slots_per_problem: usize,
    /// Point threshold. Retain the stored/API key for existing contest configs.
    pub solves_to_qualify: usize,
    pub advancement_slots: usize,
    /// Zero disables automatic polling, while manual refresh remains available.
    pub scoreboard_refresh_seconds: u32,
}

impl Default for ContestConfig {
    fn default() -> Self {
        Self {
            expected_problem_count: 20,
            slots_per_problem: 2,
            solves_to_qualify: 3,
            advancement_slots: 16,
            scoreboard_refresh_seconds: 5,
        }
    }
}

impl ContestConfig {
    pub fn load(host: &Host, contest_id: i32) -> Result<Self, SdkError> {
        let value = host.config.get_contest(contest_id, "contest")?.config;
        // Missing fields use defaults. Invalid stored values must not silently
        // revert a live contest to different qualification rules.
        let config: Self = serde_json::from_value(value)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), SdkError> {
        for (name, value, min, max) in [
            (
                "expected_problem_count",
                self.expected_problem_count,
                0,
                1000,
            ),
            ("slots_per_problem", self.slots_per_problem, 1, 1000),
            ("solves_to_qualify", self.solves_to_qualify, 1, 1000),
            ("advancement_slots", self.advancement_slots, 1, 1000),
            (
                "scoreboard_refresh_seconds",
                self.scoreboard_refresh_seconds as usize,
                0,
                3600,
            ),
        ] {
            if !(min..=max).contains(&value) {
                return Err(SdkError::Other(format!(
                    "Invalid Codelink configuration: {name} must be between {min} and {max}"
                )));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProblemConfig {
    pub points: usize,
}

impl Default for ProblemConfig {
    fn default() -> Self {
        Self { points: 1 }
    }
}

impl ProblemConfig {
    pub fn load(host: &Host, contest_id: i32, problem_id: i32) -> Result<Self, SdkError> {
        let value = host
            .config
            .get_contest_problem(contest_id, problem_id, "problem")?
            .config;
        let config: Self = serde_json::from_value(value)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), SdkError> {
        if !(1..=2).contains(&self.points) {
            return Err(SdkError::Other(
                "Invalid Codelink configuration: problem points must be 1 or 2".into(),
            ));
        }
        Ok(())
    }
}
