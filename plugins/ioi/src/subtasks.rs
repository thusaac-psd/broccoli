use std::collections::{HashMap, HashSet};

#[cfg(test)]
use broccoli_server_sdk::types::TestCaseBodyRef;
use broccoli_server_sdk::types::TestCaseRow;

use crate::config::{SubtaskDef, SubtaskScoringMethod, resolve_tc_label, round_score};
use broccoli_server_sdk::score::total_score;

#[derive(Debug, Clone)]
pub struct SubtaskResult {
    pub name: String,
    pub score: f64,
    pub max_score: f64,
}

pub fn test_case_reference_keys(tc: &TestCaseRow) -> Vec<String> {
    let label = resolve_tc_label(tc);
    let id = tc.id.to_string();
    if label == id {
        vec![id]
    } else {
        vec![label, id]
    }
}

/// What a subtask member key (a test case's label, or its numeric id) names.
#[derive(Debug, Clone, Copy)]
pub enum Member<'a> {
    /// Exactly one test case has the key.
    One(&'a TestCaseRow),
    /// No test case has it.
    Unknown,
    /// This many test cases share it as their label.
    Ambiguous(usize),
}

/// Resolves subtask members to test cases, by the one rule every reader of a
/// subtask uses (the validator, scoring, weights, the short-circuit and the
/// scoring set): a key names a test case only when exactly one test case has
/// it. A label two test cases share names none of them. The validator
/// refuses such a member ("refer to one by its id"); when a test-data change
/// is forced past that check, the member counts as missing everywhere, never
/// as one of the cases that happen to share the label.
pub struct MemberIndex<'a> {
    by_key: HashMap<String, Vec<&'a TestCaseRow>>,
}

impl<'a> MemberIndex<'a> {
    pub fn new(test_cases: &'a [TestCaseRow]) -> Self {
        let mut by_key: HashMap<String, Vec<&'a TestCaseRow>> = HashMap::new();
        for tc in test_cases {
            for key in test_case_reference_keys(tc) {
                by_key.entry(key).or_default().push(tc);
            }
        }
        Self { by_key }
    }

    pub fn resolve(&self, key: &str) -> Member<'a> {
        match self.by_key.get(key).map(Vec::as_slice) {
            None | Some([]) => Member::Unknown,
            Some([tc]) => Member::One(tc),
            Some(several) => Member::Ambiguous(several.len()),
        }
    }

    /// The test case `key` names, if it names exactly one.
    pub fn test_case(&self, key: &str) -> Option<&'a TestCaseRow> {
        match self.resolve(key) {
            Member::One(tc) => Some(tc),
            Member::Unknown | Member::Ambiguous(_) => None,
        }
    }

    /// The weight member `key` carries in a `sum` subtask: the score of the
    /// test case it names, or 0 when it names none. A broken member adds
    /// nothing to a subtask's total weight, so the penalty for it does not
    /// depend on the scale of the other members' weights.
    pub fn weight(&self, key: &str) -> f64 {
        self.test_case(key).map_or(0.0, |tc| tc.score)
    }
}

/// Every subtask member key, with the test case id it names (when it names
/// exactly one) and the weight it carries ([`MemberIndex::weight`]). The task
/// config route serves these, so what a contestant sees per member agrees
/// with what scoring counted.
pub fn member_ids_and_weights(
    subtask_defs: &[SubtaskDef],
    test_cases: &[TestCaseRow],
) -> (HashMap<String, i32>, HashMap<String, f64>) {
    let members = MemberIndex::new(test_cases);
    let mut ids = HashMap::new();
    let mut weights = HashMap::new();
    for key in subtask_defs.iter().flat_map(|def| def.test_cases.iter()) {
        if let Some(tc) = members.test_case(key) {
            ids.insert(key.clone(), tc.id);
        }
        weights.insert(key.clone(), members.weight(key));
    }
    (ids, weights)
}

/// The members of `def` a viewer is shown. A member that names no single
/// test case (one a forced test data change left behind) is a problem for
/// staff to fix, so staff see it, and the task config's id map leaves it
/// out; a contestant is shown only members that name a test case.
pub fn shown_members<'d>(
    def: &'d SubtaskDef,
    test_cases: &[TestCaseRow],
    staff: bool,
) -> Vec<&'d str> {
    let members = MemberIndex::new(test_cases);
    def.test_cases
        .iter()
        .filter(|key| staff || members.test_case(key).is_some())
        .map(String::as_str)
        .collect()
}

