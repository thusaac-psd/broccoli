use std::collections::HashSet;

use broccoli_server_sdk::prelude::*;

use crate::config::{ContestConfig, SubtaskDef, TaskConfig};
use crate::evaluate_batch::evaluate_all_detached;
use crate::subtasks::{Counting, compute_scoring_test_case_ids};

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

/// Judge a submission against `ctx`. A problem with nothing to score cannot
/// be judged: the submission ends as a system error that stands, without
/// being evaluated. Otherwise a detached evaluation starts and its callbacks
/// finish the submission.
pub fn judge_with_context_detached(
    host: &Host,
    req: &OnSubmissionInput,
    ctx: &JudgeContext,
) -> Result<OnSubmissionOutput, SdkError> {
    if let Some(why) = crate::subtasks::unjudgeable(&ctx.subtask_defs, &ctx.test_cases) {
        return refuse(host, req, why.code(), why.message());
    }

    // One membership, over all the problem's test cases as scoring resolves
    // subtask members: which cases run, what each records and what the
    // short-circuit follows.
    let counting = Counting::new(&ctx.subtask_defs, &ctx.test_cases);
    let scoring_test_case_ids: HashSet<i32> =
        compute_scoring_test_case_ids(&counting, &ctx.test_cases)
            .into_iter()
            .collect();
    let scoring_test_cases: Vec<TestCaseRow> = ctx
        .test_cases
        .iter()
        .filter(|tc| scoring_test_case_ids.contains(&tc.id))
        .cloned()
        .collect();

    evaluate_all_detached(
        host,
        req,
        &ctx.test_cases,
        &scoring_test_cases,
        ctx.submission_id,
        &ctx.subtask_defs,
        &counting,
    )
}

