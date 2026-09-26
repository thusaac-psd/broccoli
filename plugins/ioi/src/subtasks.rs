use std::collections::{HashMap, HashSet};

#[cfg(test)]
use broccoli_server_sdk::types::TestCaseBodyRef;
use broccoli_server_sdk::types::TestCaseRow;

use crate::config::{SubtaskDef, SubtaskScoringMethod, resolve_tc_label, round_score};

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

fn test_case_weights(test_cases: &[TestCaseRow]) -> HashMap<String, f64> {
    let mut weights = HashMap::new();
    for tc in test_cases {
        for key in test_case_reference_keys(tc) {
            weights.insert(key, tc.score);
        }
    }
    weights
}

/// Make every scored test case reachable by BOTH its label and its numeric id.
/// The incoming `tc_scores` map is keyed by label only, but a `SubtaskDef` may
/// reference a member by numeric id (e.g. `"42"`); without this the id-keyed
/// lookup misses and the member silently scores 0 (dragging `Sum`, zeroing
/// `GroupMin`/`GroupMul`). Mirrors [`test_case_weights`], which already keys by
/// both via [`test_case_reference_keys`].
fn scores_by_all_keys(
    test_cases: &[TestCaseRow],
    tc_scores: &HashMap<String, f64>,
) -> HashMap<String, f64> {
    let mut out = tc_scores.clone();
    for tc in test_cases {
        if let Some(&score) = tc_scores.get(&resolve_tc_label(tc)) {
            for key in test_case_reference_keys(tc) {
                out.entry(key).or_insert(score);
            }
        }
    }
    out
}

/// Score a single subtask using the configured method.
pub fn score_subtask(
    def: &SubtaskDef,
    test_cases: &[TestCaseRow],
    tc_scores: &HashMap<String, f64>,
) -> SubtaskResult {
    let weights = test_case_weights(test_cases);
    let tc_scores = scores_by_all_keys(test_cases, tc_scores);
    score_subtask_with_weights(def, &weights, &tc_scores)
}

fn score_subtask_with_weights(
    def: &SubtaskDef,
    test_case_weights: &HashMap<String, f64>,
    tc_scores: &HashMap<String, f64>,
) -> SubtaskResult {
    let score = if def.test_cases.is_empty() {
        0.0
    } else {
        match def.scoring_method {
            SubtaskScoringMethod::GroupMin => {
                let all_pass = def
                    .test_cases
                    .iter()
                    .all(|label| tc_scores.get(label).copied().unwrap_or(0.0) >= 1.0);
                if all_pass { def.max_score } else { 0.0 }
            }
            SubtaskScoringMethod::Sum => {
                let mut total_weight = 0.0;
                let mut weighted_sum = 0.0;
                for label in &def.test_cases {
                    let weight = test_case_weights.get(label).copied().unwrap_or(1.0);
                    if weight <= 0.0 {
                        continue;
                    }
                    total_weight += weight;
                    weighted_sum += tc_scores.get(label).copied().unwrap_or(0.0) * weight;
                }
                if total_weight > 0.0 {
                    def.max_score * (weighted_sum / total_weight)
                } else {
                    0.0
                }
            }
            SubtaskScoringMethod::GroupMul => {
                let product: f64 = def
                    .test_cases
                    .iter()
                    .map(|label| tc_scores.get(label).copied().unwrap_or(0.0))
                    .product();
                def.max_score * product
            }
        }
    };

    SubtaskResult {
        name: def.name.clone(),
        score: round_score(score),
        max_score: def.max_score,
    }
}

/// Score all subtasks and return results in definition order.
pub fn score_all_subtasks(
    defs: &[SubtaskDef],
    test_cases: &[TestCaseRow],
    tc_scores: &HashMap<String, f64>,
) -> Vec<SubtaskResult> {
    let weights = test_case_weights(test_cases);
    let tc_scores = scores_by_all_keys(test_cases, tc_scores);
    defs.iter()
        .map(|def| score_subtask_with_weights(def, &weights, &tc_scores))
        .collect()
}