/// Score a single subtask using the configured method. `tc_scores` holds
/// each judged test case's score ratio by its id. A member that names no
/// single test case ([`MemberIndex`]) scores 0 at weight 0: it fails a
/// `group_min` or `group_mul` subtask and adds nothing to a `sum` one.
pub fn score_subtask(
    def: &SubtaskDef,
    test_cases: &[TestCaseRow],
    tc_scores: &HashMap<i32, f64>,
) -> SubtaskResult {
    score_subtask_with_index(def, &MemberIndex::new(test_cases), tc_scores)
}

fn score_subtask_with_index(
    def: &SubtaskDef,
    members: &MemberIndex<'_>,
    tc_scores: &HashMap<i32, f64>,
) -> SubtaskResult {
    // Each member's score ratio and weight.
    let resolved: Vec<(f64, f64)> = def
        .test_cases
        .iter()
        .map(|key| {
            let ratio = members
                .test_case(key)
                .and_then(|tc| tc_scores.get(&tc.id).copied())
                .unwrap_or(0.0);
            (ratio, members.weight(key))
        })
        .collect();
    let score = if resolved.is_empty() {
        0.0
    } else {
        match def.scoring_method {
            SubtaskScoringMethod::GroupMin => {
                if resolved.iter().all(|(score, _)| *score >= 1.0) {
                    def.max_score
                } else {
                    0.0
                }
            }
            SubtaskScoringMethod::Sum => {
                let mut total_weight = 0.0;
                let mut weighted_sum = 0.0;
                for (score, weight) in &resolved {
                    if *weight <= 0.0 {
                        continue;
                    }
                    total_weight += weight;
                    weighted_sum += score * weight;
                }
                if total_weight > 0.0 {
                    def.max_score * (weighted_sum / total_weight)
                } else {
                    0.0
                }
            }
            SubtaskScoringMethod::GroupMul => {
                let product: f64 = resolved.iter().map(|(score, _)| score).product();
                def.max_score * product
            }
        }
    };

    SubtaskResult {
        name: def.name.clone(),
        score: round_score(score),
        max_score: subtask_max_score(def, members),
    }
}

/// Score all subtasks and return results in definition order. `tc_scores`
/// holds each judged test case's score ratio by its id.
pub fn score_all_subtasks(
    defs: &[SubtaskDef],
    test_cases: &[TestCaseRow],
    tc_scores: &HashMap<i32, f64>,
) -> Vec<SubtaskResult> {
    let members = MemberIndex::new(test_cases);
    defs.iter()
        .map(|def| score_subtask_with_index(def, &members, tc_scores))
        .collect()
}

/// Return test case IDs that should be evaluated for IOI scoring/feedback.
///
/// Samples are retained for feedback. Positive-score cases are retained for
/// normal scoring. Zero-score non-samples are retained only when an explicit
/// subtask member names them ([`MemberIndex`]).
pub fn compute_scoring_test_case_ids(
    subtask_defs: &[SubtaskDef],
    test_cases: &[TestCaseRow],
) -> Vec<i32> {
    let members = MemberIndex::new(test_cases);
    let in_a_subtask: HashSet<i32> = subtask_defs
        .iter()
        .flat_map(|def| def.test_cases.iter())
        .filter_map(|key| members.test_case(key))
        .map(|tc| tc.id)
        .collect();

    test_cases
        .iter()
        .filter(|tc| tc.is_sample || tc.score > 0.0 || in_a_subtask.contains(&tc.id))
        .map(|tc| tc.id)
        .collect()
}

/// Why a problem cannot be judged at all. Judging refuses such a problem with
/// a system error that stands, and the `task` config's validator reports it
/// as a config issue so staff see it before anyone submits; both read this
/// one rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unjudgeable {
    /// The problem has no test cases.
    NoTestCases,
    /// No subtask can score: none is worth points and names a test case
    /// that counts toward it ([`can_score`]), so every submission would
    /// score 0 whatever it does.
    NothingToScore,
}

