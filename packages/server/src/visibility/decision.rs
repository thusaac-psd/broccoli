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

    // M2: the six tests above look like a complete lattice-law suite, but a
    // mutant that replaces `meet`'s whole body with `self` (a first-argument
    // projection) survives FOUR of them. Not by sloppiness - a
    // first-argument projection genuinely IS associative
    // (`f(f(a,b),c) == f(a,f(b,c)) == a`, trivially, for ANY `f` that always
    // returns its first argument) and idempotent (`f(a,a) == a`, same
    // reason), so `meet_is_associative`/`meet_is_idempotent` are
    // mathematically incapable of ever detecting this mutant - no
    // assertion strengthening fixes that, only a different property can.
    // `plugin_cannot_widen_a_host_deny` and `plugin_cannot_widen_a_host_redact`
    // also survive it, but only because BOTH happen to write the "expected
    // winner" (`Deny`, `host`) as the FIRST `.meet()` operand - the
    // projection returns the right answer by construction of the test, not
    // because it implements `meet`. The two tests below pick the operand
    // order that a first-argument projection gets wrong: Allow written
    // FIRST (where a projection would wrongly answer `Allow` itself), and
    // an explicit swap-and-compare for commutativity. Confirmed by hand:
    // temporarily changing `meet`'s body to `self` fails both of these
    // (and `redact_meets_redact_by_union_of_masks`, and `meet_is_commutative`
    // above) while leaving `meet_is_associative`/`meet_is_idempotent`/
    // `plugin_cannot_widen_a_host_deny`/`plugin_cannot_widen_a_host_redact`
    // green - restoring the real implementation makes the whole module
    // green again.

    #[test]
    fn allow_is_the_meet_identity_from_either_operand_position() {
        // `plugin_cannot_widen_a_host_redact` above already pins
        // `x.meet(Allow) == x` (Allow as the SECOND operand) - a
        // first-argument-projection stub answers that correctly by
        // accident, since it returns `x` regardless of what `Allow` even
        // is. Allow as the FIRST operand is the direction that actually
        // distinguishes real `meet` from that stub: a projection would
        // wrongly answer `Allow` itself instead of `x`.
        let redact = Decision::Redact(mask(&["x"]));
        assert_eq!(Decision::Allow.meet(redact.clone()), redact);
        assert_eq!(Decision::Allow.meet(Decision::Deny), Decision::Deny);
    }

    #[test]
    fn meet_is_commutative_across_every_variant_pair() {
        // `meet_is_commutative` above already swaps Redact/Deny and
        // correctly fails the first-argument-projection stub. This test
        // adds the pairs that stub's OTHER two survivors
        // (`plugin_cannot_widen_a_host_deny`, `plugin_cannot_widen_a_host_redact`)
        // never swap - both always write the expected winner first. Swap
        // order here on purpose: a first-argument projection cannot be
        // commutative for any pair of genuinely different values, since it
        // always returns whichever side happens to be written first.
        let redact = Decision::Redact(mask(&["x"]));
        assert_eq!(
            Decision::Allow.meet(redact.clone()),
            redact.meet(Decision::Allow)
        );
        assert_eq!(
            Decision::Allow.meet(Decision::Deny),
            Decision::Deny.meet(Decision::Allow)
        );
    }
}
