use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum_typed_multipart::BaseMultipart;
use sea_orm::*;
use tracing::instrument;

// visibility-bypass-audited: `upload_test_cases` requires perm::PROBLEM_EDIT
// at handler entry, pinned by `contestant_cannot_upload_test_cases`
// (tests/integration/problem.rs). This is a write path only - it never
// returns test case content to a viewer.
use crate::entity::test_case;
use crate::error::{AppError, ErrorBody};
use crate::extractors::auth::{Authorized, ProblemEdit};
use crate::extractors::path::AppPath;
use crate::models::problem::*;
use crate::state::AppState;
use crate::upload_limits::{
    BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_BYTES, BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_MIB,
    LARGE_UPLOAD_LIMIT_BYTES, LARGE_UPLOAD_LIMIT_MIB,
};
use crate::utils::filename::{is_sample_directory, split_dir_filename};
use crate::utils::test_case_body::{TestCaseFile, prepare_test_case_file};

use super::find_problem_for_update;
use super::test_cases::{delete_unjudged_test_cases, next_test_case_position, tc_to_list_item};

/// What an uploaded test case does when a case with its label exists.
#[derive(Clone, Copy)]
enum OnExistingLabel {
    Abort,
    Skip,
    Overwrite,
}

#[utoipa::path(
    post,
    path = "/upload",
    tag = "Test Cases",
    operation_id = "uploadTestCases",
    summary = "Upload test cases from a ZIP file",
    description = "Bulk-creates test cases from a ZIP archive. Customizable file matching formats using `*` wildcard. A file matching both formats belongs to the more specific one (more literal characters, e.g. `*.a` over `*`); equally specific overlaps are rejected. Requires `problem:edit` permission. Files under `sample/` are marked as samples. Files are bytes: one that is not UTF-8 text, holds a NUL or is 1 MiB or more is stored as a blob, byte for byte. Decompression limits: 1 GB per file, 4 GB total. Body limit: 1 GB.",
    params(("id" = i32, Path, description = "Problem ID")),
    request_body(content_type = "multipart/form-data", content = UploadTestCasesRequest),
    responses(
        (status = 201, description = "Test cases uploaded", body = UploadTestCasesResponse),
        (status = 400, description = "Validation error (VALIDATION_ERROR)", body = ErrorBody),
        (status = 413, description = "Request body over the endpoint's limit (PAYLOAD_TOO_LARGE)", body = ErrorBody),
        (status = 401, description = "Unauthorized (TOKEN_MISSING, TOKEN_INVALID)", body = ErrorBody),
        (status = 403, description = "Forbidden (PERMISSION_DENIED)", body = ErrorBody),
        (status = 404, description = "Problem not found (NOT_FOUND)", body = ErrorBody),
        (status = 409, description = "Duplicate label (abort), or replace would delete test cases that have judge results (CONFLICT)", body = ErrorBody),
    ),
    security(("jwt" = [])),
)]
#[instrument(skip(_auth, state, data), fields(problem_id))]
pub async fn upload_test_cases(
    _auth: Authorized<ProblemEdit>,
    State(state): State<AppState>,
    AppPath(problem_id): AppPath<i32>,
    BaseMultipart { data, .. }: BaseMultipart<UploadTestCasesRequest, AppError>,
) -> Result<impl IntoResponse, AppError> {
    if data.input_format.matches('*').count() != 1 || data.output_format.matches('*').count() != 1 {
        return Err(AppError::Validation(
            "Formats must contain exactly one '*' wildcard".into(),
        ));
    }

    // Unpacking is blocking file I/O and decompression: off the async
    // runtime, each entry into its own temporary file, so memory stays
    // bounded whatever the archive holds.
    let entries = {
        let archive = data.file.contents;
        let (input_format, output_format) = (data.input_format.clone(), data.output_format.clone());
        tokio::task::spawn_blocking(move || {
            parse_zip_test_cases(archive.as_file(), &input_format, &output_format)
        })
        .await
        .map_err(|e| AppError::Internal(format!("ZIP unpacking task failed: {e}")))??
    };
    if entries.is_empty() {
        return Err(AppError::Validation(
            "ZIP contains no valid input/output file pairs matching the specified formats".into(),
        ));
    }
    let auto_scores = default_uploaded_scores(&entries);

    let txn = state.db.begin().await?;
    find_problem_for_update(&txn, problem_id).await?;

    // What an uploaded case does when its label already exists. `Replace`
    // first deletes every existing case, so no label can collide.
    let on_existing_label = match data.strategy {
        UploadTestCasesMergeStrategy::Replace => {
            delete_unjudged_test_cases(&txn, test_case::Column::ProblemId.eq(problem_id)).await?;
            None
        }
        UploadTestCasesMergeStrategy::Abort => Some(OnExistingLabel::Abort),
        UploadTestCasesMergeStrategy::Skip => Some(OnExistingLabel::Skip),
        UploadTestCasesMergeStrategy::Overwrite => Some(OnExistingLabel::Overwrite),
    };

    let (mut start_pos, mut existing_cases) = if on_existing_label.is_some() {
        let existing: std::collections::HashMap<String, test_case::Model> =
            test_case::Entity::find()
                .filter(test_case::Column::ProblemId.eq(problem_id))
                .all(&txn)
                .await?
                .into_iter()
                .map(|m| (m.label.clone(), m))
                .collect();
        (next_test_case_position(&txn, problem_id).await?, existing)
    } else {
        (0, Default::default())
    };

    let mut affected = Vec::with_capacity(entries.len());
    let mut created_count = 0;
    let mut updated_count = 0;

    let now = state.clock.now();

    for entry in entries {
        let score = auto_score_for_uploaded_entry(&entry, &auto_scores);
        let input_body = prepare_test_case_file(entry.input, &*state.blob_store).await?;
        let output_body = prepare_test_case_file(entry.expected_output, &*state.blob_store).await?;
        if let Some(on_existing) = on_existing_label
            && let Some(existing) = existing_cases.remove(&entry.label)
        {
            match on_existing {
                OnExistingLabel::Abort => {
                    return Err(AppError::Conflict(format!(
                        "Test case with label '{}' already exists",
                        entry.label
                    )));
                }
                OnExistingLabel::Skip => continue,
                OnExistingLabel::Overwrite => {
                    let mut active: test_case::ActiveModel = existing.into();
                    active.input = Set(input_body.inline_text);
                    active.expected_output = Set(output_body.inline_text);
                    active.input_blob_hash = Set(input_body.blob_hash);
                    active.expected_output_blob_hash = Set(output_body.blob_hash);
                    active.input_size = Set(Some(input_body.size));
                    active.expected_output_size = Set(Some(output_body.size));
                    active.input_preview = Set(Some(input_body.preview));
                    active.expected_output_preview = Set(Some(output_body.preview));
                    active.label = Set(entry.label);
                    active.score = Set(score);
                    active.is_sample = Set(entry.is_sample);
                    let model = active.update(&txn).await?;
                    affected.push(model);
                    updated_count += 1;
                    continue;
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
            label: Set(entry.label),
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

pub fn upload_body_limit() -> usize {
    LARGE_UPLOAD_LIMIT_BYTES
}

struct ZipTestEntry {
    label: String,
    input: TestCaseFile,
    expected_output: TestCaseFile,
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

/// What a ZIP entry is, given the input and output filename formats.
#[derive(Debug, PartialEq, Eq)]
enum ZipEntryRole<'a> {
    Input(&'a str),
    Output(&'a str),
}

/// Classify `filename` as an input or output file and extract its label.
///
/// Overlapping formats are legitimate (Polygon's `*` for inputs and `*.a` for
/// answers), so a name matching both goes to the MORE SPECIFIC format, the one
/// with more literal characters around the `*`: `01.a` is the answer for `01`,
/// not an input labelled `01.a`. When both are equally specific there is no
/// principled choice, so the upload is rejected instead of guessing.
fn classify_zip_entry<'a>(
    filename: &'a str,
    input_format: &str,
    output_format: &str,
) -> Result<Option<ZipEntryRole<'a>>, AppError> {
    match (
        extract_label(filename, input_format),
        extract_label(filename, output_format),
    ) {
        (None, None) => Ok(None),
        (Some(label), None) => Ok(Some(ZipEntryRole::Input(label))),
        (None, Some(label)) => Ok(Some(ZipEntryRole::Output(label))),
        (Some(as_input), Some(as_output)) => {
            // Both formats contain exactly one `*`, so `len - 1` is the
            // number of literal characters.
            match input_format.len().cmp(&output_format.len()) {
                std::cmp::Ordering::Greater => Ok(Some(ZipEntryRole::Input(as_input))),
                std::cmp::Ordering::Less => Ok(Some(ZipEntryRole::Output(as_output))),
                std::cmp::Ordering::Equal => Err(AppError::Validation(format!(
                    "File '{filename}' matches both input format '{input_format}' and \
                     output format '{output_format}'; make one format more specific"
                ))),
            }
        }
    }
}

fn parse_zip_test_cases(
    archive: &std::fs::File,
    input_format: &str,
    output_format: &str,
) -> Result<Vec<ZipTestEntry>, AppError> {
    let mut archive = zip::ZipArchive::new(archive)
        .map_err(|e| AppError::Validation(format!("Invalid ZIP archive: {e}")))?;

    let mut in_files: BTreeMap<String, (TestCaseFile, bool)> = BTreeMap::new();
    let mut ans_files: BTreeMap<String, TestCaseFile> = BTreeMap::new();
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
        let Some(role) = classify_zip_entry(filename, input_format, output_format)? else {
            continue;
        };

        let mut body = tempfile::NamedTempFile::new()
            .map_err(|e| AppError::Internal(format!("Failed to create a temporary file: {e}")))?;
        let size = std::io::copy(&mut file.take(MAX_DECOMPRESSED_FILE_SIZE + 1), &mut body)
            .map_err(|e| AppError::Validation(format!("Failed to read '{name}': {e}")))?;

        if size > MAX_DECOMPRESSED_FILE_SIZE {
            return Err(AppError::Validation(format!(
                "File '{name}' exceeds maximum decompressed size of {LARGE_UPLOAD_LIMIT_MIB}MB"
            )));
        }

        total_decompressed += size;
        if total_decompressed > MAX_TOTAL_DECOMPRESSED_SIZE {
            return Err(AppError::Validation(format!(
                "Total decompressed ZIP content exceeds {BULK_TEST_CASE_MAX_TOTAL_DECOMPRESSED_MIB}MB limit"
            )));
        }
        let content = TestCaseFile { file: body, size };

        match role {
            ZipEntryRole::Input(label) => {
                let key = label.to_string();
                validate_label(&key)?;
                if in_files.contains_key(&key) {
                    return Err(AppError::Validation(format!(
                        "Duplicate input file for test case label '{key}'"
                    )));
                }
                in_files.insert(key, (content, is_sample));
            }
            ZipEntryRole::Output(label) => {
                let key = label.to_string();
                validate_label(&key)?;
                if ans_files.contains_key(&key) {
                    return Err(AppError::Validation(format!(
                        "Duplicate output file for test case label '{key}'"
                    )));
                }
                ans_files.insert(key, content);
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_formats_prefer_the_more_specific_pattern() {
        assert_eq!(
            classify_zip_entry("01.a", "*", "*.a").unwrap(),
            Some(ZipEntryRole::Output("01"))
        );
        assert_eq!(
            classify_zip_entry("01", "*", "*.a").unwrap(),
            Some(ZipEntryRole::Input("01"))
        );
        // Mirror image: the input format is the specific one.
        assert_eq!(
            classify_zip_entry("01.in", "*.in", "*").unwrap(),
            Some(ZipEntryRole::Input("01"))
        );
        assert_eq!(classify_zip_entry("x.txt", "*.in", "*.ans").unwrap(), None);
    }

    #[test]
    fn equally_specific_overlap_is_rejected() {
        assert!(classify_zip_entry("ab", "a*", "*b").is_err());
        assert!(classify_zip_entry("1.txt", "*.txt", "*.txt").is_err());
    }
}
