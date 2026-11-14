use serde::{Deserialize, Serialize};

use super::evaluate::JudgeFile;
use super::operation::{Environment, Step};
use super::verdict::Verdict;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckerVerdict {
    pub verdict: Verdict,
    pub score: f64,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OutputMode {
    Stream { channel: String },
    File { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckerStage {
    pub checker_env: Environment,
    pub steps: Vec<Step>,
    pub output_mode: OutputMode,
    pub result_step_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveCheckerInput {
    pub format: String,
    pub answer: JudgeFile,
    pub test_input: JudgeFile,
    /// Problem the checker is being resolved for. Checker plugins read their own
    /// per-problem config (the testlib checker source, the `tokens-float`
    /// tolerances) via `host.config.get_problem(problem_id, ...)` instead of
    /// receiving it inlined by the host.
    #[serde(default)]
    pub problem_id: Option<i32>,
    pub output_binding: OutputMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckerRunOutcome {
    pub exit_code: Option<i32>,
    pub stderr: String,
    pub sandbox_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpretCheckerInput {
    pub format: String,
    pub result: CheckerRunOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::JudgeFile;

    #[test]
    fn output_mode_stream_round_trips_with_tag() {
        let mode = OutputMode::Stream {
            channel: "verdict".to_string(),
        };

        let json = serde_json::to_value(&mode).unwrap();
        assert_eq!(json["mode"], "stream");
        assert_eq!(json["channel"], "verdict");

        let back: OutputMode = serde_json::from_value(json).unwrap();
        match back {
            OutputMode::Stream { channel } => assert_eq!(channel, "verdict"),
            _ => panic!("expected stream variant"),
        }
    }

    #[test]
    fn output_mode_file_round_trips_with_tag() {
        let mode = OutputMode::File {
            name: "result.json".to_string(),
        };

        let json = serde_json::to_value(&mode).unwrap();
        assert_eq!(json["mode"], "file");
        assert_eq!(json["name"], "result.json");

        let back: OutputMode = serde_json::from_value(json).unwrap();
        match back {
            OutputMode::File { name } => assert_eq!(name, "result.json"),
            _ => panic!("expected file variant"),
        }
    }

    #[test]
    fn checker_stage_round_trips() {
        use crate::types::{Environment, Step, StepKind};
        use crate::types::{IOConfig, RunOptions};

        let stage = CheckerStage {
            checker_env: Environment {
                id: "checker".to_string(),
                files_in: vec![],
            },
            steps: vec![Step {
                id: "run-checker".to_string(),
                kind: StepKind::Checker,
                env_ref: "checker".to_string(),
                argv: vec!["checker".to_string()],
                conf: RunOptions::default(),
                io: IOConfig::default(),
                collect: vec![],
                depends_on: vec![],
                cache: None,
                mounts: vec![],
            }],
            output_mode: OutputMode::Stream {
                channel: "verdict".to_string(),
            },
            result_step_id: "run-checker".to_string(),
        };

        let json = serde_json::to_value(&stage).unwrap();
        let back: CheckerStage = serde_json::from_value(json).unwrap();
        assert_eq!(back.checker_env.id, "checker");
        assert_eq!(back.steps.len(), 1);
        assert_eq!(back.result_step_id, "run-checker");
        match back.output_mode {
            OutputMode::Stream { channel } => assert_eq!(channel, "verdict"),
            _ => panic!("expected stream variant"),
        }
    }

    #[test]
    fn resolve_checker_input_round_trips() {
        let input = ResolveCheckerInput {
            format: "testlib".to_string(),
            answer: JudgeFile::inline("answer\n"),
            test_input: JudgeFile::inline("input\n"),
            problem_id: Some(42),
            config: Some(serde_json::json!({"strict": true})),
            output_binding: OutputMode::File {
                name: "result.json".to_string(),
            },
        };

        let json = serde_json::to_value(&input).unwrap();
        assert_eq!(json["output_binding"]["mode"], "file");

        let back: ResolveCheckerInput = serde_json::from_value(json).unwrap();
        assert_eq!(back.format, "testlib");
        assert_eq!(back.answer.inline_text(), "answer\n");
        assert_eq!(back.test_input.inline_text(), "input\n");
        assert_eq!(back.problem_id, Some(42));
        assert_eq!(back.config, Some(serde_json::json!({"strict": true})));
        match back.output_binding {
            OutputMode::File { name } => assert_eq!(name, "result.json"),
            _ => panic!("expected file variant"),
        }
    }

    #[test]
    fn checker_run_outcome_round_trips() {
        let result = CheckerRunOutcome {
            exit_code: Some(0),
            stderr: "ok".to_string(),
            sandbox_status: Some("OK".to_string()),
        };

        let json = serde_json::to_value(&result).unwrap();
        let back: CheckerRunOutcome = serde_json::from_value(json).unwrap();
        assert_eq!(back.exit_code, Some(0));
        assert_eq!(back.stderr, "ok");
        assert_eq!(back.sandbox_status.as_deref(), Some("OK"));
    }

    #[test]
    fn interpret_checker_input_round_trips() {
        let input = InterpretCheckerInput {
            format: "testlib".to_string(),
            result: CheckerRunOutcome {
                exit_code: Some(1),
                stderr: "wrong answer".to_string(),
                sandbox_status: None,
            },
        };

        let json = serde_json::to_value(&input).unwrap();
        let back: InterpretCheckerInput = serde_json::from_value(json).unwrap();
        assert_eq!(back.format, "testlib");
        assert_eq!(back.result.exit_code, Some(1));
        assert_eq!(back.result.stderr, "wrong answer");
        assert_eq!(back.result.sandbox_status, None);
    }
}
