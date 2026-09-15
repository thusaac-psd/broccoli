//! `Visible<T>`: the kernel's capability token.
//!
//! An entity the kernel has cleared for one subject and action. The
//! constructor is `pub(super)` — private to the `visibility` module — so a
//! handler cannot mint one directly; the only public way to obtain a
//! `Visible<T>` is through `VisibilityKernel::fetch_visible` /
//! `fetch_visible_batch`. Combined with `crate::entity` being closed to
//! handler modules (Task 13), this makes "a read path that skips the
//! kernel" a compile error rather than a production finding.
//!
//! A DTO must reach a response body ONLY through [`Visible::into_masked_json`],
//! which applies the `FieldMask` when the decision is `Redact` — a handler
//! that filters a decision vector by hand can apply `Deny` but silently
//! forget `Redact`; going through this type instead makes that impossible.
use super::Decision;
use crate::error::AppError;

pub struct Visible<T> {
    inner: T,
    decision: Decision,
}

impl<T: serde::Serialize> Visible<T> {
    /// Only reachable from within `visibility` (this module and `mod.rs`) —
    /// see the module docs for why that is the entire point of this type.
    pub(super) fn new(inner: T, decision: Decision) -> Self {
        Self { inner, decision }
    }

    /// Borrow the entity for non-serializing internal use within the
    /// `visibility` module (e.g. re-deriving another `Resource` from it to
    /// ask the kernel a follow-up question). This bypasses masking
    /// entirely, so nothing returned from here may be serialized into a
    /// response — [`Self::into_masked_json`] is the only sanctioned path to
    /// a wire body.
    ///
    /// Deliberately `pub(super)`, not `pub`: a plain `pub` getter on a
    /// `pub`-exported type would let any handler call
    /// `serde_json::to_value(visible.as_inner())` and ship the entity
    /// straight past the kernel's `FieldMask`, silently discarding a
    /// `Redact` decision. `pub(super)` makes `handlers::*` unable to reach
    /// this at all, which is what actually closes that hole (a `pub(crate)`
    /// getter would not — `handlers` lives in the same crate). If a later
    /// task needs a field off a `Visible<T>` from outside `visibility`, add
    /// a narrow, purpose-specific accessor for that field instead of
    /// widening this one back to `pub`/`pub(crate)`.
    ///
    /// `#[allow(dead_code)]`: Tasks 9-12 (not yet written) are the intended
    /// production callers; today the only call site is this module's own
    /// `#[cfg(test)]` tests, which the lib build (unlike the test build)
    /// does not compile, so `dead_code` fires without the allow. Remove the
    /// allow once a non-test caller inside `visibility` exists.
    #[allow(dead_code)]
    pub(super) fn as_inner(&self) -> &T {
        &self.inner
    }

    /// Serialize the entity, then apply the `FieldMask` if the decision was
    /// `Redact`. This is the only way to turn a `Visible<T>` into JSON, so a
    /// `Redact` decision can never be skipped at the serialization step.
    pub fn into_masked_json(self) -> Result<serde_json::Value, AppError> {
        let mut value = serde_json::to_value(&self.inner)
            .map_err(|e| AppError::Internal(format!("visibility serialize: {e}")))?;
        if let Decision::Redact(mask) = &self.decision {
            super::apply_mask(&mut value, mask);
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use serde::Serialize;
    use serde_json::json;

    use super::*;
    use crate::visibility::FieldMask;

    #[derive(Debug, Clone, Serialize)]
    struct SampleDto {
        verdict: Option<String>,
        score: Option<i32>,
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
