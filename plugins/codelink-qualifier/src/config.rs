use broccoli_server_sdk::{Host, error::SdkError};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContestConfig {
    /// Zero disables the setup notice. The actual problem list comes from the contest.
    pub expected_problem_count: usize,
    pub slots_per_problem: usize,
    pub solves_to_qualify: usize,
    /// Zero disables automatic polling, while manual refresh remains available.
    pub scoreboard_refresh_seconds: u32,
}

impl Default for ContestConfig {
    fn default() -> Self {
        Self {
            expected_problem_count: 16,
            slots_per_problem: 2,
            solves_to_qualify: 2,
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