impl Unjudgeable {
    /// The error code the refused submission records.
    pub fn code(self) -> &'static str {
        match self {
            Self::NoTestCases => "NO_TEST_CASES",
            Self::NothingToScore => "NOTHING_TO_SCORE",
        }
    }

    /// The English description the refused submission records.
    pub fn message(self) -> &'static str {
        match self {
            Self::NoTestCases => "Problem has no test cases",
            Self::NothingToScore => {
                "No subtask of the problem can score: none is worth points and has a test case that counts toward it"
            }
        }
    }
}

/// Whether `test_cases`, scored by `subtask_defs` (the problem's configured
/// subtasks, or its [`effective_subtasks`]), leave nothing to judge.
pub fn unjudgeable(subtask_defs: &[SubtaskDef], test_cases: &[TestCaseRow]) -> Option<Unjudgeable> {
    let members = MemberIndex::new(test_cases);
    if test_cases.is_empty() {
        Some(Unjudgeable::NoTestCases)
    } else if !effective_subtasks(subtask_defs, test_cases)
        .iter()
        .any(|def| can_score(def, &members))
    {
        Some(Unjudgeable::NothingToScore)
    } else {
        None
    }
}

/// Whether subtask `def` can score: it is worth points and one of its members
/// is a test case of the problem that counts toward it. A `sum` subtask
/// weighs its members by their points, so there a member counts only when it
/// is worth more than 0.
pub fn can_score(def: &SubtaskDef, members: &MemberIndex<'_>) -> bool {
    def.max_score > 0.0
        && def
            .test_cases
            .iter()
            .filter_map(|key| members.test_case(key))
            .any(|tc| def.scoring_method != SubtaskScoringMethod::Sum || tc.score > 0.0)
}

/// The subtasks a problem is scored by: its configured subtasks, or the
/// default "All Tests" subtask when none are configured. Scoring, feedback
/// and the scoreboard all read a problem's subtasks through this.
pub fn effective_subtasks(
    configured: &[SubtaskDef],
    test_cases: &[TestCaseRow],
) -> Vec<SubtaskDef> {
    if configured.is_empty() {
        build_default_subtasks(test_cases)
    } else {
        configured.to_vec()
    }
}

/// A problem's maximum score: the sum of what its subtasks are worth
/// ([`subtask_max_score`]), 0 for a problem none of whose subtasks can score.
pub fn problem_max_score(subtasks: &[SubtaskDef], test_cases: &[TestCaseRow]) -> f64 {
    let members = MemberIndex::new(test_cases);
    total_score(subtasks.iter().map(|def| subtask_max_score(def, &members)))
}

/// What subtask `def` is worth: its configured maximum when it can score
/// ([`can_score`]), else nothing. Judging, the scoreboard and a result all
/// read a subtask's maximum through this.
pub fn subtask_max_score(def: &SubtaskDef, members: &MemberIndex<'_>) -> f64 {
    if can_score(def, members) {
        def.max_score
    } else {
        0.0
    }
}

