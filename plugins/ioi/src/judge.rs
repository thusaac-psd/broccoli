#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;

use broccoli_server_sdk::prelude::*;

#[cfg(test)]
use crate::config::round_score;
use crate::config::{ContestConfig, SubtaskDef, TaskConfig};
#[cfg(test)]
use crate::evaluate_batch::evaluate_all;
use crate::evaluate_batch::evaluate_all_detached;
#[cfg(test)]
use crate::persist::persist_results;
use crate::subtasks::compute_scoring_test_case_ids;
#[cfg(test)]
use crate::subtasks::{score_all_subtasks, test_case_reference_keys};

/// Context gathered from host functions, passed to pure judge logic.
pub struct JudgeContext {
    pub contest_config: ContestConfig,
    pub task_config: TaskConfig,
    pub submission_id: i32,
    pub problem_id: i32,
    pub contest_id: i32,
    pub test_cases: Vec<TestCaseRow>,
    pub subtask_defs: Vec<SubtaskDef>,
}

/// Result from judge_with_context.
pub struct JudgeResult {
    pub output: OnSubmissionOutput,
    pub submission_score: Option<f64>,
    pub subtask_scores: Option<Vec<f64>>,
}

#[cfg(test)]
pub fn judge_with_context(
    host: &Host,
    req: &OnSubmissionInput,
    ctx: &JudgeContext,
) -> Result<JudgeResult, SdkError> {
    if ctx.test_cases.is_empty() {
        let _ = host
            .log
            .info("No test cases found, marking as judged with score 0");
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: ctx.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::Accepted)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            compile_output: None,
            error_code: None,
            error_message: None,
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }
        return Ok(JudgeResult {
            output: OnSubmissionOutput {
                success: true,
                error_message: None,
            },
            submission_score: Some(0.0),
            subtask_scores: Some(vec![]),
        });
    }

    let scoring_test_case_ids: HashSet<i32> =
        compute_scoring_test_case_ids(&ctx.subtask_defs, &ctx.test_cases)
            .into_iter()
            .collect();
    let scoring_test_cases: Vec<TestCaseRow> = ctx
        .test_cases
        .iter()
        .filter(|tc| scoring_test_case_ids.contains(&tc.id))
        .cloned()
        .collect();

    if scoring_test_cases.is_empty() {
        let _ =
            host.submission
                .delete_results(ctx.submission_id, req.judgement_id, req.judge_epoch);
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: ctx.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::Accepted)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            compile_output: None,
            error_code: None,
            error_message: None,
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }
        return Ok(JudgeResult {
            output: OnSubmissionOutput {
                success: true,
                error_message: None,
            },
            submission_score: Some(0.0),
            subtask_scores: Some(vec![0.0; ctx.subtask_defs.len()]),
        });
    }

    let outcomes = match evaluate_all(
        host,
        req,
        &scoring_test_cases,
        ctx.submission_id,
        |raw, tc| round_score(raw * tc.score),
        &ctx.subtask_defs,
    ) {
        Ok(outcomes) => outcomes,
        Err(SdkError::StaleEpoch) => {
            // Submission was rejudged. This execution is stale. Stop gracefully
            // without persisting anything (the new epoch's plugin will handle it).
            let _ = host.log.info(&format!(
                "Submission {} epoch {} is stale, stopping",
                ctx.submission_id, req.judge_epoch
            ));
            return Ok(JudgeResult {
                output: OnSubmissionOutput {
                    success: true,
                    error_message: None,
                },
                submission_score: None,
                subtask_scores: None,
            });
        }
        Err(e) => return Err(e),
    };

    let id_to_keys: HashMap<i32, Vec<String>> = ctx
        .test_cases
        .iter()
        .map(|tc| (tc.id, test_case_reference_keys(tc)))
        .collect();
    let tc_scores: HashMap<String, f64> = outcomes
        .iter()
        .filter(|o| !o.verdict.is_skipped_or_cancelled())
        .flat_map(|o| {
            id_to_keys
                .get(&o.test_case_id)
                .into_iter()
                .flat_map(|keys| keys.iter().cloned().map(|key| (key, o.raw_score)))
        })
        .collect();

    let subtask_results = score_all_subtasks(&ctx.subtask_defs, &ctx.test_cases, &tc_scores);
    let subtask_scores: Vec<f64> = subtask_results.iter().map(|r| r.score).collect();

    let submission_score = round_score(subtask_scores.iter().sum());

    let output = persist_results(
        host,
        ctx.submission_id,
        req.judgement_id,
        req.judge_epoch,
        &outcomes,
        submission_score,
    )?;

    Ok(JudgeResult {
        output,
        submission_score: Some(submission_score),
        subtask_scores: Some(subtask_scores),
    })
}

