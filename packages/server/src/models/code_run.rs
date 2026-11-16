use crate::error::AppError;
use crate::utils::filename::validate_flat_filename;
pub use broccoli_api_types::code_run::{
    CodeRunJudgeResult, CodeRunResponse, CodeRunResultResponse, CustomTestCaseInput, RunCodeRequest,
};

pub fn validate_run_code(req: &RunCodeRequest, max_size: usize) -> Result<(), AppError> {
    use std::collections::HashSet;

    if req.files.is_empty() {
        return Err(AppError::Validation("At least one file is required".into()));
    }

    let mut total_size = 0usize;
    let mut seen_filenames = HashSet::with_capacity(req.files.len());

    for file in &req.files {
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
            "Total submission size ({} bytes) exceeds maximum ({} bytes)",
            total_size, max_size
        )));
    }

    let language = req.language.trim();
    if language.is_empty() {
        return Err(AppError::Validation("Language is required".into()));
    }

    if req.custom_test_cases.is_empty() {
        return Err(AppError::Validation(
            "At least one custom test case is required".into(),
        ));
    }
    if req.custom_test_cases.len() > 10 {
        return Err(AppError::Validation(
            "Maximum 10 custom test cases allowed".into(),
        ));
    }
    for (i, tc) in req.custom_test_cases.iter().enumerate() {
        if tc.input.len() > 1_048_576 {
            return Err(AppError::Validation(format!(
                "Custom test case {} input exceeds 1MB limit",
                i
            )));
        }
        if let Some(ref expected) = tc.expected_output
            && expected.len() > 1_048_576
        {
            return Err(AppError::Validation(format!(
                "Custom test case {} expected_output exceeds 1MB limit",
                i
            )));
        }
    }

    Ok(())
}
