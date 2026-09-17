use std::collections::BTreeSet;

/// JSON paths to blank, dot-separated (e.g. `result.verdict`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldMask(BTreeSet<String>);

impl FieldMask {
    pub fn new(fields: impl IntoIterator<Item = String>) -> Self {
        Self(fields.into_iter().collect())
    }

    pub fn union(mut self, other: FieldMask) -> Self {
        self.0.extend(other.0);
        self
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Number of distinct field paths. Used by
    /// `plugin_query::query_plugins` to reapply `MAX_MASK_FIELDS` after
    /// [`Decision::meet`] has unioned masks from more than one plugin
    /// together - see that constant's doc comment.
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

/// Reachability decision for one (subject, action, resource) triple.
///
/// Ordered `Allow > Redact > Deny`. Combining decisions uses [`Decision::meet`],
/// which can only move down this order — that is what makes "plugins may narrow,
/// never widen" an algebraic property rather than a documented promise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Redact(FieldMask),
    Deny,
}

impl Decision {
    pub fn meet(self, other: Decision) -> Decision {
        match (self, other) {
            (Decision::Deny, _) | (_, Decision::Deny) => Decision::Deny,
            (Decision::Redact(a), Decision::Redact(b)) => Decision::Redact(a.union(b)),
            (Decision::Redact(m), Decision::Allow) | (Decision::Allow, Decision::Redact(m)) => {
                Decision::Redact(m)
            }
            (Decision::Allow, Decision::Allow) => Decision::Allow,
        }
    }

    pub fn is_denied(&self) -> bool {
        matches!(self, Decision::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask(fields: &[&str]) -> FieldMask {
        FieldMask::new(fields.iter().map(|s| s.to_string()))
    }

    #[test]
    fn is_denied_true_only_for_deny() {
        // Redact is deliberately NOT denied - see the handler-site doc
        // comments this test's sibling HTTP-level tests pin (get_test_case,
        // download_attachment): a hypothetical Redact decision at those
        // sites degenerates to Allow because the response is built without
        // going through `Visible<T>`/`into_masked_json`. If `is_denied`
        // ever started treating `Redact` as denied, those sites would start
        // 404ing viewers who should merely be un-redactable there.
        assert!(Decision::Deny.is_denied());
        assert!(!Decision::Allow.is_denied());
        assert!(!Decision::Redact(mask(&["x"])).is_denied());
    }

    #[test]
    fn plugin_cannot_widen_a_host_deny() {
        assert_eq!(Decision::Deny.meet(Decision::Allow), Decision::Deny);
    }

    #[test]
    fn plugin_cannot_widen_a_host_redact() {
        let host = Decision::Redact(mask(&["result.verdict"]));
        assert_eq!(host.clone().meet(Decision::Allow), host);
    }

    #[test]
    fn redact_meets_redact_by_union_of_masks() {
        let a = Decision::Redact(mask(&["result.verdict"]));
        let b = Decision::Redact(mask(&["result.score"]));
        assert_eq!(
            a.meet(b),
            Decision::Redact(mask(&["result.verdict", "result.score"]))
        );
    }

    #[test]
    fn meet_is_commutative() {
        let a = Decision::Redact(mask(&["x"]));
        let b = Decision::Deny;
        assert_eq!(a.clone().meet(b.clone()), b.meet(a));
    }

    #[test]
    fn meet_is_associative() {
        let a = Decision::Allow;
        let b = Decision::Redact(mask(&["x"]));
        let c = Decision::Redact(mask(&["y"]));
        assert_eq!(a.clone().meet(b.clone()).meet(c.clone()), a.meet(b.meet(c)));
    }

    #[test]
    fn meet_is_idempotent() {
        let a = Decision::Redact(mask(&["x"]));
        assert_eq!(a.clone().meet(a.clone()), a);
    }
}
