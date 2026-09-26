use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use axum::Json;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum_typed_multipart::BaseMultipart;
use broccoli_server_sdk::permissions as perm;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `upload_test_cases` requires perm::PROBLEM_EDIT
// at handler entry, pinned by `contestant_cannot_upload_test_cases`
// (tests/integration/problem.rs). This is a write path only - it never
// returns test case content to a viewer.
use crate::entity::test_case;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::FreshAuthUser;
use crate::extractors::path::AppPath;
use crate::models::problem::*;
use crate::state::AppState;
use crate::upload_limits::{
    BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_BYTES, BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_MIB,
    LARGE_UPLOAD_LIMIT_BYTES, LARGE_UPLOAD_LIMIT_MIB,
};
use crate::utils::filename::{is_sample_directory, split_dir_filename};
use crate::utils::test_case_body::prepare_test_case_body;
use crate::utils::text::sanitize_db_text;

use super::find_problem_for_update;
use super::test_cases::{next_test_case_position, tc_to_list_item};

#[utoipa::path(
    post,
    path = "/upload",
    tag = "Test Cases",
    operation_id = "uploadTestCases",
    summary = "Upload test cases from a ZIP file",
    description = "Bulk-creates test cases from a ZIP archive. Customizable file matching formats using `*` wildcard. Requires `problem:edit` permission. Files under `sample/` are marked as samples. Decompression limits: 1 GB per file, 4 GB total. Body limit: 1 GB.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body(content_type = "multipart/form-data", content = UploadTestCasesRequest),
    responses(
        (status = 201, description = "Test cases uploaded", body = UploadTestCasesResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Duplicate label in problem (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(state, auth_user, data), fields(problem_id))]
pub async fn upload_test_cases(
    auth_user: FreshAuthUser,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    BaseMultipart { data, .. }: BaseMultipart<UploadTestCasesRequest, AppError>,
) -> Result<impl IntoResponse, AppError> {
    auth_user.require_permission(perm::PROBLEM_EDIT)?;

    if data.input_format.matches('*').count() != 1 || data.output_format.matches('*').count() != 1 {
        return Err(AppError::Validation(
            "Formats must contain exactly one '*' wildcard".into(),
        ));
    }

    let entries = parse_zip_test_cases(&data.file, &data.input_format, &data.output_format)?;
    if entries.is_empty() {
        return Err(AppError::Validation(
            "ZIP contains no valid input/output file pairs matching the specified formats".into(),
        ));
    }
    let auto_scores = default_uploaded_scores(&entries);

    let txn = state.db.begin().await?;
    find_problem_for_update(&txn, problem_id).await?;

    let is_replace = matches!(data.strategy, UploadTestCasesMergeStrategy::Replace);
    if is_replace {
        test_case::Entity::delete_many()
            .filter(test_case::Column::ProblemId.eq(problem_id))
            .exec(&txn)
            .await?;
    }

    let mut start_pos = if is_replace {
        0
    } else {
        next_test_case_position(&txn, problem_id).await?
    };

    let mut existing_cases: std::collections::HashMap<String, test_case::Model> =
        test_case::Entity::find()
            .filter(test_case::Column::ProblemId.eq(problem_id))
            .all(&txn)
            .await?
            .into_iter()
            .map(|m| (m.label.clone(), m))
            .collect();

    let mut affected = Vec::with_capacity(entries.len());
    let mut created_count = 0;
    let mut updated_count = 0;

    let now = chrono::Utc::now();

    for entry in entries {
        let score = auto_score_for_uploaded_entry(&entry, &auto_scores);
        let input_body = prepare_test_case_body(entry.input, state.blob_store.clone()).await?;
        let output_body =
            prepare_test_case_body(entry.expected_output, state.blob_store.clone()).await?;
        if let Some(existing) = existing_cases.remove(&entry.label) {
            match data.strategy {
                UploadTestCasesMergeStrategy::Abort => {
                    return Err(AppError::Conflict(format!(
                        "Test case with label '{}' already exists",
                        entry.label
                    )));
                }
                UploadTestCasesMergeStrategy::Skip => continue,
                UploadTestCasesMergeStrategy::Overwrite => {
                    let mut active: test_case::ActiveModel = existing.into();
                    active.input = Set(input_body.inline_text);
                    active.expected_output = Set(output_body.inline_text);
                    active.input_blob_hash = Set(input_body.blob_hash);
                    active.expected_output_blob_hash = Set(output_body.blob_hash);
                    active.input_size = Set(Some(input_body.size));
                    active.expected_output_size = Set(Some(output_body.size));
                    active.input_preview = Set(Some(input_body.preview));
                    active.expected_output_preview = Set(Some(output_body.preview));
                    active.label = Set(sanitize_db_text(entry.label));
                    active.score = Set(score);
                    active.is_sample = Set(entry.is_sample);
                    let model = active.update(&txn).await?;
                    affected.push(model);
                    updated_count += 1;
                    continue;
                }
                UploadTestCasesMergeStrategy::Replace => {
                    unreachable!();
                }
            }
        }

        let new_tc = test_case::ActiveModel {
            input: Set(input_body.inline_text),
            expected_output: Set(output_body.inline_text),
            input_blob_hash: Set(input_body.blob_hash),
            expected_output_blob_hash: Set(output_body.blob_hash),
            input_size: Set(Some(input_body.size)),
            expected_output_size: Set(Some(output_body.size)),
            input_preview: Set(Some(input_body.preview)),
            expected_output_preview: Set(Some(output_body.preview)),
            score: Set(score),
            label: Set(sanitize_db_text(entry.label)),
            description: Set(None),
            is_sample: Set(entry.is_sample),
            position: Set(start_pos),
            problem_id: Set(problem_id),
            created_at: Set(now),
            ..Default::default()
        };
        let model = new_tc.insert(&txn).await?;
        affected.push(model);
        created_count += 1;
        start_pos = start_pos
            .checked_add(1)
            .ok_or_else(|| AppError::Validation("Position overflow".into()))?;
    }

    txn.commit().await?;

    let test_cases: Vec<TestCaseListItem> = affected.into_iter().map(tc_to_list_item).collect();

    Ok((
        StatusCode::CREATED,
        Json(UploadTestCasesResponse {
            created: created_count,
            updated: updated_count,
            test_cases,
        }),
    ))
}

pub fn upload_body_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(LARGE_UPLOAD_LIMIT_BYTES)
}

