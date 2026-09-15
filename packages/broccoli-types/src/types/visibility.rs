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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryContext {
    pub contest_id: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResource {
    pub kind: String,
    pub id: i32,
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
