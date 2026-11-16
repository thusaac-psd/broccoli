//! What of each response shape the host decides alone ([`Governed`]): the
//! fields still shown while a plugin that governs the resource withholds the
//! rest. Every shape that reaches a response body through `Visible` is
//! listed here, so the whole policy reads in one place.

use broccoli_api_types::attachment::AttachmentResponse;
use broccoli_api_types::clarification::ClarificationResponse;
use broccoli_api_types::contest::ContestProblemResponse;
use broccoli_api_types::problem::{ProblemResponse, ProblemSamplesResponse};
use broccoli_api_types::submission::{SubmissionListItem, SubmissionResponse};

use super::Governed;

/// A contest's problem row: its place in the contest, which the contest
/// owns. What the problem holds is the plugin's to show.
impl Governed for ContestProblemResponse {
    const HOST_DECIDED: Option<&'static [&'static str]> =
        Some(&["contest_id", "problem_id", "label", "position"]);
}

/// Whether a problem may be read at all is the plugin's to decide, so none
/// of its statement, samples or attachments is shown without it.
impl Governed for ProblemResponse {
    const HOST_DECIDED: Option<&'static [&'static str]> = None;
}

impl Governed for ProblemSamplesResponse {
    const HOST_DECIDED: Option<&'static [&'static str]> = None;
}

impl Governed for AttachmentResponse {
    const HOST_DECIDED: Option<&'static [&'static str]> = None;
}

/// A clarification is a host record (`Resource::degrades_in_outage`), so
/// it is never withheld.
impl Governed for ClarificationResponse {
    const HOST_DECIDED: Option<&'static [&'static str]> = None;
}

/// A submission without its outcome: who submitted what, where and when.
impl Governed for SubmissionResponse {
    const HOST_DECIDED: Option<&'static [&'static str]> = Some(&[
        "id",
        "files",
        "language",
        "user_id",
        "username",
        "problem_id",
        "problem_title",
        "contest_id",
        "contest_type",
        "judge_epoch",
        "target_worker_id",
        "created_at",
    ]);
}

impl Governed for SubmissionListItem {
    const HOST_DECIDED: Option<&'static [&'static str]> = Some(&[
        "id",
        "language",
        "user_id",
        "username",
        "problem_id",
        "problem_title",
        "contest_id",
        "contest_type",
        "judge_epoch",
        "target_worker_id",
        "created_at",
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use utoipa::PartialSchema;

    /// The properties of `T`'s OpenAPI schema.
    fn properties<T: PartialSchema>() -> Vec<String> {
        let schema = serde_json::to_value(T::schema()).unwrap();
        schema["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("no properties in {schema}"))
            .keys()
            .cloned()
            .collect()
    }

    /// A host-decided field that names no field of its shape shows nothing,
    /// and a shape that can be withheld must be able to say so.
    fn assert_declared<T: Governed + PartialSchema>() {
        let fields = properties::<T>();
        for name in T::HOST_DECIDED.into_iter().flatten() {
            assert!(
                fields.iter().any(|f| f == name),
                "{name} is not in {fields:?}"
            );
        }
        if T::HOST_DECIDED.is_some() {
            assert!(
                fields.iter().any(|f| f == "withheld"),
                "no `withheld` in {fields:?}"
            );
        }
    }

    #[test]
    fn every_host_decided_field_is_a_field_of_its_shape() {
        assert_declared::<ContestProblemResponse>();
        assert_declared::<SubmissionResponse>();
        assert_declared::<SubmissionListItem>();
    }
}
