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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResource {
    pub kind: String,
    pub id: i32,
    #[serde(default)]
    pub contest_id: Option<i32>,
}

/// Plugin -> host. `decisions` is positional and MUST be the same length as the
/// request's `resources`; any other length is a protocol violation and the host
/// denies the whole batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VisibilityQueryOutput {
    pub decisions: Vec<WireDecision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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
            id: 42,
            contest_id: Some(7),
        };
        let json = serde_json::to_string(&resource).unwrap();
        assert!(
            json.contains(r#""contest_id":7"#),
            "contest_id must be present on the wire: {json}"
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
        let back: QueryResource = serde_json::from_str(r#"{"kind":"submission","id":42}"#).unwrap();
        assert_eq!(back.contest_id, None);
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
