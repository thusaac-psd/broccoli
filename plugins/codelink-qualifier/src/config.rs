use broccoli_server_sdk::types::ConfigViolation;
use broccoli_server_sdk::{Host, error::SdkError};
use serde::{Deserialize, Serialize};

/// The `contest` config namespace. Its defaults and bounds live in the
/// namespace's schema in `plugin.toml`; the host fills the defaults in, so
/// every field is present here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

impl ContestConfig {
    pub fn load(host: &Host, contest_id: i32) -> Result<Self, SdkError> {
        // Invalid stored values must not silently revert a live contest to
        // different qualification rules.
        let config: Self = host.config.get_contest(contest_id, "contest")?.parse()?;
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

    /// The most contestants who can qualify directly in a contest whose
    /// problems are worth `problem_points`: a problem credits its points to
    /// at most `slots_per_problem` contestants, and a direct qualifier holds
    /// at least `solves_to_qualify` credited points.
    pub fn most_direct_qualifiers(&self, problem_points: &[usize]) -> usize {
        let credit: usize = problem_points
            .iter()
            .map(|points| points * self.slots_per_problem)
            .sum();
        credit / self.solves_to_qualify
    }

    /// What the `contest` namespace's places do not fit in a contest whose
    /// problems are worth `problem_points`: fewer advancement places than
    /// contestants who can qualify directly, since every direct qualifier is
    /// promised a place. It depends on the contest's problems as well as the
    /// config, so it is an issue for staff to resolve either way, never a
    /// refusal of a save.
    pub fn capacity_issues(&self, problem_points: &[usize]) -> Vec<ConfigViolation> {
        let most = self.most_direct_qualifiers(problem_points);
        if self.advancement_slots >= most {
            return Vec::new();
        }
        vec![
            ConfigViolation::new(
                "advancement_slots",
                format!(
                    "up to {most} contestants can qualify directly, more than the {} advancement places",
                    self.advancement_slots
                ),
            )
            .key("codelink-qualifier.config.placesBelowDirectQualifiers")
            .param("most", most)
            .param("places", self.advancement_slots),
        ]
    }
}

/// The `problem` config namespace, schema-filled like [`ContestConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProblemConfig {
    pub points: usize,
}

impl ProblemConfig {
    pub fn load(host: &Host, contest_id: i32, problem_id: i32) -> Result<Self, SdkError> {
        let config: Self = host
            .config
            .get_contest_problem(contest_id, problem_id, "problem")?
            .parse()?;
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
