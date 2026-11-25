use std::str::FromStr;

use axum_typed_multipart::{TryFromField, TryFromMultipart};
use chrono::{DateTime, Utc};
use sea_orm::DerivePartialModel;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

use crate::error::AppError;
use crate::utils::filename::validate_flat_filename;

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
    #[schema(example = "my-contest-type")]
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
    #[schema(example = "lines")]
    pub checker_format: String,
    #[schema(example = "my-contest-type")]
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
    /// Streamed to a temporary file, never held in memory.
    #[form_data(limit = "unlimited")]
    #[schema(value_type = String, format = Binary)]
    pub file: axum_typed_multipart::FieldData<tempfile::NamedTempFile>,
    #[schema(example = "input_*.txt")]
    pub input_format: String,
    #[schema(example = "output_*.txt")]
    pub output_format: String,
    #[schema(example = "abort")]
    pub strategy: UploadTestCasesMergeStrategy,
}

/// The query of applying a judgement.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ApplyJudgementQuery {
    /// Apply it even if it was judged against inputs the problem has changed
    /// since (test data, limits, checker or other files), which is otherwise
    /// refused with `JUDGED_ON_OTHER_INPUTS`; the judgement is then marked
    /// `judged_on_earlier_inputs`.
    #[serde(default)]
    pub force: bool,
}

/// The query of a change to a problem's test data.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TestDataWriteQuery {
    /// Make the change even if it leaves stored configs of the problem
    /// refused by their plugins, which it otherwise refuses with
    /// `CONFIG_WOULD_BREAK`; the problem's config issues then list them.
    #[serde(default)]
    pub force: bool,
}

impl TestDataWriteQuery {
    pub(crate) fn on_config_break(&self) -> crate::services::test_data::OnConfigBreak {
        crate::services::test_data::OnConfigBreak::forced(self.force)
    }
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

/// The problem's response, with what keeps its default contest type from
/// being served (`crate::plugin_lifecycle::contest_type_outage`), what keeps
/// its practice work from running
/// (`crate::plugin_lifecycle::PluginLifecycle::closed_by`), and no samples
/// (the caller loads them when it shows them).
pub async fn problem_response(
    state: &crate::state::AppState,
    m: crate::entity::problem::Model,
) -> ProblemResponse {
    let plugin_outage =
        crate::plugin_lifecycle::contest_type_outage(state, &m.default_contest_type).await;
    let closure = submit_closure(state, &m.default_contest_type, &m);
    let submission_format: Option<std::collections::HashMap<String, Vec<String>>> = m
        .submission_format
        .and_then(|v| serde_json::from_value(v).ok());
    ProblemResponse {
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
        plugin_outage,
        closed_by: closure.closed_by,
        closed_languages: closure.closed_languages,
    }
}

/// What closes new work on problem `m` in `contest_type`
/// (`PluginLifecycle::problem_closure`): its languages are those its
/// `submission_format` names, when it names any.
pub fn submit_closure(
    state: &crate::state::AppState,
    contest_type: &str,
    m: &crate::entity::problem::Model,
) -> crate::plugin_lifecycle::ProblemClosure {
    let format = m.get_submission_format().filter(|f| !f.is_empty());
    let accepted: Option<std::collections::HashSet<&str>> = format
        .as_ref()
        .map(|f| f.keys().map(String::as_str).collect());
    state.plugin_lifecycle.problem_closure(
        state.plugin_lifecycle.needs_on(contest_type, m, None),
        accepted.as_ref(),
    )
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

use crate::registry::CheckerStageRegistry;

/// The checker format a problem gets when it names none: the
/// `default_checker_format` of the one loaded plugin whose manifest declares
/// one and that registers that format here. `None` when no plugin, or more
/// than one, declares a default.
pub async fn default_checker_format(
    plugins: &plugin_core::registry::PluginRegistry,
    checker_stage_registry: &CheckerStageRegistry,
) -> Option<String> {
    let declared: Vec<(String, String)> = {
        let registry = plugins.read().ok()?;
        registry
            .iter()
            .filter_map(|(id, entry)| {
                let format = entry
                    .manifest
                    .server
                    .as_ref()?
                    .default_checker_format
                    .clone()?;
                Some((id.clone(), format))
            })
            .collect()
    };
    let stages = checker_stage_registry.read().await;
    let mut provided = declared
        .into_iter()
        .filter(|(plugin_id, format)| {
            stages
                .get(format)
                .is_some_and(|handlers| &handlers.plugin_id == plugin_id)
        })
        .map(|(_, format)| format);
    let first = provided.next()?;
    provided.next().is_none().then_some(first)
}

/// Check that `value` names a `kind` key the fleet knows
/// ([`crate::state::AppState::known_keys`]); `field` is the request field
/// reported in the error. Every request that names a contest type, problem
/// type or checker format goes through here. A key whose plugin cannot run
/// now (disabled, loaded nowhere, not loaded here) is known: what needs it
/// is refused or parked by why, never as unknown.
pub async fn validate_key(
    state: &crate::state::AppState,
    kind: broccoli_api_types::visibility::Provided,
    field: &str,
    value: &str,
) -> Result<(), AppError> {
    check_known_key(field, value, &state.known_keys(kind).await)
}

/// Check that `value` is one of `known`, listing them when it is not.
pub fn check_known_key(field: &str, value: &str, known: &HashSet<String>) -> Result<(), AppError> {
    if known.contains(value) {
        return Ok(());
    }
    let mut valid: Vec<&str> = known.iter().map(String::as_str).collect();
    valid.sort_unstable();
    Err(AppError::Validation(format!(
        "{field} must be one of: {}",
        valid.join(", ")
    )))
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
// in the namespace the checker format's plugin declares `checker_source`,
// served only through the `problem:edit`-gated checker-source endpoints.

#[cfg(test)]
mod checker_format_tests {
    use super::*;

    #[test]
    fn a_known_key_validates_and_an_unknown_one_lists_the_known() {
        let known: HashSet<String> = ["exact", "tokens", "testlib", "none"]
            .map(String::from)
            .into();
        for fmt in ["exact", "tokens", "testlib", "none"] {
            assert!(
                check_known_key("checker_format", fmt, &known).is_ok(),
                "{fmt}"
            );
        }
        match check_known_key("checker_format", "bogus", &known).unwrap_err() {
            AppError::Validation(m) => {
                assert_eq!(
                    m,
                    "checker_format must be one of: exact, none, testlib, tokens"
                );
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