/// Finish the submission as a system error that stands: every attempt would
/// fail the same way until the problem changes.
fn refuse(
    host: &Host,
    req: &OnSubmissionInput,
    code: &str,
    message: &str,
) -> Result<OnSubmissionOutput, SdkError> {
    let _ = host.log.info(&format!(
        "Submission {} cannot be judged: {message}",
        req.submission_id
    ));
    let update = SubmissionUpdate::standing_system_error(
        req.submission_id,
        req.judgement_id,
        req.judge_epoch,
        code,
        message,
    );
    if host.submission.update(&update)? == 0 {
        return Err(SdkError::StaleEpoch);
    }
    Ok(OnSubmissionOutput::ok())
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

    /// Judge `ctx` and feed `results` in order through the real detached
    /// callback, the way the host delivers them.
    fn judge(host: &Host, ctx: &JudgeContext, results: Vec<TestCaseVerdict>) -> OnSubmissionOutput {
        let output = judge_with_context_detached(host, &sample_input(), ctx).unwrap();
        let Some(start) = host.eval.detached_windowed_requests().pop() else {
            assert!(results.is_empty(), "nothing was evaluated");
            return output;
        };
        let mut state = start.state;
        let total = start.batch.test_cases.len();
        for (i, result) in results.into_iter().enumerate() {
            let out = crate::evaluate_batch::handle_detached_eval_callback(
                host,
                DetachedEvaluateCallbackInput {
                    session_id: "judge".into(),
                    state,
                    event: DetachedEvaluateCallbackEvent::Result { result },
                    completed: i + 1,
                    total,
                },
            )
            .unwrap();
            state = out.state;
        }
        output
    }

    fn default_ctx(test_cases: Vec<TestCaseRow>) -> JudgeContext {
        let subtask_defs = crate::subtasks::effective_subtasks(&[], &test_cases);
        JudgeContext {
            contest_config: ContestConfig::schema_defaults(),
            task_config: TaskConfig::schema_defaults(),
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
        let result = judge(
            &host,
            &ctx,
            vec![TestCaseVerdict::accepted(1), TestCaseVerdict::accepted(2)],
        );

        assert!(result.success);
        assert_eq!(host.submission.last_update().score, Some(100.0));

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
        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::accepted(1),
                TestCaseVerdict::wrong_answer(2),
            ],
        );

        assert_eq!(host.submission.last_update().score, Some(0.0));
        assert_eq!(host.submission.last_update().score, Some(0.0));
    }

    #[test]
    fn a_case_that_counts_toward_no_subtask_records_no_score() {
        // A sample carrying a weight still counts toward nothing: its row
        // shows 0, as the total does, not its weight.
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 100.0);
        let tcs = vec![
            TestCaseRow {
                id: 1,
                score: 50.0,
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
        judge(
            &host,
            &default_ctx(tcs),
            vec![TestCaseVerdict::accepted(1), TestCaseVerdict::accepted(2)],
        );

        let recorded: Vec<(Option<i32>, f64)> = host
            .submission
            .results()
            .iter()
            .map(|row| (row.test_case_id, row.score))
            .collect();
        assert_eq!(recorded, vec![(Some(1), 0.0), (Some(2), 100.0)]);
        assert_eq!(host.submission.last_update().score, Some(100.0));
    }

    /// A label a judged sample shares with a case that is not judged names
    /// neither, over all the problem's test cases as scoring resolves it: the
    /// sample counts toward no subtask, so its row records nothing and its
    /// failure cuts short no subtask.
    #[test]
    fn a_label_a_judged_case_shares_with_one_not_judged_names_neither_in_judging_either() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 0.0);
        host.submission.add_test_case(3, 100.0);
        let row = |id: i32, score: f64, is_sample: bool, label: &str| TestCaseRow {
            id,
            score,
            is_sample,
            position: id,
            description: None,
            label: Some(label.into()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        };
        let ctx = explicit_subtask_ctx(
            vec![
                row(1, 50.0, true, "x"),
                row(2, 0.0, false, "x"),
                row(3, 100.0, false, "t"),
            ],
            vec![SubtaskDef {
                name: "S".into(),
                scoring_method: SubtaskScoringMethod::GroupMin,
                max_score: 100.0,
                test_cases: vec!["x".into(), "t".into()],
            }],
        );

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::wrong_answer(1),
                TestCaseVerdict::accepted(3),
            ],
        );

        let judged: Vec<i32> = host.eval.detached_windowed_requests()[0]
            .batch
            .test_cases
            .iter()
            .map(|tc| tc.test_case_id)
            .collect();
        assert_eq!(
            judged,
            vec![1, 3],
            "case 2 counts toward nothing and is no sample"
        );
        let recorded: Vec<(Option<i32>, f64)> = host
            .submission
            .results()
            .iter()
            .map(|row| (row.test_case_id, row.score))
            .collect();
        assert_eq!(recorded, vec![(Some(1), 0.0), (Some(3), 100.0)]);
        // The broken member fails the `group_min` subtask, as scoring reads it.
        assert_eq!(host.submission.last_update().score, Some(0.0));

        // Accepted, the sample still records nothing the total leaves out.
        let host = Host::mock();
        for (id, score) in [(1, 50.0), (2, 0.0), (3, 100.0)] {
            host.submission.add_test_case(id, score);
        }
        let sum = explicit_subtask_ctx(
            ctx.test_cases.clone(),
            vec![SubtaskDef {
                scoring_method: SubtaskScoringMethod::Sum,
                ..ctx.subtask_defs[0].clone()
            }],
        );
        judge(
            &host,
            &sum,
            vec![TestCaseVerdict::accepted(1), TestCaseVerdict::accepted(3)],
        );
        let recorded: Vec<(Option<i32>, f64)> = host
            .submission
            .results()
            .iter()
            .map(|row| (row.test_case_id, row.score))
            .collect();
        assert_eq!(recorded, vec![(Some(1), 0.0), (Some(3), 100.0)]);
        assert_eq!(host.submission.last_update().score, Some(100.0));
    }

    #[test]
    fn judge_filters_zero_score_cases_unless_sample_or_subtask_member() {
        let host = Host::mock();
        host.submission.add_test_case(1, 0.0);
        host.submission.add_test_case(2, 0.0);
        host.submission.add_test_case(3, 100.0);
        host.submission.add_test_case(4, 0.0);

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

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::accepted(1),
                TestCaseVerdict::accepted(2),
                TestCaseVerdict::accepted(3),
            ],
        );

        assert_eq!(host.submission.last_update().score, Some(100.0));
        let started = host.eval.detached_windowed_requests();
        assert_eq!(started.len(), 1);
        let evaluated_ids: Vec<i32> = started[0]
            .batch
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

        judge(&host, &ctx, vec![TestCaseVerdict::accepted(7)]);

        assert_eq!(host.submission.last_update().score, Some(100.0));
    }

    #[test]
    fn no_config_default_scoring_uses_test_case_scores_as_weights() {
        let host = Host::mock();
        host.submission.add_test_case(1, 10.0);
        host.submission.add_test_case(2, 90.0);

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
        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::accepted(1),
                TestCaseVerdict::wrong_answer(2),
            ],
        );

        assert_eq!(host.submission.last_update().score, Some(10.0));
        assert_eq!(host.submission.last_update().score, Some(10.0));
    }

    #[test]
    fn partial_with_subtasks() {
        let host = Host::mock();
        host.submission.add_test_case(1, 30.0);
        host.submission.add_test_case(2, 30.0);
        host.submission.add_test_case(3, 40.0);

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

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::accepted(1),
                TestCaseVerdict::accepted(2),
                TestCaseVerdict::wrong_answer(3),
            ],
        );

        // Subtask 1: all pass (1.0, 1.0) -> 60.0
        // Subtask 2: WA (score 0) -> 0.0
        assert_eq!(host.submission.last_update().score, Some(60.0));
    }

    #[test]
    fn compile_error() {
        let host = Host::mock();
        host.submission.add_test_case(1, 100.0);
        host.submission.add_test_case(2, 100.0);
        host.submission.add_test_case(3, 100.0);

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
        let result = judge(&host, &ctx, vec![TestCaseVerdict::compile_error(1)]);

        assert!(result.success);
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
        judge(
            &host,
            &ctx,
            vec![TestCaseVerdict {
                test_case_id: 1,
                verdict: Verdict::WrongAnswer,
                score: 0.0,
                time_used_ms: Some(10),
                memory_used_kb: Some(256),
                message: Some("bad message".into()),
                stdout: Some("out put".into()),
                stderr: Some("err or".into()),
                problem_failure: None,
            }],
        );

        let rows = host.submission.results();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message.as_deref(), Some("bad\u{FFFD}message"));
        assert_eq!(rows[0].stdout.as_deref(), Some("out\u{FFFD}put"));
        assert_eq!(rows[0].stderr.as_deref(), Some("err\u{FFFD}or"));
    }

    /// A problem with nothing to score cannot be judged: the submission is
    /// never compiled or run and never reads as Accepted. It ends as a
    /// system error that stands, under the plugin's own code.
    #[test]
    fn a_problem_with_nothing_to_score_refuses_every_submission() {
        let unscored = TestCaseRow {
            id: 1,
            score: 0.0,
            is_sample: false,
            position: 0,
            description: None,
            label: Some("tc_01".into()),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        };
        for (test_cases, code) in [
            (vec![], "NO_TEST_CASES"),
            (vec![unscored], "NOTHING_TO_SCORE"),
        ] {
            let host = Host::mock();
            judge(&host, &default_ctx(test_cases), vec![]);

            assert_eq!(host.submission.updates().len(), 1, "{code}: nothing ran");
            let sub = host.submission.last_update();
            assert_eq!(sub.status, Some(SubmissionStatus::Judged), "{code}");
            assert_eq!(sub.verdict, Some(Some(Verdict::SystemError)), "{code}");
            assert_eq!(sub.error_code, Some(Some(code.to_string())));
            assert!(sub.stands, "{code}: every attempt repeats it");
        }
    }

    #[test]
    fn group_min_subtask_one_fail() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 50.0);

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

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::accepted(1),
                TestCaseVerdict {
                    test_case_id: 2,
                    verdict: Verdict::Accepted,
                    score: 0.8,
                    time_used_ms: None,
                    memory_used_kb: None,
                    message: None,
                    stdout: None,
                    stderr: None,
                    problem_failure: None,
                },
            ],
        );
        // GroupMin: TC 2 has score 0.8, not 1.0 -> subtask = 0
        assert_eq!(host.submission.last_update().score, Some(0.0));
    }

    #[test]
    fn group_mul_subtask() {
        let host = Host::mock();
        host.submission.add_test_case(1, 50.0);
        host.submission.add_test_case(2, 50.0);

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

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict {
                    test_case_id: 1,
                    verdict: Verdict::Accepted,
                    score: 0.8,
                    time_used_ms: None,
                    memory_used_kb: None,
                    message: None,
                    stdout: None,
                    stderr: None,
                    problem_failure: None,
                },
                TestCaseVerdict {
                    test_case_id: 2,
                    verdict: Verdict::Accepted,
                    score: 0.5,
                    time_used_ms: None,
                    memory_used_kb: None,
                    message: None,
                    stdout: None,
                    stderr: None,
                    problem_failure: None,
                },
            ],
        );
        // GroupMul: 100 * 0.8 * 0.5 = 40.0
        assert_eq!(host.submission.last_update().score, Some(40.0));
    }

    #[test]
    fn explicit_sum_subtask_uses_test_case_scores_as_weights() {
        let host = Host::mock();
        host.submission.add_test_case(1, 10.0);
        host.submission.add_test_case(2, 90.0);

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

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict::accepted(1),
                TestCaseVerdict::wrong_answer(2),
            ],
        );

        assert_eq!(host.submission.last_update().score, Some(10.0));
    }

    #[test]
    fn sum_scoring_partial() {
        let host = Host::mock();
        host.submission.add_test_case(1, 30.0);
        host.submission.add_test_case(2, 70.0);

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

        judge(
            &host,
            &ctx,
            vec![
                TestCaseVerdict {
                    test_case_id: 1,
                    verdict: Verdict::Accepted,
                    score: 0.5,
                    time_used_ms: Some(50),
                    memory_used_kb: Some(1024),
                    message: None,
                    stdout: None,
                    stderr: None,
                    problem_failure: None,
                },
                TestCaseVerdict::accepted(2),
            ],
        );
        // Sum weights by testcase score: 100 * ((0.5 * 30) + (1.0 * 70)) / 100 = 85.0
        assert_eq!(host.submission.last_update().score, Some(85.0));
    }
}
