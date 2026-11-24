//! `Visible<T>`: the kernel's capability token.
//!
//! An entity the kernel has cleared for one subject and action. The
//! constructor is `pub(super)` — private to the `visibility` module — so a
//! handler cannot mint one directly; the only way to obtain a `Visible<T>`
//! is through `VisibilityKernel::fetch_visible` / `fetch_visible_batch`,
//! both of which route through a kernel decision first. That alone makes
//! "obtain an entity without a kernel decision" a compile error, enforced
//! exhaustively by the compiler — no pattern list to maintain.
//!
//! Separately, direct `crate::entity` imports in handler modules are
//! flagged by a CI-enforced static source scan
//! (`handlers_do_not_import_entities_directly`), **not** by the compiler:
//! `pub(crate)` grants visibility to the whole defining crate, and handlers
//! live in that crate, so there is no visibility modifier that walls
//! handlers off from `entity` while leaving `entity`'s ~19 other, legitimate
//! non-handler consumers untouched. That scan raises the cost of a bypass;
//! it does not make one impossible. See the design spec's "Layer ④ —
//! enforcement by construction" (amended 2026-09-16) for the full argument.
//!
//! A DTO must reach a response body ONLY through [`Visible::into_masked_json`],
//! which applies the `FieldMask` when the decision is `Redact` — a handler
//! that filters a decision vector by hand can apply `Deny` but silently
//! forget `Redact`; going through this type instead makes that impossible.
use broccoli_api_types::visibility::Withheld;
use broccoli_server_sdk::visibility::cap_submission_detail_texts;

use super::{Decision, FieldMask};
use crate::error::AppError;
use crate::services::submission_feedback::Feedback;

/// A response shape the kernel can present with its plugin-governed fields
/// withheld: while a plugin that governs the resource cannot decide
/// ([`Decision::Withheld`]), or hides the resource's outcome from the viewer
/// ([`Withheld::Hidden`]). Every shape that reaches a response body through
/// [`Visible`] declares which of its fields the host decides alone, so a new
/// field is withheld until someone decides otherwise.
pub trait Governed {
    /// The top-level fields shown while the rest is withheld; every other
    /// field is `null` (an array, empty) and `withheld` says why. `None`: the
    /// whole shape is plugin-governed, so a request for it fails with the
    /// outage instead.
    const HOST_DECIDED: Option<&'static [&'static str]>;

    /// Whether the value at `path` is one the host decides alone.
    fn host_decides(path: &str) -> bool {
        let top = path.split('.').next().unwrap_or(path);
        Self::HOST_DECIDED.is_some_and(|fields| fields.contains(&top))
    }
}

impl<T: Governed> Governed for Option<T> {
    const HOST_DECIDED: Option<&'static [&'static str]> = T::HOST_DECIDED;
}

pub struct Visible<T> {
    inner: T,
    decision: Decision,
    feedback: Option<Feedback>,
}

impl<T: serde::Serialize + Governed> Visible<T> {
    /// Only reachable from within `visibility` (this module and `mod.rs`) —
    /// see the module docs for why that is the entire point of this type.
    pub(super) fn new(inner: T, decision: Decision) -> Self {
        Self {
            inner,
            decision,
            feedback: None,
        }
    }

    pub(super) fn with_feedback(mut self, feedback: Option<Feedback>) -> Self {
        self.feedback = feedback;
        self
    }