/// Return test case IDs that should be evaluated for IOI scoring/feedback.
///
/// Samples are retained for feedback. Positive-score cases are retained for
/// normal scoring. Zero-score non-samples are retained only when an explicit
/// subtask references their resolved label or numeric ID.
pub fn compute_scoring_test_case_ids(
    subtask_defs: &[SubtaskDef],
    test_cases: &[TestCaseRow],
) -> Vec<i32> {
    let subtask_labels: HashSet<&str> = subtask_defs
        .iter()
        .flat_map(|def| def.test_cases.iter().map(String::as_str))
        .collect();

    test_cases
        .iter()
        .filter(|tc| {
            tc.is_sample
                || tc.score > 0.0
                || test_case_reference_keys(tc)
                    .iter()
                    .any(|key| subtask_labels.contains(key.as_str()))
        })
        .map(|tc| tc.id)
        .collect()
}

/// Build a default "All Tests" subtask (Sum scoring) over the non-sample,
/// positive-score test cases. Returns no subtasks when none qualify.
///
/// Used when no subtask definitions are configured.
pub fn build_default_subtasks(test_cases: &[TestCaseRow]) -> Vec<SubtaskDef> {
    let scoring_test_cases: Vec<&TestCaseRow> = test_cases
        .iter()
        .filter(|tc| !tc.is_sample && tc.score > 0.0)
        .collect();
    if scoring_test_cases.is_empty() {
        return vec![];
    }
    let total_score: f64 = scoring_test_cases.iter().map(|tc| tc.score).sum();
    vec![SubtaskDef {
        name: "All Tests".into(),
        scoring_method: SubtaskScoringMethod::Sum,
        max_score: total_score,
        test_cases: scoring_test_cases
            .iter()
            .map(|tc| resolve_tc_label(tc))
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

    fn scores(pairs: &[(&str, f64)]) -> HashMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
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
    fn group_min_all_pass() {
        let def = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec!["1", "2", "3"]);
        let s = scores(&[("1", 1.0), ("2", 1.0), ("3", 1.0)]);
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 30.0);
    }

    #[test]
    fn group_min_one_fail() {
        let def = make_def(SubtaskScoringMethod::GroupMin, 30.0, vec!["1", "2", "3"]);
        let s = scores(&[("1", 1.0), ("2", 0.5), ("3", 1.0)]);
        let result = score_subtask(&def, &[], &s);
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
        let s = scores(&[("1", 1.0), ("2", 1.0)]);
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 100.0);
    }

    #[test]
    fn sum_partial_scores() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["1", "2"]);
        let s = scores(&[("1", 0.5), ("2", 0.5)]);
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 50.0);
    }

    #[test]
    fn subtask_member_referenced_by_numeric_id_scores_via_label_keyed_map() {
        // tc id=42 has an explicit label "big_01"; the score map is keyed by
        // label. A subtask that references the member by its numeric id must
        // still resolve it (regression for the id-vs-label lookup bug).
        let tc = test_case(42, 30.0, false, Some("big_01"));
        let s = scores(&[("big_01", 1.0)]);

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
        let s = scores(&[("1", 0.0), ("2", 0.0)]);
        let result = score_subtask(&def, &[], &s);
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
        let s = scores(&[("1", 1.0), ("2", 1.0)]);
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 50.0);
    }

    #[test]
    fn group_mul_one_half() {
        let def = make_def(SubtaskScoringMethod::GroupMul, 50.0, vec!["1", "2"]);
        let s = scores(&[("1", 1.0), ("2", 0.5)]);
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 25.0);
    }

    #[test]
    fn group_mul_one_zero() {
        let def = make_def(SubtaskScoringMethod::GroupMul, 50.0, vec!["1", "2"]);
        let s = scores(&[("1", 1.0), ("2", 0.0)]);
        let result = score_subtask(&def, &[], &s);
        assert_eq!(result.score, 0.0);
    }

    #[test]
    fn missing_tc_treated_as_zero() {
        let def = make_def(SubtaskScoringMethod::Sum, 100.0, vec!["1", "2", "3"]);
        let s = scores(&[("1", 1.0)]); // 2 and 3 missing
        let result = score_subtask(&def, &[], &s);
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
}