pub fn judge_with_context_detached(
    host: &Host,
    req: &OnSubmissionInput,
    ctx: &JudgeContext,
) -> Result<JudgeResult, SdkError> {
    if ctx.test_cases.is_empty() {
        let _ = host
            .log
            .info("No test cases found, marking as judged with score 0");
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: ctx.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::Accepted)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            compile_output: None,
            error_code: None,
            error_message: None,
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }
        return Ok(JudgeResult {
            output: OnSubmissionOutput {
                success: true,
                error_message: None,
            },
            submission_score: Some(0.0),
            subtask_scores: Some(vec![]),
        });
    }

    let scoring_test_case_ids: HashSet<i32> =
        compute_scoring_test_case_ids(&ctx.subtask_defs, &ctx.test_cases)
            .into_iter()
            .collect();
    let scoring_test_cases: Vec<TestCaseRow> = ctx
        .test_cases
        .iter()
        .filter(|tc| scoring_test_case_ids.contains(&tc.id))
        .cloned()
        .collect();

    if scoring_test_cases.is_empty() {
        let _ =
            host.submission
                .delete_results(ctx.submission_id, req.judgement_id, req.judge_epoch);
        let affected = host.submission.update(&SubmissionUpdate {
            submission_id: ctx.submission_id,
            judgement_id: req.judgement_id,
            judge_epoch: req.judge_epoch,
            status: Some(SubmissionStatus::Judged),
            verdict: Some(Some(Verdict::Accepted)),
            score: Some(0.0),
            time_used: Some(None),
            memory_used: Some(None),
            compile_output: None,
            error_code: None,
            error_message: None,
        })?;
        if affected == 0 {
            return Err(SdkError::StaleEpoch);
        }
        return Ok(JudgeResult {
            output: OnSubmissionOutput {
                success: true,
                error_message: None,
            },
            submission_score: Some(0.0),
            subtask_scores: Some(vec![0.0; ctx.subtask_defs.len()]),
        });
    }

    let output = evaluate_all_detached(
        host,
        req,
        &ctx.test_cases,
        &scoring_test_cases,
        ctx.submission_id,
        &ctx.subtask_defs,
    )?;

    Ok(JudgeResult {
        output,
        submission_score: None,
        subtask_scores: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::*;

    fn sample_input() -> OnSubmissionInput {
        OnSubmissionInput {
            submission_id: 1,
            judgement_id: 1,
            fire_after_judging: true,
            user_id: 1,
            problem_id: 10,
            contest_id: Some(1),
            files: vec![SourceFile {
                filename: "sol.cpp".into(),
                content: "int main(){}".into(),
            }],
            language: "cpp".into(),
            time_limit_ms: 1000,
            memory_limit_kb: 262144,
            problem_type: "standard".into(),
            test_cases: vec![],
            judge_epoch: 0,
            target_worker_id: None,
        }
    }

    fn default_ctx(test_cases: Vec<TestCaseRow>) -> JudgeContext {
        let subtask_defs = if test_cases.is_empty() {
            vec![]
        } else {
            crate::subtasks::build_default_subtasks(&test_cases)
        };
        JudgeContext {
            contest_config: ContestConfig::default(),
            task_config: TaskConfig::default(),
            submission_id: 1,
            problem_id: 10,
            contest_id: 1,
            test_cases,
            subtask_defs,
        }
    }

    fn explicit_subtask_ctx(
        test_cases: Vec<TestCaseRow>,
        subtask_defs: Vec<SubtaskDef>,
    ) -> JudgeContext {
        JudgeContext {
            task_config: TaskConfig {
                subtasks: subtask_defs.clone(),
            },
            subtask_defs,
            ..default_ctx(test_cases)
        }
    }

    #[test]
    fn all_accepted_flat_scoring() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 50.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 50.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 50.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = default_ctx(tcs);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert!(result.output.success);
        assert_eq!(result.submission_score, Some(100.0));
        assert_eq!(result.subtask_scores, Some(vec![100.0]));

        // Compiling + Running + terminal = 3 submission updates
        let updates = host.submission.updates();
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));
        assert_eq!(updates[1].status, Some(SubmissionStatus::Running));

        let sub = host.submission.last_update();
        assert_eq!(sub.score, Some(100.0));
        assert_eq!(sub.verdict, Some(Some(Verdict::Accepted)));
    }

    #[test]
    fn no_config_sample_does_not_contribute_to_score() {
        let host = Host::mock();
        host.submission.add_test_case(1, 0.0);
        host.submission.add_test_case(2, 100.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(2));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 0.0,
                is_sample: true,
                position: 0,
                description: None,
                label: Some("sample_01".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 100.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("tc_01".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = default_ctx(tcs);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert_eq!(result.submission_score, Some(0.0));
        assert_eq!(host.submission.last_update().score, Some(0.0));
    }

    #[test]
    fn judge_filters_zero_score_cases_unless_sample_or_subtask_member() {
        let host = Host::mock();
        host.submission.add_test_case(1, 0.0);
        host.submission.add_test_case(2, 0.0);
        host.submission.add_test_case(3, 100.0);
        host.submission.add_test_case(4, 0.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));
        host.eval.queue_result(TestCaseVerdict::accepted(3));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 0.0,
                is_sample: true,
                position: 0,
                description: None,
                label: Some("sample".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 0.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("subtask_zero".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 3,
                score: 100.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("scored".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 4,
                score: 0.0,
                is_sample: false,
                position: 3,
                description: None,
                label: Some("unused_zero".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![SubtaskDef {
                name: "Scoring".into(),
                scoring_method: SubtaskScoringMethod::Sum,
                max_score: 100.0,
                test_cases: vec!["subtask_zero".into(), "scored".into()],
            }],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert_eq!(result.submission_score, Some(100.0));
        let batch_inputs = host.eval.batch_inputs();
        assert_eq!(batch_inputs.len(), 1);
        let evaluated_ids: Vec<i32> = batch_inputs[0]
            .test_cases
            .iter()
            .map(|tc| tc.test_case_id)
            .collect();
        assert_eq!(evaluated_ids, vec![1, 2, 3]);
        assert_eq!(host.submission.results().len(), 3);
    }

    #[test]
    fn judge_scores_labeled_test_case_referenced_by_numeric_id() {
        let host = Host::mock();
        host.submission.add_test_case(7, 100.0);
        host.eval.queue_result(TestCaseVerdict::accepted(7));

        let tcs = vec![TestCaseRow {
            id: 7,
            score: 100.0,
            is_sample: false,
            position: 0,
            description: None,
            label: Some("named_case".into()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        }];
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![SubtaskDef {
                name: "By ID".into(),
                scoring_method: SubtaskScoringMethod::GroupMin,
                max_score: 100.0,
                test_cases: vec!["7".into()],
            }],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert_eq!(result.submission_score, Some(100.0));
        assert_eq!(result.subtask_scores, Some(vec![100.0]));
    }

    #[test]
    fn no_config_default_scoring_uses_test_case_scores_as_weights() {
        let host = Host::mock();
        host.submission.add_test_case(1, 10.0);
        host.submission.add_test_case(2, 90.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(2));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 10.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("small".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 90.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("large".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = default_ctx(tcs);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert_eq!(result.submission_score, Some(10.0));
        assert_eq!(result.subtask_scores, Some(vec![10.0]));
        assert_eq!(host.submission.last_update().score, Some(10.0));
    }

    #[test]
    fn partial_with_subtasks() {
        let host = Host::mock();
        host.submission.add_test_case(1, 30.0);
        host.submission.add_test_case(2, 30.0);
        host.submission.add_test_case(3, 40.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::accepted(2));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(3));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 30.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 30.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 3,
                score: 40.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("3".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];

        // Two subtasks: GroupMin for first 2 TCs, GroupMin for TC 3
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![
                SubtaskDef {
                    name: "Subtask 1".into(),
                    scoring_method: SubtaskScoringMethod::GroupMin,
                    max_score: 60.0,
                    test_cases: vec!["1".into(), "2".into()],
                },
                SubtaskDef {
                    name: "Subtask 2".into(),
                    scoring_method: SubtaskScoringMethod::GroupMin,
                    max_score: 40.0,
                    test_cases: vec!["3".into()],
                },
            ],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        // Subtask 1: all pass (1.0, 1.0) -> 60.0
        // Subtask 2: WA (score 0) -> 0.0
        assert_eq!(result.submission_score, Some(60.0));
        assert_eq!(result.subtask_scores, Some(vec![60.0, 0.0]));
    }

    #[test]
    fn compile_error() {
        let host = Host::mock();
        host.submission.add_test_case(1, 100.0);
        host.submission.add_test_case(2, 100.0);
        host.submission.add_test_case(3, 100.0);
        host.eval.queue_result(TestCaseVerdict::compile_error(1));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 100.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 100.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 3,
                score: 100.0,
                is_sample: false,
                position: 2,
                description: None,
                label: Some("3".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = default_ctx(tcs);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert!(result.output.success);
        // Compile-error-only runs never enter Running; they stay in Compiling
        // until the terminal CompilationError update.
        let updates = host.submission.updates();
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0].status, Some(SubmissionStatus::Compiling));

        let sub = host.submission.last_update();
        assert_eq!(sub.status, Some(SubmissionStatus::CompilationError));
        assert_eq!(sub.verdict, Some(None));

        let results = host.submission.results();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].test_case_id, Some(1));
        assert_eq!(results[0].verdict, Verdict::CompileError);
        assert_eq!(results[1].test_case_id, Some(2));
        assert_eq!(results[1].verdict, Verdict::Skipped);
        assert_eq!(results[1].message.as_deref(), Some("SKIPPED_SHORT_CIRCUIT"));
        assert_eq!(results[2].test_case_id, Some(3));
        assert_eq!(results[2].verdict, Verdict::Skipped);
        assert_eq!(results[2].message.as_deref(), Some("SKIPPED_SHORT_CIRCUIT"));
    }

    #[test]
    fn test_case_result_text_replaces_nul_bytes() {
        let host = Host::mock();
        host.submission.add_test_case(1, 100.0);
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::WrongAnswer,
            score: 0.0,
            time_used_ms: Some(10),
            memory_used_kb: Some(256),
            message: Some("bad\0message".into()),
            stdout: Some("out\0put".into()),
            stderr: Some("err\0or".into()),
        });

        let tcs = vec![TestCaseRow {
            id: 1,
            score: 100.0,
            is_sample: false,
            position: 0,
            description: None,
            label: Some("1".into()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        }];
        let ctx = default_ctx(tcs);
        judge_with_context(&host, &sample_input(), &ctx).unwrap();

        let rows = host.submission.results();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("bad\u{FFFD}message"));
        assert_eq!(rows[0].stdout.as_deref(), Some("out\u{FFFD}put"));
        assert_eq!(rows[0].stderr.as_deref(), Some("err\u{FFFD}or"));
    }

    #[test]
    fn polling_error_fills_system_error() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 50.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval
            .queue_result_error(SdkError::Other("poll failed".into()));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 50.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 50.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = default_ctx(tcs);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert!(result.output.success);
        let sub = host.submission.last_update();
        assert_eq!(sub.verdict, Some(Some(Verdict::SystemError)));
        assert!(host.eval.was_cancelled());
        // TC results: 1 Accepted + 1 SystemError (timeout fill)
        assert_eq!(host.submission.results().len(), 2);
    }

    #[test]
    fn empty_test_cases() {
        let host = Host::mock();
        let ctx = default_ctx(vec![]);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert_eq!(result.submission_score, Some(0.0));
        assert_eq!(result.subtask_scores, Some(vec![]));
        // No evaluation -> only 1 update (terminal), no Running
        assert_eq!(host.submission.updates().len(), 1);
        let sub = host.submission.last_update();
        assert_eq!(sub.verdict, Some(Some(Verdict::Accepted)));
        assert_eq!(sub.score, Some(0.0));
    }

    #[test]
    fn group_min_subtask_one_fail() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 50.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 2,
            verdict: Verdict::Accepted,
            score: 0.8,
            time_used_ms: None,
            memory_used_kb: None,
            message: None,
            stdout: None,
            stderr: None,
        });

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 50.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 50.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![SubtaskDef {
                name: "All".into(),
                scoring_method: SubtaskScoringMethod::GroupMin,
                max_score: 100.0,
                test_cases: vec!["1".into(), "2".into()],
            }],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();
        // GroupMin: TC 2 has score 0.8, not 1.0 -> subtask = 0
        assert_eq!(result.submission_score, Some(0.0));
    }

    #[test]
    fn group_mul_subtask() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 50.0);
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::Accepted,
            score: 0.8,
            time_used_ms: None,
            memory_used_kb: None,
            message: None,
            stdout: None,
            stderr: None,
        });
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 2,
            verdict: Verdict::Accepted,
            score: 0.5,
            time_used_ms: None,
            memory_used_kb: None,
            message: None,
            stdout: None,
            stderr: None,
        });

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 50.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 50.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![SubtaskDef {
                name: "All".into(),
                scoring_method: SubtaskScoringMethod::GroupMul,
                max_score: 100.0,
                test_cases: vec!["1".into(), "2".into()],
            }],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();
        // GroupMul: 100 * 0.8 * 0.5 = 40.0
        assert_eq!(result.submission_score, Some(40.0));
    }

    #[test]
    fn explicit_sum_subtask_uses_test_case_scores_as_weights() {
        let host = Host::mock();
        host.submission.add_test_case(1, 10.0);
        host.submission.add_test_case(2, 90.0);
        host.eval.queue_result(TestCaseVerdict::accepted(1));
        host.eval.queue_result(TestCaseVerdict::wrong_answer(2));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 10.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("small".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 90.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("large".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![SubtaskDef {
                name: "Weighted".into(),
                scoring_method: SubtaskScoringMethod::Sum,
                max_score: 100.0,
                test_cases: vec!["small".into(), "large".into()],
            }],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert_eq!(result.submission_score, Some(10.0));
        assert_eq!(result.subtask_scores, Some(vec![10.0]));
    }

    #[test]
    fn start_batch_failure() {
        let host = Host::mock();
        host.submission.add_test_case(1, 100.0);
        host.eval
            .queue_start_error(SdkError::HostCall("worker down".into()));

        let tcs = vec![TestCaseRow {
            id: 1,
            score: 100.0,
            is_sample: false,
            position: 0,
            description: None,
            label: Some("1".into()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        }];
        let ctx = default_ctx(tcs);
        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();

        assert!(result.output.success);
        // Batch never started -> only 1 update (terminal), no Running
        assert_eq!(host.submission.updates().len(), 1);
        let sub = host.submission.last_update();
        assert_eq!(sub.verdict, Some(Some(Verdict::SystemError)));
        assert_eq!(sub.score, Some(0.0));
        // SystemError rows inserted for all TCs
        assert_eq!(host.submission.results().len(), 1);
    }

    #[test]
    fn sum_scoring_partial() {
        let host = Host::mock();
        host.submission.add_test_case(1, 30.0);
        host.submission.add_test_case(2, 70.0);
        host.eval.queue_result(TestCaseVerdict {
            test_case_id: 1,
            verdict: Verdict::Accepted,
            score: 0.5,
            time_used_ms: Some(50),
            memory_used_kb: Some(1024),
            message: None,
            stdout: None,
            stderr: None,
        });
        host.eval.queue_result(TestCaseVerdict::accepted(2));

        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 30.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("1".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
            TestCaseRow {
                id: 2,
                score: 70.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let ctx = explicit_subtask_ctx(
            tcs,
            vec![SubtaskDef {
                name: "All".into(),
                scoring_method: SubtaskScoringMethod::Sum,
                max_score: 100.0,
                test_cases: vec!["1".into(), "2".into()],
            }],
        );

        let result = judge_with_context(&host, &sample_input(), &ctx).unwrap();
        // Sum weights by testcase score: 100 * ((0.5 * 30) + (1.0 * 70)) / 100 = 85.0
        assert_eq!(result.submission_score, Some(85.0));
    }
}
