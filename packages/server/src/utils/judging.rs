use std::collections::{HashMap, HashSet};

use crate::error::AppError;
use crate::models::submission::{SubmissionFile, SubmissionFileDto};
use crate::utils::filename::validate_flat_filename;

pub fn validate_code_payload(
    files: &[SubmissionFileDto],
    language: &str,
    max_size: usize,
) -> Result<(), AppError> {
    if files.is_empty() {
        return Err(AppError::Validation("At least one file is required".into()));
    }

    if language.trim().is_empty() {
        return Err(AppError::Validation("Language is required".into()));
    }

    let mut total_size = 0usize;
    let mut seen_filenames = HashSet::with_capacity(files.len());

    for file in files {
        let filename = validate_flat_filename(&file.filename)
            .map_err(|e| AppError::Validation(e.message().into()))?;

        if !seen_filenames.insert(filename) {
            return Err(AppError::Validation(format!(
                "Duplicate filename: '{}'",
                filename
            )));
        }

        if file.content.is_empty() {
            return Err(AppError::Validation(format!(
                "File '{}' cannot be empty",
                filename
            )));
        }

        total_size = total_size.saturating_add(file.content.len());
    }

    if total_size > max_size {
        return Err(AppError::Validation(format!(
            "Total code size ({} bytes) exceeds maximum ({} bytes)",
            total_size, max_size
        )));
    }

    Ok(())
}

pub fn validate_submission_contract(
    files: &[SubmissionFileDto],
    language: &str,
    submission_format: Option<HashMap<String, Vec<String>>>,
    known_languages: &HashSet<String>,
) -> Result<(), AppError> {
    let language = language.trim();

    check_known_language(language, known_languages)?;

    let Some(submission_format) = submission_format else {
        return Ok(());
    };

    if submission_format.is_empty() {
        return Ok(());
    }

    let mut expected = submission_format.get(language).cloned().ok_or_else(|| {
        AppError::Validation(format!(
            "Language '{}' is not allowed for this problem",
            language
        ))
    })?;
    let mut actual = files
        .iter()
        .map(|file| file.filename.trim().to_string())
        .collect::<Vec<_>>();
    expected.sort();
    actual.sort();

    if actual != expected {
        return Err(AppError::Validation(format!(
            "Files for language '{}' must exactly match: {}",
            language,
            expected.join(", ")
        )));
    }

    Ok(())
}

pub fn files_to_json(files: &[SubmissionFileDto]) -> serde_json::Value {
    let submission_files: Vec<SubmissionFile> = files
        .iter()
        .map(|f| SubmissionFile {
            filename: f.filename.trim().to_string(),
            content: f.content.clone(),
        })
        .collect();
    serde_json::to_value(&submission_files).unwrap_or(serde_json::Value::Array(vec![]))
}

