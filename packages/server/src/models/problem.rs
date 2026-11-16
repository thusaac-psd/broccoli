use std::str::FromStr;

use axum::body::Bytes;
use axum_typed_multipart::{TryFromField, TryFromMultipart};
use chrono::{DateTime, Utc};
use sea_orm::DerivePartialModel;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::error::AppError;
use crate::utils::filename::validate_flat_filename;
use crate::utils::test_case_body::{test_case_body_preview, test_case_body_size};

pub use super::shared::{Pagination, escape_like};
use super::shared::{
    double_option, validate_bulk_ids, validate_optional_position, validate_reorder_ids,
    validate_title,
};
pub use broccoli_api_types::problem::{
    CreateProblemRequest, CreateTestCaseRequest, ProblemResponse, SampleTestCaseMeta,
    TestCaseListItem, TestCaseResponse,
};

#[derive(Deserialize, Default, PartialEq, utoipa::ToSchema)]
pub struct UpdateProblemRequest {
    #[schema(example = "Two Sum (Easy)")]
    pub title: Option<String>,
    #[schema(example = "Updated problem statement...")]
    pub content: Option<String>,
    #[schema(example = 2000)]
    pub time_limit: Option<i32>,
    #[schema(example = 524288)]
    pub memory_limit: Option<i32>,
    #[schema(example = "batch")]
    pub problem_type: Option<String>,
    #[schema(example = "ignore_case")]
    pub checker_format: Option<String>,
    #[schema(example = "ioi")]
    pub default_contest_type: Option<String>,
    #[schema(example = true)]
    pub show_test_details: Option<bool>,
    #[schema(example = true)]
    pub is_public: Option<bool>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<std::collections::HashMap<String, Vec<String>>>, example = json!({"cpp": ["solution.cpp"], "java": ["Main.java"]}))]
    pub submission_format: Option<Option<std::collections::HashMap<String, Vec<String>>>>,
}

