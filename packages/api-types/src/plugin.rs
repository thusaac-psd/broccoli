//! Plugin registry listings.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct LanguageRegistryItem {
    #[schema(example = "cpp")]
    pub id: String,
    #[schema(example = "C++")]
    pub name: String,
    #[schema(example = "solution.cpp")]
    pub default_filename: String,
    #[schema(example = json!(["cpp", "cc", "cxx"]))]
    pub extensions: Vec<String>,
    #[schema(
        example = "#include <iostream>\nusing namespace std;\n\nint main() {\n    return 0;\n}\n"
    )]
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegistriesResponse {
    #[schema(example = json!(["batch", "interactive"]))]
    pub problem_types: Vec<String>,

    #[schema(example = json!(["exact", "tokens"]))]
    pub checker_formats: Vec<String>,

    #[schema(example = json!(["icpc", "ioi"]))]
    pub contest_types: Vec<String>,

    pub languages: Vec<LanguageRegistryItem>,

    pub evaluators: Vec<EvaluatorEntry>,

    pub contest_type_handlers: Vec<ContestTypeEntry>,

    pub hooks: Vec<HookEntryInfo>,
}

/// A single evaluator registration entry.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct EvaluatorEntry {
    /// Problem type this evaluator handles (e.g. "batch", "communication").
    #[schema(example = "communication")]
    pub problem_type: String,
    /// Plugin that registered this evaluator.
    #[schema(example = "communication-evaluator")]
    pub plugin_id: String,
    /// Plugin function invoked to evaluate test cases for this problem type.
    #[schema(example = "evaluate_communication")]
    pub function_name: String,
}

/// A single contest type registration entry.
///
/// Used to carry a fourth field, `filter_submission_fn`: the function a
/// contest-type plugin registered to filter/redact a submission response
/// body before it left the server. Retired entirely (not replaced by
/// anything in this struct) once the VisibilityKernel took over submission
/// visibility end to end - `Decision`/`FieldMask` plus each plugin's
/// `topic = "visibility"` query function now own that job, driven by
/// `packages/server/src/visibility/`, not by a per-contest-type hook listed
/// here. See `git show dc8112d8` ("retire filter_submission_fn plugin hook")
/// for the removal and SDD 2026-09-15-visibility-kernel Tasks 13-16 for what
/// replaced it.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContestTypeEntry {
    /// Contest type identifier (e.g. "icpc", "ioi").
    #[schema(example = "icpc")]
    pub contest_type: String,
    /// Plugin that registered this contest type.
    #[schema(example = "icpc")]
    pub plugin_id: String,
    /// Function name invoked on submission.
    #[schema(example = "on_submission")]
    pub submission_fn: String,
    /// Function name invoked for code-run / sample-run dispatch.
    #[schema(example = "on_code_run")]
    pub code_run_fn: String,
}

/// A single hook registration entry.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct HookEntryInfo {
    /// Event topic the hook is bound to (e.g. "submission.created").
    #[schema(example = "submission.created")]
    pub topic: String,
    /// Plugin that registered the hook.
    #[schema(example = "submission-limit")]
    pub plugin_id: String,
    /// Hook scope ("global", "contest", "problem", "user", ...).
    #[schema(example = "global")]
    pub scope: String,
    /// Hook execution mode ("async", "sync", ...).
    #[schema(example = "async")]
    pub mode: String,
}
