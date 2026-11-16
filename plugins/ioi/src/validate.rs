//! What the `task` namespace's validate function says beyond its schema:
//! subtasks that the scoring in [`crate::subtasks`] would silently score
//! wrong for the problem's current test data, and a problem judging would
//! refuse whatever the subtasks say.

use std::collections::HashSet;

use broccoli_server_sdk::types::{ConfigValidateOutput, ConfigViolation, TestCaseRow};

use crate::config::{SubtaskDef, SubtaskScoringMethod};
use crate::subtasks::{Member, MemberIndex, Unjudgeable, unjudgeable};

/// The validate answer for `subtasks` over `test_cases`.
///
/// Its errors, which refuse the config, are every reason `subtasks` cannot
/// score `test_cases` as written:
///
/// - a member that names no test case, or names several (a label two test
///   cases share, or a label that is another test case's id), since scoring
///   would count it as 0 or as whichever case it happens to find;
/// - a member listed twice in one subtask, which a `sum` subtask counts
///   twice;
/// - a subtask with no members, or a `sum` subtask whose members all weigh
///   0, which can never score;
/// - two subtasks with the same name, which contestants cannot tell apart.
///
/// Its issue, which leaves the config stored and lists the problem on its
/// config issues, is a problem judging would refuse outright
/// ([`unjudgeable`]), in the words the refused submission records: one with
/// no test cases yet, or none of whose subtasks can score. The config still
/// saves, so staff can fix the test data and the subtasks in either order,
/// and the host checks the defaults (no subtasks) of every problem in an IOI
/// contest, so it shows before anyone submits.
pub fn check_task(subtasks: &[SubtaskDef], test_cases: &[TestCaseRow]) -> ConfigValidateOutput {
    let issues = unjudgeable(subtasks, test_cases)
        .map(|reason| {
            let key = match reason {
                Unjudgeable::NoTestCases => "ioi.config.task.problemHasNoTestCases",
                Unjudgeable::NothingToScore => "ioi.config.task.nothingToScore",
            };
            ConfigViolation::new("subtasks", reason.message()).key(key)
        })
        .into_iter()
        .collect();
    ConfigValidateOutput {
        errors: subtask_violations(subtasks, test_cases),
        issues,
    }
}

fn subtask_violations(subtasks: &[SubtaskDef], test_cases: &[TestCaseRow]) -> Vec<ConfigViolation> {
    let index = MemberIndex::new(test_cases);
    let mut violations = Vec::new();
    let mut names = HashSet::new();
    for (i, subtask) in subtasks.iter().enumerate() {
        if !names.insert(subtask.name.as_str()) {
            violations.push(
                ConfigViolation::new(
                    format!("subtasks[{i}].name"),
                    format!("another subtask is already named '{}'", subtask.name),
                )
                .key("ioi.config.task.nameTaken")
                .param("name", subtask.name.as_str()),
            );
        }
        if subtask.test_cases.is_empty() {
            violations.push(
                ConfigViolation::new(
                    format!("subtasks[{i}].test_cases"),
                    "a subtask needs at least one test case",
                )
                .key("ioi.config.task.noTestCases"),
            );
            continue;
        }

        let mut members = HashSet::new();
        let mut total_weight = 0.0;
        let mut all_resolved = true;
        for (j, key) in subtask.test_cases.iter().enumerate() {
            let path = format!("subtasks[{i}].test_cases[{j}]");
            match index.resolve(key) {
                Member::Unknown => {
                    all_resolved = false;
                    violations.push(
                        ConfigViolation::new(
                            path,
                            format!("this problem has no test case '{key}'"),
                        )
                        .key("ioi.config.task.unknownTestCase")
                        .param("testCase", key.as_str()),
                    );
                }
                Member::One(tc) => {
                    if members.insert(tc.id) {
                        total_weight += tc.score.max(0.0);
                    } else {
                        violations.push(
                            ConfigViolation::new(
                                path,
                                format!("test case '{key}' is already in this subtask"),
                            )
                            .key("ioi.config.task.duplicateTestCase")
                            .param("testCase", key.as_str()),
                        );
                    }
                }
                Member::Ambiguous(count) => {
                    all_resolved = false;
                    violations.push(
                        ConfigViolation::new(
                            path,
                            format!("'{key}' names {count} test cases; refer to one by its id"),
                        )
                        .key("ioi.config.task.ambiguousTestCase")
                        .param("testCase", key.as_str())
                        .param("count", count),
                    );
                }
            }
        }
        if subtask.scoring_method == SubtaskScoringMethod::Sum
            && all_resolved
            && total_weight <= 0.0
        {
            violations.push(
                ConfigViolation::new(
                    format!("subtasks[{i}].test_cases"),
                    "every test case of this sum subtask has score 0, so it can never score",
                )
                .key("ioi.config.task.zeroWeight"),
            );
        }
    }
    violations
}

