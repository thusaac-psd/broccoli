//! Durable contest feedback, separate from the judge/dispatcher lifecycle.
use std::collections::HashMap;

use broccoli_server_sdk::types::SubmissionAdmission;
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DbBackend, EntityTrait, QuerySelect, Set, Statement,
    TransactionTrait,
};
use serde_json::{Value, json};

use crate::entity::{plugin_storage, submission};
use crate::error::AppError;
use crate::state::AppState;

/// Filtering and sorting must use the same public status as the response;
/// otherwise `?status=Judged` reveals an AC that is still withheld.
pub fn public_status(can_view_all: bool) -> sea_orm::sea_query::Expr {
    sea_orm::sea_query::Expr::cust(if can_view_all {
        "submission.status"
    } else {
        "CASE WHEN EXISTS (SELECT 1 FROM submission_feedback f WHERE f.submission_id = submission.id AND f.outcome IN ('pending', 'retired')) THEN 'Running' ELSE submission.status END"
    })
}

fn rejected(code: &str, message: &str) -> AppError {
    AppError::PluginRejection {
        code: code.into(),
        message: message.into(),
        status_code: 409,
        details: None,
    }
}

/// A context-free submission cannot participate in atomic contest admission.
/// Also check linked contests: choosing a different contest type must not let
/// a contestant use the judge to avoid penalties during a live match.
pub async fn require_standalone_allowed(
    state: &AppState,
    problem_id: i32,
    contest_type: &str,
) -> Result<(), AppError> {
    let types: Vec<String> = state
        .registries
        .contest_type_registry
        .read()
        .await
        .iter()
        .filter(|(_, handler)| handler.delayed_feedback)
        .map(|(name, _)| name.clone())
        .collect();
    if types.is_empty() {
        return Ok(());
    }
    let selected_delayed_type = types.iter().any(|name| name == contest_type);
    let placeholders = (2..=types.len() + 1)
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    let mut args = vec![problem_id.into()];
    args.extend(types.into_iter().map(Into::into));
    let linked = state
        .db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!(
                "SELECT c.id FROM contest c JOIN contest_problem cp ON cp.contest_id = c.id
            WHERE cp.problem_id = $1 AND c.deleted_at IS NULL
              AND (c.end_time IS NULL OR c.end_time > clock_timestamp())
              AND c.contest_type IN ({placeholders}) LIMIT 1"
            ),
            args,
        ))
        .await?
        .is_some();
    if selected_delayed_type || linked {
        return Err(AppError::PluginRejection {
            code: "CONTEST_SUBMISSION_REQUIRED".into(),
            message:
                "Submit this problem through its contest so the contest's submission rules apply."
                    .into(),
            status_code: 400,
            details: None,
        });
    }
    Ok(())
}

/// Opponent admissions may change the guard without changing the game. A
/// second request from the same player must rerun its hooks even if the first
/// submission has already finished and freed the pending-feedback slot.
fn guarded_state(current_raw: &str, expected_raw: &str, user_id: i32) -> Result<Value, AppError> {
    let mut current: Value =
        serde_json::from_str(current_raw).map_err(|e| AppError::Internal(e.to_string()))?;
    let mut expected: Value =
        serde_json::from_str(expected_raw).map_err(|e| AppError::Internal(e.to_string()))?;
    let user_key = user_id.to_string();
    let user_revision = current["admission_by_user"][&user_key]
        .as_u64()
        .unwrap_or(0);
    if user_revision
        != expected["admission_by_user"][&user_key]
            .as_u64()
            .unwrap_or(0)
    {
        return Err(rejected(
            "SUBMISSION_IN_FLIGHT",
            "Another submission was admitted; refresh and retry after its result.",
        ));
    }
    let revision = current["admission_revision"].as_u64().unwrap_or(0);
    let mut users = current["admission_by_user"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for state in [&mut current, &mut expected] {
        let obj = state
            .as_object_mut()
            .ok_or_else(|| AppError::Internal("Invalid admission state".into()))?;
        obj.remove("admission_revision");
        obj.remove("admission_by_user");
    }
    if current != expected {
        return Err(rejected(
            "SUBMISSION_STATE_CHANGED",
            "The game has changed; refresh and retry.",
        ));
    }
    users.insert(user_key, json!(user_revision.saturating_add(1)));
    current["admission_revision"] = json!(revision.saturating_add(1));
    current["admission_by_user"] = json!(users);
    Ok(current)
}

/// All hooks have finished before this short transaction starts. Lock and bump
/// the plugin's guarded state so a deciding CAS cannot overlook this submission.
/// The pending-feedback unique index also serializes requests from one player.
pub async fn insert_admitted(
    state: &AppState,
    mut new: submission::ActiveModel,
    admission: &SubmissionAdmission,
    plugin_id: &str,
) -> Result<submission::Model, AppError> {
    if admission.delay_seconds < 0 || admission.delay_seconds > i64::MAX / 1_000_000 {
        return Err(AppError::Internal("Invalid submission delay".into()));
    }
    let txn = state.db.begin().await?;
    let row = plugin_storage::Entity::find_by_id((
        plugin_id.to_owned(),
        "default".to_owned(),
        admission.state_key.clone(),
    ))
    .lock_exclusive()
    .one(&txn)
    .await?
    .ok_or_else(|| {
        rejected(
            "SUBMISSION_STATE_CHANGED",
            "The game has changed; refresh and retry.",
        )
    })?;
    let current_raw = row
        .data
        .as_str()
        .ok_or_else(|| AppError::Internal("Invalid admission state".into()))?;
    let current = guarded_state(
        current_raw,
        &admission.expected_state,
        new.user_id.clone().unwrap(),
    )?;
    let mut guarded: plugin_storage::ActiveModel = row.into();
    guarded.data = Set(json!(current.to_string()));
    guarded.update(&txn).await?;

    // Use database arrival time AFTER acquiring the guard, never a timestamp
    // captured before slow hooks. A winning CAS cannot miss an older arrival.
    let clock = txn
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT clock_timestamp() AS now".to_owned(),
        ))
        .await?
        .ok_or_else(|| AppError::Internal("Missing database clock".into()))?;
    let now: DateTime<Utc> = clock.try_get("", "now")?;
    let effective_at_us = now
        .timestamp_micros()
        .checked_add(admission.delay_seconds * 1_000_000)
        .ok_or_else(|| AppError::Internal("Submission time overflow".into()))?;
    if effective_at_us > admission.deadline_ms.saturating_mul(1_000) {
        return Err(rejected(
            "SUBMISSION_TOO_LATE",
            "The effective submission time is past this game's deadline.",
        ));
    }
    new.created_at = Set(now);
    let model = new.insert(&txn).await?;
    let inserted = txn.execute_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO submission_feedback (submission_id, contest_id, user_id, plugin_id, state_key, scope_key, effective_at_us, delay_seconds)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT DO NOTHING",
        [model.id.into(), model.contest_id.into(), model.user_id.into(), plugin_id.into(),
         admission.state_key.clone().into(), admission.scope_key.clone().into(), effective_at_us.into(), admission.delay_seconds.into()],
    )).await?;
    if inserted.rows_affected() == 0 {
        return Err(rejected(
            "SUBMISSION_IN_FLIGHT",
            "Wait for your previous submission's result before submitting again.",
        ));
    }
    txn.commit().await?;
    Ok(model)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Feedback {
    /// Includes retired admissions: their results are still withheld, but
    /// staff have closed the match and freed their submission slot.
    Pending,
    TooLate,
}