struct ZipTestEntry {
    label: String,
    input: String,
    expected_output: String,
    is_sample: bool,
    sort_key: (u8, String),
}

fn default_uploaded_scores(entries: &[ZipTestEntry]) -> HashMap<String, i32> {
    let non_sample_labels: Vec<&str> = entries
        .iter()
        .filter(|entry| !entry.is_sample)
        .map(|entry| entry.label.as_str())
        .collect();
    let count = non_sample_labels.len();
    if count == 0 {
        return HashMap::new();
    }

    let mut scores = HashMap::with_capacity(count);
    if count > 100 {
        for label in non_sample_labels {
            scores.insert(label.to_string(), 1);
        }
        return scores;
    }

    let base = (100 / count) as i32;
    let remainder = 100 % count;
    for (index, label) in non_sample_labels.into_iter().enumerate() {
        let score = base + i32::from(index < remainder);
        scores.insert(label.to_string(), score);
    }
    scores
}

fn auto_score_for_uploaded_entry(entry: &ZipTestEntry, scores: &HashMap<String, i32>) -> i32 {
    if entry.is_sample {
        0
    } else {
        scores.get(&entry.label).copied().unwrap_or(0)
    }
}

const MAX_DECOMPRESSED_FILE_SIZE: u64 = LARGE_UPLOAD_LIMIT_BYTES as u64;

const MAX_TOTAL_DECOMPRESSED_SIZE: u64 = BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_BYTES;

fn extract_label<'a>(filename: &'a str, format: &str) -> Option<&'a str> {
    let (prefix, suffix) = format.split_once('*')?;

    if filename.starts_with(prefix)
        && filename.ends_with(suffix)
        && filename.len() >= prefix.len() + suffix.len()
    {
        Some(&filename[prefix.len()..filename.len() - suffix.len()])
    } else {
        None
    }
}