#[cfg(test)]
mod tests {
    use broccoli_server_sdk::types::TestCaseBodyRef;

    use super::*;

    fn tc(id: i32, score: f64, label: Option<&str>) -> TestCaseRow {
        TestCaseRow {
            id,
            score,
            is_sample: false,
            position: id,
            description: None,
            label: label.map(String::from),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        }
    }

    fn subtask(name: &str, method: SubtaskScoringMethod, members: &[&str]) -> SubtaskDef {
        SubtaskDef {
            name: name.into(),
            scoring_method: method,
            max_score: 100.0,
            test_cases: members.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn paths(violations: &[ConfigViolation]) -> Vec<&str> {
        violations.iter().map(|v| v.path.as_str()).collect()
    }

    fn task_violations(subtasks: &[SubtaskDef], cases: &[TestCaseRow]) -> Vec<ConfigViolation> {
        check_task(subtasks, cases).errors
    }

    fn problem() -> Vec<TestCaseRow> {
        vec![
            tc(11, 10.0, Some("a")),
            tc(12, 10.0, Some("b")),
            tc(13, 0.0, None),
        ]
    }

    #[test]
    fn subtasks_over_existing_test_cases_by_label_or_id_are_accepted() {
        let subtasks = [
            subtask("S1", SubtaskScoringMethod::GroupMin, &["a", "12"]),
            subtask("S2", SubtaskScoringMethod::Sum, &["b", "13"]),
        ];
        assert_eq!(task_violations(&subtasks, &problem()), []);
    }

    #[test]
    fn a_member_that_names_no_test_case_is_refused() {
        let subtasks = [subtask("S", SubtaskScoringMethod::Sum, &["a", "nope"])];
        let violations = task_violations(&subtasks, &problem());
        assert_eq!(paths(&violations), ["subtasks[0].test_cases[1]"]);
        assert!(violations[0].message.contains("'nope'"), "{violations:?}");
    }

    #[test]
    fn a_label_two_test_cases_share_is_refused_as_ambiguous() {
        let cases = vec![tc(11, 10.0, Some("a")), tc(12, 10.0, Some("a"))];
        let subtasks = [subtask("S", SubtaskScoringMethod::GroupMin, &["a", "12"])];
        assert_eq!(
            paths(&task_violations(&subtasks, &cases)),
            ["subtasks[0].test_cases[0]"]
        );
    }

    #[test]
    fn the_same_test_case_twice_in_one_subtask_is_refused() {
        let subtasks = [subtask("S", SubtaskScoringMethod::Sum, &["a", "11"])];
        assert_eq!(
            paths(&task_violations(&subtasks, &problem())),
            ["subtasks[0].test_cases[1]"]
        );
    }

    #[test]
    fn subtasks_that_can_never_score_are_refused() {
        let subtasks = [
            subtask("Empty", SubtaskScoringMethod::GroupMin, &[]),
            subtask("Weightless", SubtaskScoringMethod::Sum, &["13"]),
            subtask("Pass/fail", SubtaskScoringMethod::GroupMin, &["13"]),
        ];
        assert_eq!(
            paths(&task_violations(&subtasks, &problem())),
            ["subtasks[0].test_cases", "subtasks[1].test_cases"]
        );
    }

    #[test]
    fn two_subtasks_with_one_name_are_refused() {
        let subtasks = [
            subtask("S", SubtaskScoringMethod::GroupMin, &["a"]),
            subtask("S", SubtaskScoringMethod::GroupMin, &["b"]),
        ];
        assert_eq!(
            paths(&task_violations(&subtasks, &problem())),
            ["subtasks[1].name"]
        );
    }

    /// A problem judging would refuse is an issue, never a refusal, by the
    /// same rule judging reads (`subtasks::unjudgeable`): its test data is
    /// not ready yet, and nothing in the subtasks is wrong.
    #[test]
    fn a_problem_with_nothing_to_judge_is_an_issue_not_a_refusal() {
        let keys = |subtasks: &[SubtaskDef], cases: &[TestCaseRow]| -> Vec<String> {
            let output = check_task(subtasks, cases);
            assert_eq!(output.errors, [], "nothing in the subtasks is wrong");
            output.issues.into_iter().filter_map(|v| v.key).collect()
        };
        assert_eq!(
            keys(&[], &[]),
            vec!["ioi.config.task.problemHasNoTestCases"]
        );
        assert_eq!(
            keys(&[], &[tc(1, 0.0, None)]),
            vec!["ioi.config.task.nothingToScore"]
        );
        assert!(keys(&[], &[tc(1, 10.0, None)]).is_empty());
        let mut sample = tc(1, 0.0, None);
        sample.is_sample = true;
        assert_eq!(
            keys(&[], &[sample]),
            vec!["ioi.config.task.nothingToScore"],
            "a sample is judged but scores nothing"
        );
    }
}