/// Build a default "All Tests" subtask (Sum scoring) over the non-sample,
/// positive-score test cases. Returns no subtasks when none qualify.
fn build_default_subtasks(test_cases: &[TestCaseRow]) -> Vec<SubtaskDef> {
    let scoring_test_cases: Vec<&TestCaseRow> = test_cases
        .iter()
        .filter(|tc| !tc.is_sample && tc.score > 0.0)
        .collect();
    if scoring_test_cases.is_empty() {
        return vec![];
    }
    let max_score = total_score(scoring_test_cases.iter().map(|tc| tc.score));
    // A label can be another case's id, so each case goes by a key that
    // names it alone: its label, else its id.
    let members = MemberIndex::new(test_cases);
    vec![SubtaskDef {
        name: "All Tests".into(),
        scoring_method: SubtaskScoringMethod::Sum,
        max_score,
        test_cases: scoring_test_cases
            .iter()
            .filter_map(|tc| {
                test_case_reference_keys(tc).into_iter().find(|key| {
                    members
                        .test_case(key)
                        .is_some_and(|named| named.id == tc.id)
                })
            })
            .collect(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_def(method: SubtaskScoringMethod, max_score: f64, labels: Vec<&str>) -> SubtaskDef {
        SubtaskDef {
            name: "Test".into(),
            scoring_method: method,
            max_score,
            test_cases: labels.into_iter().map(String::from).collect(),
        }
    }

    /// A weight-1 test case for every member of `def` (each member key is
    /// the case's numeric id), and `pairs` as the score ratios by id.
    fn scored(def: &SubtaskDef, pairs: &[(&str, f64)]) -> (Vec<TestCaseRow>, HashMap<i32, f64>) {
        let cases = def
            .test_cases
            .iter()
            .map(|key| test_case(key.parse().unwrap(), 1.0, false, None))
            .collect();
        let scores = pairs
            .iter()
            .map(|(k, v)| (k.parse().unwrap(), *v))
            .collect();
        (cases, scores)
    }

    fn test_case(id: i32, score: f64, is_sample: bool, label: Option<&str>) -> TestCaseRow {
        TestCaseRow {
            id,
            score,
            is_sample,
            position: id,
            description: None,
            label: label.map(String::from),
            input: TestCaseBodyRef::Missing,
            expected_output: TestCaseBodyRef::Missing,
            is_custom: false,
        }
    }

    #[test]
    fn a_member_naming_no_test_case_is_shown_to_staff_only() {
        let def = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec!["t1", "t2", "t3"]);
        let cases = vec![
            test_case(11, 0.0, false, Some("t1")),
            test_case(12, 0.0, false, Some("t2")),
        ];
        assert_eq!(shown_members(&def, &cases, true), ["t1", "t2", "t3"]);
        assert_eq!(shown_members(&def, &cases, false), ["t1", "t2"]);
        let (ids, _) = member_ids_and_weights(&[def], &cases);
        assert!(!ids.contains_key("t3"), "the id map leaves it out");
    }

    #[test]
    fn group_min_all_pass() {
        let def = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec!["1", "2", "3"]);
        let (cases, s) = scored(&def, &[("1", 1.0), ("2", 1.0), ("3", 1.0)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 30.0);
    }

    #[test]
    fn group_min_one_fail() {
        let def = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec!["1", "2", "3"]);
        let (cases, s) = scored(&def, &[("1", 1.0), ("2", 0.5), ("3", 1.0)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn group_min_empty() {
        let def = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec![]);
        let s = HashMap::new();
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn sum_proportional() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["1", "2"]);
        let (cases, s) = scored(&def, &[("1", 1.0), ("2", 1.0)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 100.0);
    }

    #[test]
    fn sum_partial_scores() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["1", "2"]);
        let (cases, s) = scored(&def, &[("1", 0.5), ("2", 0.5)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 50.0);
    }

    #[test]
    fn a_member_scores_the_same_named_by_its_label_or_its_id() {
        // tc id=42 has an explicit label "big_01"; the score map is keyed by
        // id, and a member resolves to the case whichever key names it.
        let tc = test_case(42, 30.0, false, Some("big_01"));
        let s: HashMap<i32, f64> = [(42, 1.0)].into_iter().collect();

        let group_min = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec!["42"]);
        assert_eq!(
            score_subtask(&group_min, std::slice::from_ref(&tc), &s).score,
            30.0
        );

        let sum = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["42"]);
        assert_eq!(score_subtask(&sum, &[tc], &s).score, 100.0);
    }

    #[test]
    fn sum_all_zero() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["1", "2"]);
        let (cases, s) = scored(&def, &[("1", 0.0), ("2", 0.0)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn sum_empty() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec![]);
        let s = HashMap::new();
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn group_mul_all_perfect() {
        let def = make_def(SubtaskScoringMethod::GroupMul, 50.0, vec!["1", "2"]);
        let (cases, s) = scored(&def, &[("1", 1.0), ("2", 1.0)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 50.0);
    }

    #[test]
    fn group_mul_one_half() {
        let def = make_def(SubtaskScoringMethod::GroupMul, 50.0, vec!["1", "2"]);
        let (cases, s) = scored(&def, &[("1", 1.0), ("2", 0.5)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 25.0);
    }

    #[test]
    fn group_mul_one_zero() {
        let def = make_def(SubtaskScoringMethod::GroupMul, 50.0, vec!["1", "2"]);
        let (cases, s) = scored(&def, &[("1", 1.0), ("2", 0.0)]);
        let result = score_subtask(&def, &cases, &s);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn missing_tc_treated_as_zero() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["1", "2", "3"]);
        let (cases, s) = scored(&def, &[("1", 1.0)]); // 2 and 3 unjudged
        let result = score_subtask(&def, &cases, &s);
        // 100 * (1.0 + 0.0 + 0.0) / 3 = 33.33
        assert_eq!(result.score, 33.33);
    }

    #[test]
    fn build_default_subtasks_creates_single_sum_group() {
        let test_cases = vec![
            TestCaseRow {
                id: 1,
                score: 30.0,
                is_sample: false,
                position: 0,
                description: None,
                label: Some("tc_1".into()),
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
                label: Some("tc_2".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let defs = build_default_subtasks(&test_cases);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].max_score, 100.0);
        assert_eq!(defs[0].scoring_method, SubtaskScoringMethod::Sum);
        assert_eq!(defs[0].test_cases, vec!["tc_1", "tc_2"]);
    }

    #[test]
    fn build_default_subtasks_fallback_to_id() {
        let test_cases = vec![
            TestCaseRow {
                id: 1,
                score: 50.0,
                is_sample: false,
                position: 0,
                description: None,
                label: None,
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
                label: None,
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];
        let defs = build_default_subtasks(&test_cases);
        assert_eq!(defs[0].test_cases, vec!["1", "2"]);
    }

    #[test]
    fn build_default_subtasks_ignores_samples_and_zero_point_cases() {
        let test_cases = vec![
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
                score: 0.0,
                is_sample: false,
                position: 1,
                description: None,
                label: Some("zero_01".into()),
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
                label: Some("tc_01".into()),
                input: TestCaseBodyRef::Missing,
                expected_output: TestCaseBodyRef::Missing,
                is_custom: false,
            },
        ];

        let defs = build_default_subtasks(&test_cases);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].max_score, 100.0);
        assert_eq!(defs[0].test_cases, vec!["tc_01"]);
    }

    #[test]
    fn problem_max_score_without_subtask_config_leaves_out_samples() {
        let test_cases = vec![
            test_case(1, 10.0, true, None),
            test_case(2, 90.0, false, None),
        ];
        assert_eq!(
            problem_max_score(&effective_subtasks(&[], &test_cases), &test_cases),
            90.0
        );
    }

    /// A problem with nothing to score has a maximum of 0, never `-0`.
    #[test]
    fn a_problem_with_no_subtasks_has_a_maximum_of_zero() {
        let max = problem_max_score(&effective_subtasks(&[], &[]), &[]);
        assert!(max == 0.0 && max.is_sign_positive(), "{max}");
        assert_eq!(serde_json::json!(max).to_string(), "0.0");
    }

    #[test]
    fn problem_max_score_with_subtask_config_sums_the_configured_maxima() {
        let configured = vec![
            make_def(SubtaskScoringMethod::Sum, 30.0, vec!["a"]),
            make_def(SubtaskScoringMethod::GroupMin, 70.0, vec!["b"]),
        ];
        let test_cases = vec![
            test_case(1, 10.0, false, Some("a")),
            test_case(2, 0.0, false, Some("b")),
        ];
        assert_eq!(
            problem_max_score(&effective_subtasks(&configured, &test_cases), &test_cases),
            100.0
        );
    }

    /// A subtask that can never score is worth nothing, in the problem's
    /// maximum and in its own result, so a result never reads as short of a
    /// maximum nobody can reach; a problem none of whose subtasks can score
    /// has a maximum of 0.
    #[test]
    fn a_subtask_that_cannot_score_is_worth_nothing() {
        let test_cases = vec![test_case(1, 10.0, false, Some("a"))];
        let scorable = make_def(SubtaskScoringMethod::Sum, 60.0, vec!["a"]);
        let missing = make_def(SubtaskScoringMethod::GroupMin, 100.0, vec!["gone"]);
        let defs = vec![scorable, missing.clone()];
        assert_eq!(problem_max_score(&defs, &test_cases), 60.0);
        let results = score_all_subtasks(&defs, &test_cases, &HashMap::new());
        assert_eq!(results[1].max_score, 0.0);
        assert_eq!(problem_max_score(&[missing], &test_cases), 0.0);
    }

    /// The default subtask names every case it scores by a key that names
    /// exactly that case, so a label that is another case's id (labels start
    /// from a position, ids from 1) never drops a case from the subtask, its
    /// score or the problem's maximum.
    #[test]
    fn the_default_subtask_names_each_case_by_a_key_only_it_has() {
        let test_cases = vec![
            test_case(1, 10.0, true, Some("0")),
            test_case(2, 90.0, false, Some("1")),
        ];
        let subtasks = effective_subtasks(&[], &test_cases);
        assert_eq!(problem_max_score(&subtasks, &test_cases), 90.0);
        let ratios: HashMap<i32, f64> = [(2, 1.0)].into_iter().collect();
        let results = score_all_subtasks(&subtasks, &test_cases, &ratios);
        assert_eq!(results[0].score, 90.0);
    }

    #[test]
    fn build_default_subtasks_empty() {
        let defs = build_default_subtasks(&[]);
        assert!(defs.is_empty());
    }

    #[test]
    fn compute_scoring_test_case_ids_keeps_nested_subtask_members() {
        let test_cases = vec![
            test_case(1, 0.0, true, Some("sample")),
            test_case(2, 0.0, false, Some("group_a_zero")),
            test_case(3, 20.0, false, Some("group_a_scored")),
            test_case(4, 0.0, false, Some("unused_zero")),
        ];
        let defs = vec![make_def(
            SubtaskScoringMethod::GroupMin,
            20.0,
            vec!["group_a_zero", "group_a_scored"],
        )];

        let ids = compute_scoring_test_case_ids(&defs, &test_cases);

        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn compute_scoring_test_case_ids_deduplicates_overlapping_subtasks_by_test_case_order() {
        let test_cases = vec![
            test_case(1, 0.0, false, Some("shared")),
            test_case(2, 0.0, false, Some("only_b")),
            test_case(3, 0.0, false, Some("unused")),
        ];
        let defs = vec![
            make_def(SubtaskScoringMethod::GroupMin, 10.0, vec!["shared"]),
            make_def(
                SubtaskScoringMethod::GroupMin,
                10.0,
                vec!["shared", "only_b"],
            ),
        ];

        let ids = compute_scoring_test_case_ids(&defs, &test_cases);

        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn compute_scoring_test_case_ids_without_subtasks_keeps_only_samples_and_positive_scores() {
        let test_cases = vec![
            test_case(1, 0.0, true, Some("sample")),
            test_case(2, 10.0, false, Some("scored")),
            test_case(3, 0.0, false, Some("unused_zero")),
        ];

        let ids = compute_scoring_test_case_ids(&[], &test_cases);

        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn compute_scoring_test_case_ids_keeps_labeled_zero_score_member_referenced_by_id() {
        let test_cases = vec![
            test_case(1, 0.0, false, Some("unused")),
            test_case(2, 0.0, false, Some("named_zero")),
        ];
        let defs = vec![make_def(SubtaskScoringMethod::GroupMin, 10.0, vec!["2"])];

        let ids = compute_scoring_test_case_ids(&defs, &test_cases);

        assert_eq!(ids, vec![2]);
    }

    /// A problem is judged only when some subtask can score; one whose
    /// subtasks name no test case it has scores nothing, whatever other
    /// cases it judges.
    #[test]
    fn nothing_to_score_means_no_subtask_can_score() {
        let cases = vec![
            test_case(1, 40.0, false, Some("t1")),
            test_case(2, 0.0, false, Some("free")),
            test_case(3, 0.0, true, Some("sample")),
        ];
        let missing = make_def(SubtaskScoringMethod::GroupMin, 100.0, vec!["gone", "99"]);
        assert_eq!(
            unjudgeable(std::slice::from_ref(&missing), &cases),
            Some(Unjudgeable::NothingToScore),
            "the positive-score case belongs to no subtask"
        );
        let worthless = make_def(SubtaskScoringMethod::GroupMin, 0.0, vec!["t1"]);
        assert_eq!(
            unjudgeable(&[worthless], &cases),
            Some(Unjudgeable::NothingToScore)
        );
        let weightless_sum = make_def(SubtaskScoringMethod::Sum, 50.0, vec!["free"]);
        assert_eq!(
            unjudgeable(&[weightless_sum], &cases),
            Some(Unjudgeable::NothingToScore)
        );

        let unweighted_group = make_def(SubtaskScoringMethod::GroupMin, 50.0, vec!["free"]);
        assert_eq!(
            unjudgeable(&[missing.clone(), unweighted_group], &cases),
            None
        );
        assert_eq!(
            unjudgeable(&[], &cases),
            None,
            "the default subtask scores t1"
        );
        assert_eq!(
            unjudgeable(&[], &cases[1..]),
            Some(Unjudgeable::NothingToScore),
            "a sample and a zero-point case leave the default subtask empty"
        );
    }

    /// A label two test cases share (a test-data change forced past the
    /// validator) names neither: scoring, the scoring set and the validator
    /// agree it is unresolved, and naming one of the cases by id resolves it
    /// everywhere.
    #[test]
    fn every_reader_resolves_an_ambiguous_label_the_same_way() {
        let cases = vec![
            test_case(1, 40.0, false, Some("t1")),
            test_case(2, 0.0, false, Some("dup")),
            test_case(3, 0.0, false, Some("dup")),
        ];
        let members = MemberIndex::new(&cases);
        assert!(matches!(members.resolve("dup"), Member::Ambiguous(2)));
        assert!(members.test_case("dup").is_none());
        assert_eq!(members.test_case("3").map(|tc| tc.id), Some(3));

        let scores: HashMap<i32, f64> = [(1, 1.0), (2, 1.0), (3, 1.0)].into_iter().collect();
        let by_label = make_def(SubtaskScoringMethod::GroupMin, 60.0, vec!["t1", "dup"]);
        assert_eq!(
            score_subtask(&by_label, &cases, &scores).score,
            0.0,
            "an ambiguous member counts as missing, never as one of the cases"
        );
        // Zero-weight cases stay out of the scoring set unless a member names
        // exactly one of them.
        assert_eq!(compute_scoring_test_case_ids(&[by_label], &cases), vec![1]);
        assert!(
            crate::validate::check_task(
                &[make_def(SubtaskScoringMethod::GroupMin, 60.0, vec!["dup"])],
                &cases
            )
            .issues
            .iter()
            .any(|v| v.key.as_deref() == Some("ioi.config.task.ambiguousTestCase"))
        );

        let by_id = make_def(SubtaskScoringMethod::GroupMin, 60.0, vec!["t1", "3"]);
        assert_eq!(score_subtask(&by_id, &cases, &scores).score, 60.0);
        assert_eq!(compute_scoring_test_case_ids(&[by_id], &cases), vec![1, 3]);
    }

    /// A sum subtask's score for a member that names no test case, or names
    /// two, does not depend on the scale of the other members' weights: the
    /// broken member weighs 0, and the per-member weights the task config
    /// serves say the same.
    #[test]
    fn a_broken_sum_member_weighs_nothing_at_any_weight_scale() {
        for weight in [25.0, 1.0] {
            let cases = vec![
                test_case(1, weight, false, Some("a")),
                test_case(2, weight, false, Some("b")),
                test_case(3, weight, false, Some("dup")),
                test_case(4, weight, false, Some("dup")),
            ];
            let def = make_def(
                SubtaskScoringMethod::Sum,
                100.0,
                vec!["a", "b", "missing", "dup"],
            );
            let scores: HashMap<i32, f64> = [(1, 1.0), (2, 0.0), (3, 1.0), (4, 1.0)]
                .into_iter()
                .collect();

            assert_eq!(
                score_subtask(&def, &cases, &scores).score,
                50.0,
                "weight scale {weight}"
            );

            let (ids, weights) = member_ids_and_weights(&[def], &cases);
            assert_eq!(ids.get("a"), Some(&1));
            assert_eq!(ids.get("missing"), None);
            assert_eq!(ids.get("dup"), None);
            assert_eq!(weights.get("a"), Some(&weight));
            assert_eq!(weights.get("missing"), Some(&0.0));
            assert_eq!(weights.get("dup"), Some(&0.0));
        }
    }
}