// A `problem` projection; see `ContestListItem` for why it is a partial model.
/// One problem in a problem list.
#[derive(Serialize, DerivePartialModel, utoipa::ToSchema)]
#[sea_orm(entity = "crate::entity::problem::Entity")]
pub struct ProblemListItem {
    #[schema(example = 1)]
    pub id: i32,
    #[schema(example = "Two Sum")]
    pub title: String,
    #[schema(example = 1000)]
    pub time_limit: i32,
    #[schema(example = 262144)]
    pub memory_limit: i32,
    #[schema(example = "batch")]
    pub problem_type: String,
    #[schema(example = "exact")]
    pub checker_format: String,
    #[schema(example = "ioi")]
    pub default_contest_type: String,
    #[schema(example = false)]
    pub show_test_details: bool,
    #[schema(example = false)]
    pub is_public: bool,
    #[schema(example = "2025-09-01T08:00:00Z")]
    pub created_at: DateTime<Utc>,
    #[schema(example = "2025-09-01T08:30:00Z")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct ProblemListResponse {
    pub data: Vec<ProblemListItem>,
    pub pagination: Pagination,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ProblemListQuery {
    #[param(example = 1)]
    pub page: Option<u64>,
    #[param(example = 20)]
    pub per_page: Option<u64>,
    #[param(example = "sum")]
    pub search: Option<String>,
    #[param(example = "created_at")]
    pub sort_by: Option<String>,
    #[param(example = "desc")]
    pub sort_order: Option<String>,
}

#[derive(Deserialize, Default, PartialEq, utoipa::ToSchema)]
pub struct UpdateTestCaseRequest {
    #[schema(example = "5\n1 2 3 4 5\n3")]
    pub input: Option<String>,
    #[schema(example = "1 2")]
    pub expected_output: Option<String>,
    #[schema(example = 20)]
    pub score: Option<i32>,
    #[schema(example = false)]
    pub is_sample: Option<bool>,
    #[schema(example = 1)]
    pub position: Option<i32>,
    #[serde(default, deserialize_with = "double_option")]
    #[schema(value_type = Option<String>, example = "Updated edge case")]
    pub description: Option<Option<String>>,
    #[schema(example = "sample_01")]
    pub label: Option<String>,
}

#[derive(Deserialize, Serialize, TryFromField, utoipa::ToSchema)]
#[try_from_field(rename_all = "snake_case")]
#[serde(rename_all = "snake_case")]
pub enum UploadTestCasesMergeStrategy {
    Abort,
    Skip,
    Overwrite,
    Replace,
}

#[derive(TryFromMultipart, utoipa::ToSchema)]
pub struct UploadTestCasesRequest {
    #[form_data(limit = "unlimited")]
    #[schema(value_type = String, format = Binary)]
    pub file: Bytes,
    #[schema(example = "input_*.txt")]
    pub input_format: String,
    #[schema(example = "output_*.txt")]
    pub output_format: String,
    #[schema(example = "abort")]
    pub strategy: UploadTestCasesMergeStrategy,
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct ReorderTestCasesRequest {
    #[schema(example = json!([3, 1, 2]))]
    pub test_case_ids: Vec<i32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct UploadTestCasesResponse {
    #[schema(example = 5)]
    pub created: usize,
    #[schema(example = 2)]
    pub updated: usize,
    pub test_cases: Vec<TestCaseListItem>,
}

impl From<crate::entity::problem::Model> for ProblemResponse {
    fn from(m: crate::entity::problem::Model) -> Self {
        let submission_format: Option<std::collections::HashMap<String, Vec<String>>> = m
            .submission_format
            .and_then(|v| serde_json::from_value(v).ok());
        Self {
            id: m.id,
            title: m.title,
            content: m.content,
            time_limit: m.time_limit,
            memory_limit: m.memory_limit,
            problem_type: m.problem_type,
            checker_format: m.checker_format,
            default_contest_type: m.default_contest_type,
            show_test_details: m.show_test_details,
            is_public: m.is_public,
            submission_format,
            samples: vec![],
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl From<crate::entity::test_case::Model> for TestCaseResponse {
    fn from(m: crate::entity::test_case::Model) -> Self {
        Self {
            id: m.id,
            input_size: test_case_body_size(&m.input, m.input_size),
            output_size: test_case_body_size(&m.expected_output, m.expected_output_size),
            input_preview: test_case_body_preview(&m.input, m.input_preview.as_deref()),
            output_preview: test_case_body_preview(
                &m.expected_output,
                m.expected_output_preview.as_deref(),
            ),
            input: m.input,
            expected_output: m.expected_output,
            score: m.score,
            description: m.description,
            label: m.label,
            is_sample: m.is_sample,
            position: m.position,
            problem_id: m.problem_id,
            created_at: m.created_at,
        }
    }
}

impl FromStr for UploadTestCasesMergeStrategy {
    type Err = AppError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "replace" => Ok(Self::Replace),
            "overwrite" => Ok(Self::Overwrite),
            "skip" => Ok(Self::Skip),
            "abort" => Ok(Self::Abort),
            _ => Err(AppError::Validation(
                "Invalid merge strategy. Must be one of: replace, overwrite, skip, abort".into(),
            )),
        }
    }
}

pub const PREVIEW_LENGTH: usize = 100;

pub fn truncate_preview(s: &str) -> String {
    match s.char_indices().nth(PREVIEW_LENGTH) {
        Some((byte_idx, _)) => format!("{}...", &s[..byte_idx]),
        None => s.to_string(),
    }
}

use crate::registry::{CheckerStageRegistry, ContestTypeRegistry, EvaluatorRegistry};

pub async fn validate_checker_format(
    format: &str,
    registry: &CheckerStageRegistry,
) -> Result<(), AppError> {
    let reg = registry.read().await;
    if !reg.contains_key(format) {
        let mut valid: Vec<_> = reg.keys().cloned().collect();
        valid.sort();
        return Err(AppError::Validation(format!(
            "checker_format must be one of: {}",
            valid.join(", ")
        )));
    }
    Ok(())
}

pub async fn validate_problem_type(
    problem_type: &str,
    registry: &EvaluatorRegistry,
) -> Result<(), AppError> {
    let reg = registry.read().await;
    if !reg.contains_key(problem_type) {
        let mut valid: Vec<_> = reg.keys().cloned().collect();
        valid.sort();
        return Err(AppError::Validation(format!(
            "problem_type must be one of: {}",
            valid.join(", ")
        )));
    }
    Ok(())
}

/// Check that `contest_type` names a registered contest type. `field` is the
/// request field reported in the error. Every request that names a contest
/// type (problem default, contest, practice or fan-out submission) goes
/// through here.
pub async fn validate_contest_type(
    field: &str,
    contest_type: &str,
    registry: &ContestTypeRegistry,
) -> Result<(), AppError> {
    let reg = registry.read().await;
    if !reg.contains_key(contest_type) {
        let mut valid: Vec<_> = reg.keys().cloned().collect();
        valid.sort();
        return Err(AppError::Validation(format!(
            "{field} must be one of: {}",
            valid.join(", ")
        )));
    }
    Ok(())
}

pub fn validate_create_problem(req: &CreateProblemRequest) -> Result<(), AppError> {
    validate_title(&req.title)?;
    if req.content.trim().is_empty() || req.content.len() > 1_000_000 {
        return Err(AppError::Validation(
            "Content must be non-empty and at most 1MB".into(),
        ));
    }
    if !(1..=30000).contains(&req.time_limit) {
        return Err(AppError::Validation("Time limit must be 1-30000 ms".into()));
    }
    if !(1..=1_048_576).contains(&req.memory_limit) {
        return Err(AppError::Validation(
            "Memory limit must be 1-1048576 KB".into(),
        ));
    }
    Ok(())
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct UploadCheckerSourceRequest {
    pub files: Vec<CheckerSourceFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CheckerSourceFile {
    #[schema(example = "checker.cpp")]
    pub filename: String,
    #[schema(example = "#include \"testlib.h\"\nint main() { ... }")]
    pub content: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct CheckerSourceResponse {
    pub files: Option<Vec<CheckerSourceFile>>,
}

/// Parse an uploaded checker source into the files that get stored, with
/// filenames normalised by `validate_flat_filename` (and duplicates detected
/// on the normalised names), so what is stored is exactly what was validated.
pub fn parse_checker_source(
    req: UploadCheckerSourceRequest,
) -> Result<Vec<CheckerSourceFile>, AppError> {
    if req.files.is_empty() {
        return Err(AppError::Validation("At least one file is required".into()));
    }
    if req.files.len() > 20 {
        return Err(AppError::Validation("Maximum 20 files allowed".into()));
    }
    let mut files: Vec<CheckerSourceFile> = Vec::with_capacity(req.files.len());
    for file in req.files {
        let filename = validate_flat_filename(&file.filename)
            .map_err(|e| AppError::Validation(e.message().into()))?;
        if files.iter().any(|seen| seen.filename == filename) {
            return Err(AppError::Validation(format!(
                "Duplicate filename: {filename}"
            )));
        }
        if file.content.len() > 1_048_576 {
            return Err(AppError::Validation(format!(
                "File '{filename}' exceeds 1 MB limit"
            )));
        }
        files.push(CheckerSourceFile {
            filename: filename.to_owned(),
            content: file.content,
        });
    }
    Ok(files)
}

pub fn validate_update_problem(req: &UpdateProblemRequest) -> Result<(), AppError> {
    if let Some(ref title) = req.title {
        validate_title(title)?;
    }
    if let Some(ref content) = req.content
        && (content.trim().is_empty() || content.len() > 1_000_000)
    {
        return Err(AppError::Validation(
            "Content must be non-empty and at most 1MB".into(),
        ));
    }
    if let Some(tl) = req.time_limit
        && !(1..=30000).contains(&tl)
    {
        return Err(AppError::Validation("Time limit must be 1-30000 ms".into()));
    }
    if let Some(ml) = req.memory_limit
        && !(1..=1_048_576).contains(&ml)
    {
        return Err(AppError::Validation(
            "Memory limit must be 1-1048576 KB".into(),
        ));
    }

    Ok(())
}

/// Parse a submission format into the normalised form that gets stored.
///
/// Language ids and filenames are trimmed (the same normalisation
/// `validate_submission_contract` applies to submitted values) and the result
/// is what the caller must persist: validating a trimmed copy but storing the
/// raw map made a padded filename like `" solution.cpp"` impossible to submit.
/// Ids or filenames that only differ by padding are rejected as duplicates.
pub fn parse_submission_format(
    submission_format: &HashMap<String, Vec<String>>,
    known_languages: &HashSet<String>,
) -> Result<HashMap<String, Vec<String>>, AppError> {
    let mut parsed = HashMap::with_capacity(submission_format.len());
    for (language_id, filenames) in submission_format {
        let language_id = language_id.trim();
        if language_id.is_empty() {
            return Err(AppError::Validation(
                "submission_format language ids must be non-empty".into(),
            ));
        }
        if !known_languages.is_empty() && !known_languages.contains(language_id) {
            return Err(AppError::Validation(format!(
                "submission_format contains unsupported language '{language_id}'"
            )));
        }
        if filenames.is_empty() {
            return Err(AppError::Validation(format!(
                "submission_format for '{language_id}' must include at least one filename"
            )));
        }

        let mut normalized = Vec::with_capacity(filenames.len());
        for filename in filenames {
            let filename = validate_flat_filename(filename)
                .map_err(|e| AppError::Validation(e.message().into()))?;
            if normalized.iter().any(|seen| seen == filename) {
                return Err(AppError::Validation(format!(
                    "submission_format for '{language_id}' contains duplicate filename '{filename}'"
                )));
            }
            normalized.push(filename.to_owned());
        }
        if parsed.insert(language_id.to_owned(), normalized).is_some() {
            return Err(AppError::Validation(format!(
                "submission_format contains duplicate language '{language_id}'"
            )));
        }
    }
    Ok(parsed)
}

/// Max length of a per-test-case description / sample explanation. Rendered as
/// markdown in the UI, so allow a few paragraphs of prose (was 256).
pub(crate) const MAX_TEST_CASE_DESCRIPTION_CHARS: usize = 4096;

pub fn validate_create_test_case(req: &CreateTestCaseRequest) -> Result<(), AppError> {
    if !(0..=10_000).contains(&req.score) {
        return Err(AppError::Validation("Score must be 0-10000".into()));
    }
    validate_optional_position(req.position)?;
    if let Some(ref desc) = req.description
        && desc.trim().chars().count() > MAX_TEST_CASE_DESCRIPTION_CHARS
    {
        return Err(AppError::Validation(format!(
            "Description must be at most {MAX_TEST_CASE_DESCRIPTION_CHARS} characters"
        )));
    }
    if let Some(ref label) = req.label {
        validate_label(label)?;
    }
    Ok(())
}

pub(crate) fn validate_label(label: &str) -> Result<(), AppError> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation("Label must be non-empty".into()));
    }
    if trimmed.chars().count() > 64 {
        return Err(AppError::Validation(
            "Label must be at most 64 characters".into(),
        ));
    }
    Ok(())
}

pub fn validate_reorder_test_cases(req: &ReorderTestCasesRequest) -> Result<(), AppError> {
    validate_reorder_ids(&req.test_case_ids, "test_case_id")
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct BulkDeleteTestCasesRequest {
    #[schema(example = json!([5, 7, 9]))]
    pub test_case_ids: Vec<i32>,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct BulkDeleteTestCasesResponse {
    #[schema(example = 3)]
    pub deleted: usize,
}

pub fn validate_bulk_delete_test_cases(req: &BulkDeleteTestCasesRequest) -> Result<(), AppError> {
    validate_bulk_ids(&req.test_case_ids, "test_case_ids", 1000)
}

pub fn validate_update_test_case(req: &UpdateTestCaseRequest) -> Result<(), AppError> {
    if let Some(score) = req.score
        && !(0..=10_000).contains(&score)
    {
        return Err(AppError::Validation("Score must be 0-10000".into()));
    }
    validate_optional_position(req.position)?;
    if let Some(Some(ref desc)) = req.description
        && desc.trim().chars().count() > MAX_TEST_CASE_DESCRIPTION_CHARS
    {
        return Err(AppError::Validation(format!(
            "Description must be at most {MAX_TEST_CASE_DESCRIPTION_CHARS} characters"
        )));
    }
    if let Some(ref label) = req.label {
        validate_label(label)?;
    }
    Ok(())
}

#[cfg(test)]
mod test_case_description_tests {
    use super::*;

    fn base_create(desc: Option<String>) -> CreateTestCaseRequest {
        CreateTestCaseRequest {
            input: "1".into(),
            expected_output: "1".into(),
            score: 10,
            is_sample: true,
            position: None,
            description: desc,
            label: None,
        }
    }

    #[test]
    fn accepts_multiparagraph_note_over_old_256_limit() {
        // Sample explanations are markdown prose that can exceed the old 256-char
        // cap. A ~500-char note must be accepted on both create and update.
        let desc = "paragraph ".repeat(50); // 500 chars
        assert!(validate_create_test_case(&base_create(Some(desc.clone()))).is_ok());
        assert!(
            validate_update_test_case(&UpdateTestCaseRequest {
                description: Some(Some(desc)),
                ..Default::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn rejects_description_over_cap() {
        let desc = "x".repeat(MAX_TEST_CASE_DESCRIPTION_CHARS + 1);
        assert!(validate_create_test_case(&base_create(Some(desc.clone()))).is_err());
        assert!(
            validate_update_test_case(&UpdateTestCaseRequest {
                description: Some(Some(desc)),
                ..Default::default()
            })
            .is_err()
        );
    }

    #[test]
    fn accepts_description_exactly_at_cap() {
        let desc = "x".repeat(MAX_TEST_CASE_DESCRIPTION_CHARS);
        assert!(validate_create_test_case(&base_create(Some(desc))).is_ok());
    }
}

// The checker source is not on the problem model: it is problem-scoped config
// owned by the checker plugin (`standard-checkers:checker_source`), served only
// through the `problem:edit`-gated checker-source endpoints.

#[cfg(test)]
mod checker_format_tests {
    use super::*;
    use crate::registry::CheckerStageHandlers;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    fn stage_reg(formats: &[&str]) -> CheckerStageRegistry {
        let mut map = HashMap::new();
        for f in formats {
            map.insert(
                f.to_string(),
                CheckerStageHandlers {
                    plugin_id: "standard-checkers".into(),
                    resolve_fn: "resolve_standard_checker".into(),
                    interpret_fn: "interpret_standard_checker_result".into(),
                },
            );
        }
        Arc::new(RwLock::new(map))
    }

    #[tokio::test]
    async fn validate_checker_format_accepts_stage_registered_formats() {
        // The fused stage registry is the source of truth - incl. `none`.
        let reg = stage_reg(&["exact", "tokens", "testlib", "none"]);
        for fmt in ["exact", "tokens", "testlib", "none"] {
            assert!(
                validate_checker_format(fmt, &reg).await.is_ok(),
                "{fmt} should validate"
            );
        }
    }

    #[tokio::test]
    async fn validate_checker_format_rejects_unknown_and_lists_valid() {
        let reg = stage_reg(&["exact", "none"]);
        let err = validate_checker_format("bogus", &reg).await.unwrap_err();
        match err {
            AppError::Validation(m) => {
                assert!(m.contains("exact"), "lists exact: {m}");
                assert!(m.contains("none"), "lists none: {m}");
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parsed_submission_format_is_the_normalised_value() {
        let raw = HashMap::from([(" cpp ".to_string(), vec![" solution.cpp".to_string()])]);
        let parsed = parse_submission_format(&raw, &HashSet::new()).unwrap();
        assert_eq!(
            parsed,
            HashMap::from([("cpp".to_string(), vec!["solution.cpp".to_string()])])
        );
    }

    #[test]
    fn submission_format_padding_cannot_hide_duplicates() {
        let raw = HashMap::from([(
            "cpp".to_string(),
            vec!["a.cpp".to_string(), " a.cpp".to_string()],
        )]);
        assert!(parse_submission_format(&raw, &HashSet::new()).is_err());
        let raw = HashMap::from([
            ("cpp".to_string(), vec!["a.cpp".to_string()]),
            ("cpp ".to_string(), vec!["b.cpp".to_string()]),
        ]);
        assert!(parse_submission_format(&raw, &HashSet::new()).is_err());
    }

    #[test]
    fn parsed_checker_source_has_normalised_filenames() {
        let file = |name: &str| CheckerSourceFile {
            filename: name.into(),
            content: "x".into(),
        };
        let files = parse_checker_source(UploadCheckerSourceRequest {
            files: vec![file(" checker.cpp ")],
        })
        .unwrap();
        assert_eq!(files[0].filename, "checker.cpp");
        assert!(
            parse_checker_source(UploadCheckerSourceRequest {
                files: vec![file("checker.cpp"), file(" checker.cpp")],
            })
            .is_err()
        );
    }
}