    /// Serialize the entity, withhold what the decision or the plugin's
    /// feedback withholds, then apply the decision's `FieldMask`. This is the
    /// only way to turn a `Visible<T>` into JSON, so no part of a decision can
    /// be skipped at the serialization step.
    ///
    /// Also applies [`cap_submission_detail_texts`] here, unconditionally,
    /// after masking; plugins never see a response body to cap. The byte
    /// cap on oversized free-text fields (`compile_output`, checker
    /// output, test-case blobs) is a payload-size invariant, not an access
    /// decision, so it belongs here structurally: every DTO that reaches a
    /// response body passes through this one function, including contest
    /// types with NO registered visibility plugin at all. It is a plain
    /// no-op (returns immediately) for any `Value` that isn't shaped like a
    /// submission detail response (no top-level `result` object), so calling
    /// it unconditionally for every `Visible<T>` is safe regardless of `T`.
    pub fn into_masked_json(self) -> Result<serde_json::Value, AppError> {
        let mut value = serde_json::to_value(&self.inner)
            .map_err(|e| AppError::Internal(format!("visibility serialize: {e}")))?;
        let withheld = match (&self.decision, self.feedback) {
            (Decision::Withheld { outage, .. }, _) => Some(Withheld::PluginOutage {
                outage: outage.clone(),
            }),
            (_, Some(Feedback::Withheld)) => Some(Withheld::Hidden),
            (_, Some(Feedback::Published(result))) => {
                crate::services::submission_feedback::publish(&mut value, &result);
                None
            }
            (_, None) => None,
        };
        if let Some(reason) = withheld {
            withhold::<T>(&mut value, reason)?;
        }
        match &self.decision {
            Decision::Redact(mask) | Decision::Withheld { mask, .. } => {
                super::apply_mask(&mut value, mask)
            }
            Decision::Allow | Decision::Deny => {}
        }
        cap_submission_detail_texts(&mut value);
        Ok(value)
    }
}

/// Whether every path of `mask` blanked in `entity` leaves a value `T`
/// parses: whether a client can read the answer the mask makes. A path that
/// blanks a value the API type requires (an id, a label every client reads)
/// does not; the kernel then withholds the whole resource instead of serving
/// an answer no client can read or skipping a blank the plugin asked for.
pub(super) fn admits<T>(entity: &T, mask: &FieldMask) -> bool
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    if mask.is_empty() {
        return true;
    }
    let Ok(mut value) = serde_json::to_value(entity) else {
        return false;
    };
    super::apply_mask(&mut value, mask);
    serde_json::from_value::<T>(value).is_ok()
}