fn parse_zip_test_cases(
    data: &[u8],
    input_format: &str,
    output_format: &str,
) -> Result<Vec<ZipTestEntry>, AppError> {
    let cursor = std::io::Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| AppError::Validation(format!("Invalid ZIP archive: {e}")))?;

    let mut in_files: BTreeMap<String, (String, bool)> = BTreeMap::new();
    let mut ans_files: BTreeMap<String, String> = BTreeMap::new();
    let mut total_decompressed: u64 = 0;

    for i in 0..archive.len() {
        let file = archive
            .by_index(i)
            .map_err(|e| AppError::Validation(format!("ZIP read error: {e}")))?;

        if file.is_dir() {
            continue;
        }

        let name = match file.enclosed_name() {
            Some(path) => path.to_string_lossy().to_string(),
            None => continue,
        };

        let (dir, filename) = split_dir_filename(&name);

        if filename.starts_with('.') {
            continue;
        }

        let is_sample = is_sample_directory(dir);
        let label_as_input = extract_label(filename, input_format);
        let label_as_output = extract_label(filename, output_format);

        if label_as_input.is_none() && label_as_output.is_none() {
            continue;
        }

        let mut buf = Vec::new();
        file.take(MAX_DECOMPRESSED_FILE_SIZE + 1)
            .read_to_end(&mut buf)
            .map_err(|e| AppError::Validation(format!("Failed to read '{name}': {e}")))?;

        if buf.len() as u64 > MAX_DECOMPRESSED_FILE_SIZE {
            return Err(AppError::Validation(format!(
                "File '{name}' exceeds maximum decompressed size of {LARGE_UPLOAD_LIMIT_MIB}MB"
            )));
        }

        total_decompressed += buf.len() as u64;
        if total_decompressed > MAX_TOTAL_DECOMPRESSED_SIZE {
            return Err(AppError::Validation(format!(
                "Total decompressed ZIP content exceeds {BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_MIB}MB limit"
            )));
        }

        let content = String::from_utf8(buf)
            .map_err(|_| AppError::Validation(format!("File '{name}' is not valid UTF-8")))?;
        let content = sanitize_db_text(content);

        if let Some(label) = label_as_input {
            let key = sanitize_db_text(label);
            validate_label(&key)?;
            if in_files.contains_key(&key) {
                return Err(AppError::Validation(format!(
                    "Duplicate input file for test case label '{key}'"
                )));
            }
            in_files.insert(key, (content, is_sample));
            continue;
        }

        if let Some(label) = label_as_output {
            let key = sanitize_db_text(label);
            validate_label(&key)?;
            if ans_files.contains_key(&key) {
                return Err(AppError::Validation(format!(
                    "Duplicate output file for test case label '{key}'"
                )));
            }
            ans_files.insert(key, content);
        }
    }

    let mut unmatched_in: Vec<String> = Vec::new();
    let mut entries: Vec<ZipTestEntry> = Vec::new();

    for (key, (input, is_sample)) in in_files {
        if let Some(output) = ans_files.remove(&key) {
            let sort_priority = if is_sample { 0u8 } else { 1u8 };
            let sort_key = (sort_priority, key.clone());
            entries.push(ZipTestEntry {
                label: key,
                input,
                expected_output: output,
                is_sample,
                sort_key,
            });
        } else {
            unmatched_in.push(key);
        }
    }

    let unmatched_ans: Vec<String> = ans_files.keys().cloned().collect();

    if !unmatched_in.is_empty() || !unmatched_ans.is_empty() {
        let mut parts = Vec::new();
        if !unmatched_in.is_empty() {
            parts.push(format!(
                "Input files without matching output: {}",
                unmatched_in.join(", ")
            ));
        }
        if !unmatched_ans.is_empty() {
            parts.push(format!(
                "Output files without matching input: {}",
                unmatched_ans.join(", ")
            ));
        }
        return Err(AppError::Validation(parts.join("; ")));
    }

    entries.sort_by(|a, b| a.sort_key.cmp(&b.sort_key));

    Ok(entries)
}
