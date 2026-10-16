use serde::{Deserialize, Serialize};

/// Plugin -> host response from a hook function (e.g. a `before_submission`
/// handler). The guest serializes this as its return value; the host
/// (plugin-core) deserializes it and maps it onto its internal hook action.
///
/// Wire-format note: `Reject` fields are declared in alphabetical order so
/// the derived serializer emits the exact bytes the plugins historically
/// produced with `serde_json::json!` (whose map sorts keys alphabetically).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
pub enum HookResponse {
    Pass,
    Stop,
    Reject {
        #[serde(default = "default_reject_code")]
        code: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<serde_json::Value>,
        #[serde(default = "default_reject_message")]
        message: String,
        #[serde(default = "default_reject_status")]
        status_code: u16,
    },
    Modified {
        event: serde_json::Value,
    },
}

fn default_reject_code() -> String {
    "PLUGIN_REJECTED".into()
}
fn default_reject_message() -> String {
    "Request rejected by plugin".into()
}
fn default_reject_status() -> u16 {
    400
}

impl HookResponse {
    /// Return atomic admission effects from a before-submission hook without
    /// constructing the reserved wire payload by hand. The calling plugin must
    /// own this contest's type and have registered it with admission enabled.
    pub fn admit(
        event: &super::BeforeSubmissionEvent,
        admission: super::SubmissionAdmission,
    ) -> Result<Self, serde_json::Error> {
        let mut event = serde_json::to_value(event)?;
        event["submission_admission"] = serde_json::to_value(admission)?;
        Ok(Self::Modified { event })
    }

    pub fn pass() -> Self {
        HookResponse::Pass
    }

    pub fn stop() -> Self {
        HookResponse::Stop
    }

    pub fn reject(
        code: impl Into<String>,
        message: impl Into<String>,
        status_code: u16,
        details: Option<serde_json::Value>,
    ) -> Self {
        HookResponse::Reject {
            code: code.into(),
            details,
            message: message.into(),
            status_code,
        }
    }

    pub fn modified(event: serde_json::Value) -> Self {
        HookResponse::Modified { event }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_response_preserves_the_event_and_encodes_typed_effects() {
        use super::super::*;
        let event = BeforeSubmissionEvent {
            user_id: 1,
            problem_id: 2,
            contest_id: Some(3),
            contest_type: "example".into(),
            language: "cpp".into(),
            file_count: 1,
        };
        let admission = SubmissionAdmission {
            guard: SubmissionStorageGuard::from_snapshot(
                "state",
                StorageSnapshot {
                    value: "opaque".into(),
                    revision: 7,
                },
            ),
            feedback_key: "attempts".into(),
            metadata: serde_json::json!({"own":true}),
            slot: None,
            latest_arrival_ms: None,
        };
        let response = HookResponse::admit(&event, admission).unwrap();
        let wire = serde_json::to_value(response).unwrap();
        let decoded: BeforeSubmissionEvent = serde_json::from_value(wire["event"].clone()).unwrap();
        assert_eq!(decoded.user_id, event.user_id);
        assert_eq!(decoded.contest_type, event.contest_type);
        let admission: SubmissionAdmission =
            serde_json::from_value(wire["event"]["submission_admission"].clone()).unwrap();
        assert_eq!(admission.guard.expected_value(), "opaque");
        assert_eq!(admission.guard.key(), "state");
    }

    #[test]
    fn pass_serializes_to_legacy_wire_format() {
        let json = serde_json::to_string(&HookResponse::pass()).unwrap();
        assert_eq!(json, r#"{"action":"pass"}"#);
    }

    #[test]
    fn stop_serializes_to_legacy_wire_format() {
        let json = serde_json::to_string(&HookResponse::stop()).unwrap();
        assert_eq!(json, r#"{"action":"stop"}"#);
    }

    #[test]
    fn reject_serializes_to_legacy_wire_format_and_round_trips() {
        let resp = HookResponse::reject(
            "COOLDOWN_ACTIVE",
            "Please wait 3 more seconds",
            429,
            Some(serde_json::json!({
                "remaining_seconds": 3,
                "cooldown_seconds": 10,
            })),
        );
        let json = serde_json::to_string(&resp).unwrap();
        // Byte-identical to the JSON the plugins previously built by hand
        // with `serde_json::json!` (alphabetical key order).
        assert_eq!(
            json,
            r#"{"action":"reject","code":"COOLDOWN_ACTIVE","details":{"cooldown_seconds":10,"remaining_seconds":3},"message":"Please wait 3 more seconds","status_code":429}"#
        );

        let back: HookResponse = serde_json::from_str(&json).unwrap();
        match back {
            HookResponse::Reject {
                code,
                details,
                message,
                status_code,
            } => {
                assert_eq!(code, "COOLDOWN_ACTIVE");
                assert_eq!(message, "Please wait 3 more seconds");
                assert_eq!(status_code, 429);
                assert_eq!(details.unwrap()["remaining_seconds"], 3);
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn reject_deserializes_with_host_defaults_when_fields_are_omitted() {
        let back: HookResponse = serde_json::from_str(r#"{"action":"reject"}"#).unwrap();
        match back {
            HookResponse::Reject {
                code,
                details,
                message,
                status_code,
            } => {
                assert_eq!(code, "PLUGIN_REJECTED");
                assert_eq!(message, "Request rejected by plugin");
                assert_eq!(status_code, 400);
                assert!(details.is_none());
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn modified_round_trips_with_event_payload() {
        let resp = HookResponse::modified(serde_json::json!({"topic": "t", "user_id": 1}));
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(
            json,
            r#"{"action":"modified","event":{"topic":"t","user_id":1}}"#
        );
        let back: HookResponse = serde_json::from_str(&json).unwrap();
        match back {
            HookResponse::Modified { event } => assert_eq!(event["topic"], "t"),
            other => panic!("expected Modified, got {other:?}"),
        }
    }
}