/// Blank every field of `value` but the ones `T` declares host-decided, and
/// say why in its `withheld` field.
fn withhold<T: Governed>(value: &mut serde_json::Value, reason: Withheld) -> Result<(), AppError> {
    let Some(shown) = T::HOST_DECIDED else {
        return Err(match reason {
            Withheld::PluginOutage { outage } => outage.into(),
            Withheld::Hidden => {
                AppError::Internal("a shape without host-decided fields was hidden".into())
            }
        });
    };
    let Some(fields) = value.as_object_mut() else {
        return Ok(());
    };
    for (name, field) in fields.iter_mut() {
        if !shown.contains(&name.as_str()) {
            *field = match field {
                serde_json::Value::Array(_) => serde_json::Value::Array(Vec::new()),
                _ => serde_json::Value::Null,
            };
        }
    }
    fields.insert(
        "withheld".into(),
        serde_json::to_value(reason).map_err(|e| AppError::Internal(e.to_string()))?,
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    use super::*;
    use crate::visibility::FieldMask;

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct SampleDto {
        verdict: Option<String>,
        score: Option<i32>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct FakeJudgeResult {
        compile_output: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    struct FakeSubmissionDto {
        result: FakeJudgeResult,
    }

    impl Governed for SampleDto {
        const HOST_DECIDED: Option<&'static [&'static str]> = Some(&["score"]);
    }

    impl Governed for FakeSubmissionDto {
        const HOST_DECIDED: Option<&'static [&'static str]> = None;
    }

    #[test]
    fn withheld_shows_only_the_host_decided_fields_and_says_why() {
        use broccoli_api_types::visibility::PluginOutage;
        let model = SampleDto {
            verdict: Some("AC".to_string()),
            score: Some(100),
        };
        let value = Visible::new(
            model,
            Decision::Withheld {
                outage: PluginOutage::Unavailable,
                mask: FieldMask::default(),
            },
        )
        .into_masked_json()
        .unwrap();
        assert_eq!(
            value,
            json!({
                "verdict": null,
                "score": 100,
                "withheld": {"reason": "plugin_outage", "outage": {"state": "unavailable"}},
            })
        );
    }

    #[test]
    fn a_shape_with_nothing_host_decided_fails_with_the_outage() {
        use broccoli_api_types::visibility::PluginOutage;
        let dto = FakeSubmissionDto {
            result: FakeJudgeResult {
                compile_output: None,
            },
        };
        let err = Visible::new(
            dto,
            Decision::Withheld {
                outage: PluginOutage::Disabled {
                    plugin_id: "p".into(),
                },
                mask: FieldMask::default(),
            },
        )
        .into_masked_json()
        .unwrap_err();
        assert!(
            matches!(err, AppError::PluginDisabled(ref id) if id == "p"),
            "{err:?}"
        );
    }

    /// `into_masked_json` is the only caller of `cap_submission_detail_texts`.
    /// `Decision::Allow` with no
    /// `FieldMask` in play is exactly what a contest type with NO registered
    /// visibility plugin gets: `host_decide`'s reachability answer alone,
    /// `meet`-ed with zero queriers (`query_plugins`'s zero-queriers case
    /// returns Allow, the identity element). This test proves the cap holds
    /// even here, i.e. it does not depend on any plugin cooperating.
    #[test]
    fn into_masked_json_caps_oversized_detail_text_with_no_mask_and_no_plugin() {
        let big =
            "x".repeat(broccoli_server_sdk::visibility::DETAIL_TEXT_RESPONSE_LIMIT_BYTES + 10);
        let dto = FakeSubmissionDto {
            result: FakeJudgeResult {
                compile_output: Some(big),
            },
        };
        let value = Visible::new(dto, Decision::Allow)
            .into_masked_json()
            .unwrap();
        let capped = value["result"]["compile_output"].as_str().unwrap();
        assert_eq!(
            capped.len(),
            broccoli_server_sdk::visibility::DETAIL_TEXT_RESPONSE_LIMIT_BYTES
        );
    }

    /// The cap must still apply when a `Redact` mask ALSO fires, and must not
    /// resurrect a field the mask just blanked to `null` (capping a string
    /// is a no-op on anything that isn't a string - see
    /// `cap_json_string_field` - so a masked-to-null `compile_output` stays
    /// `null`, not `""`).
    #[test]
    fn into_masked_json_caps_oversized_detail_text_alongside_a_redact_mask() {
        let big =
            "x".repeat(broccoli_server_sdk::visibility::DETAIL_TEXT_RESPONSE_LIMIT_BYTES + 10);
        let dto = FakeSubmissionDto {
            result: FakeJudgeResult {
                compile_output: Some(big),
            },
        };
        let value = Visible::new(
            dto,
            Decision::Redact(FieldMask::new(["result.compile_output".to_string()])),
        )
        .into_masked_json()
        .unwrap();
        assert!(
            value["result"]["compile_output"].is_null(),
            "masking must win over capping for a field the mask blanks: {value:?}"
        );
    }

    #[test]
    fn allow_serializes_unmasked() {
        let model = SampleDto {
            verdict: Some("AC".to_string()),
            score: Some(100),
        };
        let value = Visible::new(model, Decision::Allow)
            .into_masked_json()
            .unwrap();
        assert_eq!(value, json!({"verdict": "AC", "score": 100}));
    }

    #[test]
    fn redact_blanks_the_masked_paths() {
        let model = SampleDto {
            verdict: Some("AC".to_string()),
            score: Some(100),
        };
        let value = Visible::new(
            model,
            Decision::Redact(FieldMask::new(["verdict".to_string()])),
        )
        .into_masked_json()
        .unwrap();
        assert!(value["verdict"].is_null());
        assert_eq!(value["score"], 100);
    }

    #[test]
    fn masked_json_never_gains_a_field() {
        // Redaction is destructive-only: the masked output has exactly the
        // same key set as the unmasked one, so a mask can never introduce
        // content.
        let model = SampleDto {
            verdict: Some("AC".to_string()),
            score: Some(100),
        };
        let unmasked = Visible::new(model.clone(), Decision::Allow)
            .into_masked_json()
            .unwrap();
        let masked = Visible::new(
            model,
            Decision::Redact(FieldMask::new(["verdict".to_string()])),
        )
        .into_masked_json()
        .unwrap();
        assert_eq!(
            unmasked.as_object().unwrap().keys().collect::<Vec<_>>(),
            masked.as_object().unwrap().keys().collect::<Vec<_>>()
        );
        assert!(masked["verdict"].is_null());
    }
}
