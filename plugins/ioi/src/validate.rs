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
/// Its errors, which refuse the config, are what is wrong with the subtasks
/// whatever the test data:
///
/// - two subtasks with the same name, which contestants cannot tell apart;
/// - a subtask with no members, which can never score;
/// - a member written twice in one subtask, which a `sum` subtask counts
///   twice.
///
/// Its issues, which leave the config stored and list it on the problem's
/// config issues until the test data changes, are every reason the
/// subtasks cannot score `test_cases` as they stand:
///
/// - a member that names no test case, or names several (a label two test
///   cases share, or a label that is another test case's id), since scoring
///   would count it as 0 or as whichever case it happens to find;
/// - two members that name the same test case, which a `sum` subtask counts
///   twice;
/// - a `sum` subtask whose members all weigh 0, which can never score;
/// - a problem judging would refuse outright ([`unjudgeable`]), in the words
///   the refused submission records: one with no test cases yet, or none of
///   whose subtasks can score. The host checks the defaults (no subtasks) of
///   every problem in an IOI contest, so it shows before anyone submits.
///
/// Staff can then fix the test data and the subtasks in either order.
pub fn check_task(subtasks: &[SubtaskDef], test_cases: &[TestCaseRow]) -> ConfigValidateOutput {
    let mut output = ConfigValidateOutput::default();
    check_subtasks(subtasks, test_cases, &mut output);
    if let Some(reason) = unjudgeable(subtasks, test_cases) {
        let key = match reason {
            Unjudgeable::NoTestCases => "ioi.config.task.problemHasNoTestCases",
            Unjudgeable::NothingToScore => "ioi.config.task.nothingToScore",
        };
        output
            .issues
            .push(ConfigViolation::new("subtasks", reason.message()).key(key));
    }
    output
}

fn check_subtasks(
    subtasks: &[SubtaskDef],
    test_cases: &[TestCaseRow],
    output: &mut ConfigValidateOutput,
) {
    let index = MemberIndex::new(test_cases);
    let mut names = HashSet::new();
    for (i, subtask) in subtasks.iter().enumerate() {
        if !names.insert(subtask.name.as_str()) {
            output.errors.push(
                ConfigViolation::new(
                    format!("subtasks[{i}].name"),
                    format!("another subtask is already named '{}'", subtask.name),
                )
                .key("ioi.config.task.nameTaken")
                .param("name", subtask.name.as_str()),
            );
        }
        if subtask.test_cases.is_empty() {
            output.errors.push(
                ConfigViolation::new(
                    format!("subtasks[{i}].test_cases"),
                    "a subtask needs at least one test case",
                )
                .key("ioi.config.task.noTestCases"),
            );
            continue;
        }

        let mut written = HashSet::new();
        let mut members = HashSet::new();
        let mut total_weight = 0.0;
        let mut all_resolved = true;
        for (j, key) in subtask.test_cases.iter().enumerate() {
            let path = format!("subtasks[{i}].test_cases[{j}]");
            let twice = || {
                ConfigViolation::new(
                    path.clone(),
                    format!("test case '{key}' is already in this subtask"),
                )
                .key("ioi.config.task.duplicateTestCase")
                .param("testCase", key.as_str())
            };
            if !written.insert(key.as_str()) {
                output.errors.push(twice());
                continue;
            }
            match index.resolve(key) {
                Member::Unknown => {
                    all_resolved = false;
                    output.issues.push(
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
                        output.issues.push(twice());
                    }
                }
                Member::Ambiguous(count) => {
                    all_resolved = false;
                    output.issues.push(
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
            output.issues.push(
                ConfigViolation::new(
                    format!("subtasks[{i}].test_cases"),
                    "every test case of this sum subtask has score 0, so it can never score",
                )
                .key("ioi.config.task.zeroWeight"),
            );
        }
    }
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

    /// The paths of the errors and of the issues `check_task` reports.
    fn found(subtasks: &[SubtaskDef], cases: &[TestCaseRow]) -> (Vec<String>, Vec<String>) {
        let output = check_task(subtasks, cases);
        let paths = |v: &[ConfigViolation]| v.iter().map(|v| v.path.clone()).collect();
        (paths(&output.errors), paths(&output.issues))
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
        assert_eq!(found(&subtasks, &problem()), (vec![], vec![]));
    }

    /// Whether a member names a test case depends on the test data, so a
    /// member that names none is an issue the data or the subtask resolves,
    /// never a refusal.
    #[test]
    fn a_member_that_names_no_test_case_is_an_issue() {
        let subtasks = [subtask("S", SubtaskScoringMethod::Sum, &["a", "nope"])];
        let output = check_task(&subtasks, &problem());
        assert_eq!(output.errors, []);
        assert_eq!(paths(&output.issues), ["subtasks[0].test_cases[1]"]);
        assert!(output.issues[0].message.contains("'nope'"), "{output:?}");
    }

    #[test]
    fn a_label_two_test_cases_share_is_an_ambiguous_issue() {
        let cases = vec![tc(11, 10.0, Some("a")), tc(12, 10.0, Some("a"))];
        let subtasks = [subtask("S", SubtaskScoringMethod::GroupMin, &["a", "12"])];
        assert_eq!(
            found(&subtasks, &cases),
            (vec![], vec!["subtasks[0].test_cases[0]".to_string()])
        );
    }

    /// A member written twice is wrong whatever the test data; two members
    /// that name one test case are so only by the data.
    #[test]
    fn one_test_case_twice_in_a_subtask_is_refused_only_when_written_twice() {
        let written_twice = [subtask("S", SubtaskScoringMethod::Sum, &["a", "a"])];
        assert_eq!(
            found(&written_twice, &problem()),
            (vec!["subtasks[0].test_cases[1]".to_string()], vec![])
        );
        let named_twice = [subtask("S", SubtaskScoringMethod::Sum, &["a", "11"])];
        assert_eq!(
            found(&named_twice, &problem()),
            (vec![], vec!["subtasks[0].test_cases[1]".to_string()])
        );
    }

    #[test]
    fn subtasks_that_can_never_score_are_refused_or_reported_by_what_decides_it() {
        let subtasks = [
            subtask("Empty", SubtaskScoringMethod::GroupMin, &[]),
            subtask("Weightless", SubtaskScoringMethod::Sum, &["13"]),
            subtask("Pass/fail", SubtaskScoringMethod::GroupMin, &["13"]),
        ];
        assert_eq!(
            found(&subtasks, &problem()),
            (
                vec!["subtasks[0].test_cases".to_string()],
                vec!["subtasks[1].test_cases".to_string()]
            ),
            "an empty subtask is wrong by itself, a weightless one by its test cases' scores"
        );
    }

    #[test]
    fn two_subtasks_with_one_name_are_refused() {
        let subtasks = [
            subtask("S", SubtaskScoringMethod::GroupMin, &["a"]),
            subtask("S", SubtaskScoringMethod::GroupMin, &["b"]),
        ];
        assert_eq!(
            found(&subtasks, &problem()),
            (vec!["subtasks[1].name".to_string()], vec![])
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