/// Log-safe description of stored submission files: each file's name and
/// size in bytes, never its contents. Use it wherever a span or log line would
/// otherwise carry a submission or code-run model, whose `files` hold the
/// contestant's full source.
pub fn files_log_summary(value: &serde_json::Value) -> String {
    serde_json::from_value::<Vec<SubmissionFile>>(value.clone())
        .unwrap_or_default()
        .iter()
        .map(|f| format!("{} ({} B)", f.filename, f.content.len()))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn files_from_json(value: &serde_json::Value) -> Vec<SubmissionFileDto> {
    serde_json::from_value::<Vec<SubmissionFile>>(value.clone())
        .unwrap_or_default()
        .into_iter()
        .map(SubmissionFileDto::from)
        .collect()
}

pub fn validate_run_language(
    language: &str,
    known_languages: &HashSet<String>,
) -> Result<(), AppError> {
    check_known_language(language.trim(), known_languages)
}

/// Refuse a language no plugin registered, naming the ones that are, so a
/// client can show the choice. An empty registry checks nothing.
fn check_known_language(language: &str, known_languages: &HashSet<String>) -> Result<(), AppError> {
    if known_languages.is_empty() || known_languages.contains(language) {
        return Ok(());
    }
    let mut known: Vec<&str> = known_languages.iter().map(String::as_str).collect();
    known.sort_unstable();
    Err(AppError::Validation(format!(
        "Unsupported language: {language}. Supported languages: {}",
        known.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known_languages() -> HashSet<String> {
        HashSet::from(["cpp".to_string(), "c".to_string(), "python3".to_string()])
    }

    #[test]
    fn an_unsupported_language_lists_the_supported_ones() {
        match validate_run_language("ruby", &known_languages()) {
            Err(AppError::Validation(message)) => assert_eq!(
                message,
                "Unsupported language: ruby. Supported languages: c, cpp, python3"
            ),
            other => panic!("expected a validation error, got {other:?}"),
        }
    }

    #[test]
    fn test_validate_code_payload_success() {
        let files = vec![SubmissionFileDto {
            filename: "main.cpp".into(),
            content: "int main() {}".into(),
        }];
        assert!(validate_code_payload(&files, "cpp", 1000).is_ok());
    }

    #[test]
    fn test_validate_code_payload_empty_files() {
        let files = vec![];
        assert!(validate_code_payload(&files, "cpp", 1000).is_err());
    }

    #[test]
    fn test_validate_code_payload_duplicate_filenames() {
        let files = vec![
            SubmissionFileDto {
                filename: "a.cpp".into(),
                content: "a".into(),
            },
            SubmissionFileDto {
                filename: "a.cpp".into(),
                content: "b".into(),
            },
        ];
        assert!(validate_code_payload(&files, "cpp", 1000).is_err());
    }

    #[test]
    fn test_validate_code_payload_size_limit() {
        let files = vec![SubmissionFileDto {
            filename: "large.cpp".into(),
            content: "12345".into(),
        }];
        assert!(validate_code_payload(&files, "cpp", 4).is_err());
    }

    #[test]
    fn test_validate_code_payload_empty_content() {
        let files = vec![SubmissionFileDto {
            filename: "empty.cpp".into(),
            content: "".into(),
        }];
        assert!(validate_code_payload(&files, "cpp", 1000).is_err());
    }

    #[test]
    fn test_validate_submission_contract_success() {
        let files = vec![SubmissionFileDto {
            filename: "solution.cpp".into(),
            content: "int main() {}".into(),
        }];
        let mut submission_format = HashMap::new();
        submission_format.insert("cpp".into(), vec!["solution.cpp".into()]);
        let languages = known_languages();
        assert!(
            validate_submission_contract(&files, "cpp", Some(submission_format), &languages)
                .is_ok()
        );
    }

    #[test]
    fn test_validate_submission_contract_unsupported_language() {
        let files = vec![SubmissionFileDto {
            filename: "solution.rs".into(),
            content: "fn main() {}".into(),
        }];
        let submission_format = HashMap::new();
        let languages = known_languages();
        assert!(
            validate_submission_contract(&files, "rust", Some(submission_format), &languages)
                .is_err()
        );
    }

    #[test]
    fn test_validate_submission_contract_file_mismatch() {
        let files = vec![SubmissionFileDto {
            filename: "main.cpp".into(),
            content: "int main() {}".into(),
        }];
        let mut submission_format = HashMap::new();
        submission_format.insert("cpp".into(), vec!["solution.cpp".into()]);
        let languages = known_languages();
        assert!(
            validate_submission_contract(&files, "cpp", Some(submission_format), &languages)
                .is_err()
        );
    }

    #[test]
    fn test_files_to_json_and_from_json() {
        let files = vec![
            SubmissionFileDto {
                filename: "a.cpp".into(),
                content: "code a".into(),
            },
            SubmissionFileDto {
                filename: "b.cpp".into(),
                content: "code b".into(),
            },
        ];
        let json = files_to_json(&files);
        let parsed_files = files_from_json(&json);
        assert_eq!(files, parsed_files);
    }

    #[test]
    fn test_validate_run_language_known() {
        let languages = known_languages();
        assert!(validate_run_language("cpp", &languages).is_ok());
    }

    #[test]
    fn test_validate_run_language_unknown() {
        let languages = known_languages();
        assert!(validate_run_language("brainfuck", &languages).is_err());
    }

    #[test]
    fn test_validate_run_language_empty_set_allows_any() {
        let languages = HashSet::new();
        assert!(validate_run_language("anything", &languages).is_ok());
    }

    #[test]
    fn files_log_summary_names_sizes_and_never_contents() {
        let files = serde_json::json!([
            {"filename": "main.cpp", "content": "int main() { return 42; }"},
            {"filename": "util.h", "content": ""},
        ]);
        let summary = super::files_log_summary(&files);
        assert_eq!(summary, "main.cpp (25 B), util.h (0 B)");
        assert!(!summary.contains("return 42"));
    }
}
