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
        "parked_by",
    ]);
}

/// A list row without its outcome, like [`SubmissionResponse`]: who
/// submitted (by name too) what, where and when.
impl Governed for SubmissionListItem {
    const HOST_DECIDED: Option<&'static [&'static str]> = Some(&[
        "id",
        "language",
        "user_id",
        "username",
        "display_name",
        "problem_id",
        "problem_title",
        "contest_id",
        "contest_type",
        "judge_epoch",
        "target_worker_id",
        "created_at",
        "parked_by",
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::submission_feedback::Feedback;
    use crate::visibility::{Decision, FieldMask, Visible};
    use broccoli_api_types::visibility::PluginOutage;
    use serde::Serialize;
    use serde::de::DeserializeOwned;
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

    /// Every path in `value`, objects and leaves, with `*` for each array's
    /// elements, as a plugin's mask names them.
    fn paths(value: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
        let join = |key: &str| {
            if prefix.is_empty() {
                key.to_owned()
            } else {
                format!("{prefix}.{key}")
            }
        };
        match value {
            serde_json::Value::Object(fields) => {
                for (key, field) in fields {
                    out.push(join(key));
                    paths(field, &join(key), out);
                }
            }
            serde_json::Value::Array(items) => {
                if let Some(item) = items.first() {
                    paths(item, &join("*"), out);
                }
            }
            _ => {}
        }
    }

    /// What a client receives for `sample` under `decision` and `feedback`
    /// parses as `T`, the API type every client reads it with.
    fn served_parses<T>(
        sample: &T,
        decision: Decision,
        feedback: Option<Feedback>,
    ) -> serde_json::Value
    where
        T: Governed + Serialize + DeserializeOwned + Clone,
    {
        let value = Visible::new(sample.clone(), decision.clone())
            .with_feedback(feedback.clone())
            .into_masked_json()
            .unwrap();
        if let Err(e) = serde_json::from_value::<T>(value.clone()) {
            panic!(
                "{} under {decision:?} / {feedback:?} does not parse: {e}\n{value:#}",
                std::any::type_name::<T>()
            );
        }
        value
    }

    /// Whatever a plugin masks and whatever the host withholds, a client
    /// parses the answer: each path of a full sample masked alone, each
    /// top-level field a plugin governs blanked (`null`) by its mask, and the
    /// sample withheld for each reason.
    fn assert_every_answer_parses<T>(sample: serde_json::Value)
    where
        T: Governed + Serialize + DeserializeOwned + Clone,
    {
        let sample: T = serde_json::from_value(sample).expect("a full sample of the shape");
        let full = serde_json::to_value(&sample).unwrap();
        let mut all = Vec::new();
        paths(&full, "", &mut all);
        for path in &all {
            let mask = FieldMask::new([path.clone()]);
            let value = served_parses(&sample, Decision::Redact(mask), None);
            let top = !path.contains('.');
            if top && !T::host_decides(path) && !full[path.as_str()].is_array() {
                assert!(
                    value[path.as_str()].is_null(),
                    "{} cannot leave out `{path}`, which a plugin governs",
                    std::any::type_name::<T>()
                );
            }
        }
        let outage = Decision::Withheld {
            outage: PluginOutage::Unavailable,
            mask: FieldMask::default(),
        };
        served_parses(&sample, outage, None);
        served_parses(&sample, Decision::Allow, Some(Feedback::Withheld));
    }

    #[test]
    fn every_answer_a_mask_or_withholding_leaves_parses_as_its_api_type() {
        let result = serde_json::json!({
            "judgement_id": 1, "verdict": "Accepted", "score": 100.0, "time_used": 5,
            "memory_used": 256, "compile_output": "ok", "error_code": "E",
            "error_message": "m", "judged_at": "2026-01-01T00:00:00Z",
            "test_case_results": [{
                "id": 1, "verdict": "Accepted", "score": 10.0, "time_used": 5,
                "memory_used": 256, "test_case_id": 3, "input": "1", "expected_output": "1",
                "stdout": "1", "stderr": "", "checker_output": "ok"
            }],
            "test_case_pagination": {"page": 1, "per_page": 20, "total": 1, "total_pages": 1}
        });
        let parked_by = serde_json::json!({"plugin_id": "p", "provides": "contest_type"});
        assert_every_answer_parses::<SubmissionResponse>(serde_json::json!({
            "id": 1, "files": [{"filename": "a.cpp", "content": "int main(){}"}],
            "language": "cpp", "status": "Judged", "user_id": 2, "username": "alice",
            "problem_id": 3, "problem_title": "A", "contest_id": 4, "contest_type": "icpc",
            "judge_epoch": 1, "target_worker_id": "w", "created_at": "2026-01-01T00:00:00Z",
            "result": result, "withheld": null, "parked_by": parked_by,
        }));
        assert_every_answer_parses::<SubmissionListItem>(serde_json::json!({
            "id": 1, "language": "cpp", "status": "Judged", "verdict": "Accepted",
            "user_id": 2, "username": "alice", "display_name": "Alice", "problem_id": 3,
            "problem_title": "A", "contest_id": 4, "contest_type": "icpc", "judge_epoch": 1,
            "target_worker_id": "w", "created_at": "2026-01-01T00:00:00Z", "score": 100.0,
            "time_used": 5, "memory_used": 256, "withheld": null, "parked_by": parked_by,
        }));
        assert_every_answer_parses::<ContestProblemResponse>(serde_json::json!({
            "contest_id": 4, "problem_id": 3, "label": "A", "position": 0,
            "problem_title": "A", "withheld": null,
        }));
    }

    #[test]
    fn every_host_decided_field_is_a_field_of_its_shape() {
        assert_declared::<ContestProblemResponse>();
        assert_declared::<SubmissionResponse>();
        assert_declared::<SubmissionListItem>();
    }
}
