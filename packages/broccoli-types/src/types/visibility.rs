use serde::{Deserialize, Serialize};

/// Host -> plugin visibility query. Neutral by construction: the host never
/// describes what a contest format is, only who is asking and about what.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityQueryInput {
    pub subject: QuerySubject,
    pub action: String,
    pub context: QueryContext,
    pub resources: Vec<QueryResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySubject {
    pub user_id: Option<i32>,
    pub authenticated: bool,
    #[serde(default)]
    pub permissions: Vec<String>,
}

/// Request-level scope. `contest_id` is a HINT: the contest the calling
/// endpoint itself is scoped to when it is contest-scoped, `None`
/// otherwise. It is not authoritative for any individual resource in the
/// batch — a single batch can span multiple contests (e.g. a submission
/// list mixing submissions from more than one contest), which this single
/// value cannot represent. See [`QueryResource::contest_id`] for the
/// authoritative, per-resource value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryContext {
    pub contest_id: Option<i32>,
}

/// One resource being asked about. `contest_id` is the AUTHORITATIVE
/// contest scope for this specific resource (as opposed to
/// [`QueryContext::contest_id`], which is only a request-level hint) -
/// `Some(id)` when this resource is known to belong to contest `id`
/// (including when the resource itself IS a contest), `None` when it is
/// genuinely contest-less (e.g. a standalone submission) or its contest is
/// not resolvable by the host. `#[serde(default)]` keeps this field
/// optional on the wire so any code deserializing an older, pre-existing
/// payload that never had it still round-trips.
///
/// `id` is the resource's ONE authoritative identifier, always rendered as
/// a decimal-string (i32-native kinds: contest/problem/sample/submission/
/// clarification) or a hyphenated UUID string (attachment) - never
/// truncated, never lossy. There is deliberately no second, narrower id
/// field alongside it: an earlier draft of the attachment wiring truncated
/// a `Uuid` into an `i32` here, which (a) collided across the WHOLE
/// installation's attachment table at the 32-bit birthday bound (~50% by
/// ~77,000 attachments) since `Uuid::now_v7`'s low bits are its random
/// component, and (b) gave a plugin no way to tell two different
/// attachments apart once their low 32 bits matched. A `String` costs
/// nothing extra on the wire (JSON has no native 128-bit integer anyway)
/// and closes the gap for every future kind, not just this one.
///
/// `problem_id` is the parent problem for a `problem`, `sample`, or
/// `attachment` resource (`None` for `contest`/`submission`/
/// `clarification`, none of which have one). It exists because `contest_id`
/// alone does not give an attachment-visibility plugin anything to reason
/// about: an attachment's `contest_id` is always `None` (a problem maps to
/// contests many-to-many via `contest_problem`, so there is no single
/// authoritative contest scope to report - see
/// `visibility::mod::resource_contest_id`'s doc comment), so without
/// `problem_id` a plugin would receive `{"kind":"attachment","id":"...",
/// "contest_id":null}` and have no attribute to condition a decision on at
/// all.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResource {
    pub kind: String,
    pub id: String,
    #[serde(default)]
    pub contest_id: Option<i32>,
    #[serde(default)]
    pub problem_id: Option<i32>,
}