pub async fn load(state: &AppState, ids: &[i32]) -> Result<HashMap<i32, Feedback>, AppError> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let placeholders = (1..=ids.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    let rows = state.db.query_all_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT submission_id, outcome FROM submission_feedback WHERE submission_id IN ({placeholders}) AND outcome <> 'released'"),
        ids.iter().copied().map(Into::into),
    )).await?;
    let mut feedback = HashMap::new();
    for row in rows {
        let id: i32 = row.try_get("", "submission_id")?;
        let outcome: String = row.try_get("", "outcome")?;
        feedback.insert(
            id,
            if outcome == "too_late" {
                Feedback::TooLate
            } else {
                Feedback::Pending
            },
        );
    }
    Ok(feedback)
}

/// Project only contest feedback; the real stored verdict remains available to
/// staff and to the retry/lease machinery. Visibility masks run AFTER this.
pub fn project(value: &mut Value, feedback: Feedback) {
    if feedback == Feedback::Pending {
        value["status"] = json!("Running");
        if value.get("result").is_some() {
            value["result"] = Value::Null;
        }
        for key in ["verdict", "score", "time_used", "memory_used", "judged_at"] {
            if value.get(key).is_some() {
                value[key] = Value::Null;
            }
        }
    } else {
        if value.get("verdict").is_some() {
            value["verdict"] = json!("TooLate");
        }
        if value.get("score").is_some() {
            value["score"] = json!(0.0);
        }
        if value["result"].is_object() {
            value["result"]["verdict"] = json!("TooLate");
            value["result"]["score"] = json!(0.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_guard_supports_old_documents_and_concurrent_opponents() {
        let original = json!({"state": "in_progress"}).to_string();
        let first = guarded_state(&original, &original, 10).unwrap();
        assert_eq!(first["admission_by_user"]["10"], 1);
        let opponent = guarded_state(&first.to_string(), &original, 20).unwrap();
        assert_eq!(opponent["admission_revision"], 2);
        assert_eq!(opponent["admission_by_user"]["10"], 1);
        assert_eq!(opponent["admission_by_user"]["20"], 1);
    }

    #[test]
    fn admission_guard_rejects_stale_player_even_after_pending_slot_is_released() {
        let original = json!({"state": "in_progress"}).to_string();
        let first = guarded_state(&original, &original, 10).unwrap();
        let error = guarded_state(&first.to_string(), &original, 10).unwrap_err();
        assert!(
            matches!(error, AppError::PluginRejection { code, .. } if code == "SUBMISSION_IN_FLIGHT")
        );
    }

    #[test]
    fn admission_guard_rejects_a_game_changed_after_hooks() {
        let error = guarded_state(r#"{"game":1}"#, r#"{"game":0}"#, 10).unwrap_err();
        assert!(
            matches!(error, AppError::PluginRejection { code, .. } if code == "SUBMISSION_STATE_CHANGED")
        );
    }

    #[test]
    fn pending_feedback_hides_details_and_keeps_polling() {
        let mut value = json!({"id": 7, "status": "Judged", "result": {
            "verdict": "Accepted", "score": 1, "test_case_results": [{"verdict": "Accepted"}]
        }});
        project(&mut value, Feedback::Pending);
        assert_eq!(value, json!({"id": 7, "status": "Running", "result": null}));
        let mut list =
            json!({"status": "Judged", "verdict": "Accepted", "score": 1, "time_used": 1});
        project(&mut list, Feedback::Pending);
        assert_eq!(list["status"], "Running");
        assert!(
            list["verdict"].is_null() && list["score"].is_null() && list["time_used"].is_null()
        );
    }

    #[test]
    fn losing_correct_submission_has_a_distinct_public_result() {
        let mut value = json!({"status": "Judged", "result": {"verdict": "Accepted", "score": 1}});
        project(&mut value, Feedback::TooLate);
        assert_eq!(value["result"]["verdict"], "TooLate");
        assert_eq!(value["result"]["score"], 0.0);
    }
}