/// Plugin -> host. `decisions` is positional and MUST be the same length as the
/// request's `resources`; any other length is a protocol violation and the host
/// denies the whole batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityQueryOutput {
    pub decisions: Vec<WireDecision>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireDecision {
    Allow {},
    Deny {},
    Redact {
        #[serde(default)]
        fields: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_round_trips_with_wire_format() {
        let json = serde_json::to_string(&WireDecision::Allow {}).unwrap();
        assert_eq!(json, r#"{"allow":{}}"#);

        let back: WireDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireDecision::Allow {}));
    }

    #[test]
    fn deny_round_trips_with_wire_format() {
        let json = serde_json::to_string(&WireDecision::Deny {}).unwrap();
        assert_eq!(json, r#"{"deny":{}}"#);

        let back: WireDecision = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, WireDecision::Deny {}));
    }

    #[test]
    fn redact_round_trips_with_wire_format() {
        let decision = WireDecision::Redact {
            fields: vec!["score".to_string(), "verdict".to_string()],
        };
        let json = serde_json::to_string(&decision).unwrap();
        assert_eq!(json, r#"{"redact":{"fields":["score","verdict"]}}"#);

        let back: WireDecision = serde_json::from_str(&json).unwrap();
        match back {
            WireDecision::Redact { fields } => {
                assert_eq!(fields, vec!["score".to_string(), "verdict".to_string()])
            }
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn redact_defaults_fields_to_empty_when_omitted() {
        let back: WireDecision = serde_json::from_str(r#"{"redact":{}}"#).unwrap();
        match back {
            WireDecision::Redact { fields } => assert!(fields.is_empty()),
            other => panic!("expected Redact, got {other:?}"),
        }
    }

    #[test]
    fn query_resource_serializes_and_round_trips_its_contest_id() {
        let resource = QueryResource {
            kind: "submission".to_string(),
            id: "42".to_string(),
            contest_id: Some(7),
            problem_id: None,
        };
        let json = serde_json::to_string(&resource).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"submission","id":"42","contest_id":7,"problem_id":null}"#
        );

        let back: QueryResource = serde_json::from_str(&json).unwrap();
        assert_eq!(back.contest_id, Some(7));
    }

    #[test]
    fn query_resource_contest_id_defaults_to_none_when_omitted() {
        // Round-trip tolerance for any payload built before this field
        // existed (`#[serde(default)]`) - a `QueryResource` with no
        // `contest_id` key at all must still deserialize, as a genuinely
        // contest-less resource rather than a protocol error.
        let back: QueryResource =
            serde_json::from_str(r#"{"kind":"submission","id":"42"}"#).unwrap();
        assert_eq!(back.contest_id, None);
        assert_eq!(back.problem_id, None);
    }

    #[test]
    fn query_resource_id_is_a_decimal_string_for_i32_native_kinds() {
        let resource = QueryResource {
            kind: "contest".to_string(),
            id: 7.to_string(),
            contest_id: Some(7),
            problem_id: None,
        };
        let json = serde_json::to_string(&resource).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"contest","id":"7","contest_id":7,"problem_id":null}"#
        );
    }

    #[test]
    fn query_resource_problem_id_serializes_and_round_trips() {
        let resource = QueryResource {
            kind: "attachment".to_string(),
            id: "0196c1d2-3a4b-7c8d-9e0f-1a2b3c4d5e6f".to_string(),
            contest_id: None,
            problem_id: Some(11),
        };
        let json = serde_json::to_string(&resource).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"attachment","id":"0196c1d2-3a4b-7c8d-9e0f-1a2b3c4d5e6f","contest_id":null,"problem_id":11}"#
        );

        let back: QueryResource = serde_json::from_str(&json).unwrap();
        assert_eq!(back.problem_id, Some(11));
        assert_eq!(back.id, "0196c1d2-3a4b-7c8d-9e0f-1a2b3c4d5e6f");
    }

    /// The whole point of `id: String`: two attachment UUIDs constructed so
    /// their low 32 bits are IDENTICAL (which is exactly what
    /// `id.as_u128() as i32` would have kept, in the truncating scheme this
    /// replaces) must still produce two DIFFERENT `QueryResource.id`
    /// values, because the full UUID - not just its low 32 bits - is now
    /// on the wire.
    #[test]
    fn distinct_attachment_uuids_with_colliding_low_32_bits_produce_distinct_wire_ids() {
        // Same low 32 bits (`3c4d5e6f`), different everything else -
        // `as_u128() as i32` would truncate both of these to the identical
        // i32 value, which is precisely the collision this fix closes.
        let uuid_a = uuid::Uuid::parse_str("0196c1d2-3a4b-7c8d-9e0f-1a2b3c4d5e6f").unwrap();
        let uuid_b = uuid::Uuid::parse_str("ffffffff-ffff-7fff-bfff-ffff3c4d5e6f").unwrap();
        // Sanity check on the premise: both really do truncate to the same
        // i32 under the old (now-deleted) scheme.
        assert_eq!(uuid_a.as_u128() as i32, uuid_b.as_u128() as i32);

        let resource_a = QueryResource {
            kind: "attachment".to_string(),
            id: uuid_a.to_string(),
            contest_id: None,
            problem_id: Some(1),
        };
        let resource_b = QueryResource {
            kind: "attachment".to_string(),
            id: uuid_b.to_string(),
            contest_id: None,
            problem_id: Some(2),
        };

        assert_ne!(resource_a.id, resource_b.id);
    }

    #[test]
    fn visibility_query_output_decisions_are_positional() {
        let output = VisibilityQueryOutput {
            decisions: vec![
                WireDecision::Allow {},
                WireDecision::Deny {},
                WireDecision::Redact {
                    fields: vec!["score".to_string()],
                },
            ],
        };
        let json = serde_json::to_string(&output).unwrap();
        let back: VisibilityQueryOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(back.decisions.len(), 3);
        assert!(matches!(back.decisions[0], WireDecision::Allow {}));
        assert!(matches!(back.decisions[1], WireDecision::Deny {}));
        assert!(matches!(back.decisions[2], WireDecision::Redact { .. }));
    }
}
