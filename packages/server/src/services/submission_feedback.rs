//! Atomic admission and durable, plugin-owned public feedback.
use crate::entity::{plugin_storage, submission};
use crate::error::AppError;
use crate::state::AppState;
use broccoli_server_sdk::types::{
    FeedbackUpdateOutcome, SubmissionAdmission, SubmissionFeedbackAction, SubmissionFeedbackQuery,
    SubmissionFeedbackRecord, SubmissionFeedbackResult, SubmissionSlotObservation,
    SubmissionSlotToken,
};
use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DbBackend, DbErr, EntityTrait,
    QuerySelect, Set, Statement, TransactionTrait,
};
use serde_json::{Value, json};
use std::collections::HashMap;

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

/// No connection spans a guest call. The host compares opaque storage bytes,
/// advances its own revision, inserts the submission and holds feedback atomically.
pub async fn insert_admitted(
    state: &AppState,
    mut new: submission::ActiveModel,
    admission: &SubmissionAdmission,
    plugin_id: &str,
) -> Result<submission::Model, AppError> {
    let txn = state.db.begin().await?;
    if let Some(slot) = &admission.slot {
        // Namespaces are chosen by plugins. Lock before checking the generation,
        // including when two admissions guard different storage keys.
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [format!("submission-slot:{plugin_id}:{}", slot.key()).into()],
        ))
        .await?;
        let (generation, pending) = slot_state(&txn, plugin_id, slot.key()).await?;
        if generation != slot.generation() || pending.is_some() {
            return Err(rejected(
                "SUBMISSION_IN_FLIGHT",
                "Submission slot changed; refresh and retry.",
            ));
        }
    }
    let row = plugin_storage::Entity::find_by_id((
        plugin_id.to_owned(),
        "default".to_owned(),
        admission.guard.key().to_owned(),
    ))
    .lock_exclusive()
    .one(&txn)
    .await?
    .ok_or_else(|| {
        rejected(
            "SUBMISSION_STATE_CHANGED",
            "Admission state changed; refresh and retry.",
        )
    })?;
    if crate::utils::text::plugin_storage_text(&row.data) != admission.guard.expected_value() {
        return Err(rejected(
            "SUBMISSION_STATE_CHANGED",
            "Admission state changed; refresh and retry.",
        ));
    }
    let mut guarded: plugin_storage::ActiveModel = row.clone().into();
    guarded.revision = Set(row
        .revision
        .checked_add(1)
        .ok_or_else(|| AppError::Internal("Storage revision overflow".into()))?);
    guarded.update(&txn).await?;
    let clock = txn
        .query_one_raw(Statement::from_string(
            DbBackend::Postgres,
            "SELECT clock_timestamp() AS now".to_owned(),
        ))
        .await?
        .ok_or_else(|| AppError::Internal("Missing database clock".into()))?;
    let now: DateTime<Utc> = clock.try_get("", "now")?;
    if admission
        .latest_arrival_ms
        .is_some_and(|end| i128::from(now.timestamp_micros()) >= i128::from(end) * 1000)
    {
        return Err(rejected(
            "SUBMISSION_TOO_LATE",
            "Submission admission window has closed.",
        ));
    }
    new.created_at = Set(now);
    let model = new.insert(&txn).await?;
    txn.execute_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO submission_feedback (submission_id, contest_id, user_id, plugin_id, state_key, metadata, slot_key) VALUES ($1,$2,$3,$4,$5,$6,$7)",
        [model.id.into(), model.contest_id.into(), model.user_id.into(), plugin_id.into(),
         admission.feedback_key.clone().into(), admission.metadata.clone().into(), admission.slot.as_ref().map(|s| s.key().to_owned()).into()])).await?;
    txn.commit().await?;
    Ok(model)
}

/// The capability is scoped to its caller; plugins cannot read another owner's records.
pub async fn query(
    db: &impl ConnectionTrait,
    plugin_id: &str,
    query: &SubmissionFeedbackQuery,
) -> Result<Vec<SubmissionFeedbackRecord>, DbErr> {
    let rows = db.query_all_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT f.submission_id, f.contest_id, f.user_id, s.problem_id,
        (EXTRACT(EPOCH FROM s.created_at)*1000000)::bigint AS submitted_at_us,
        s.status::text AS status, s.verdict, f.state_key, f.metadata, f.outcome, f.public_result, f.slot_key
        FROM submission_feedback f JOIN submission s ON s.id = f.submission_id
        WHERE f.plugin_id = $1 AND ($2::text IS NULL OR f.state_key = $2)
        AND ($3::text IS NULL OR f.slot_key = $3)
        AND ($4::boolean = false OR f.outcome IN ('pending','retired'))
        AND f.submission_id > $5
        AND ($7::jsonb IS NULL OR f.submission_id IN (SELECT value::int FROM jsonb_array_elements_text($7)))
        ORDER BY f.submission_id LIMIT $6",
        [plugin_id.into(), query.key.clone().into(), query.slot_key.clone().into(), query.unpublished_only.into(),
         query.after_id.into(), i64::from(query.page_size()).into(),
         query.submission_ids.as_ref().map(serde_json::to_value).transpose().map_err(|e| DbErr::Custom(e.to_string()))?.into()])).await?;
    let mut records = Vec::new();
    for row in rows {
        let submission_id = row.try_get("", "submission_id")?;
        let status: String = row.try_get("", "status")?;
        let verdict: Option<String> = row.try_get("", "verdict")?;
        let public_result: Option<Value> = row.try_get("", "public_result")?;
        let outcome: String = row.try_get("", "outcome")?;
        let slot_key: Option<String> = row.try_get("", "slot_key")?;
        records.push(SubmissionFeedbackRecord {
            submission_id,
            contest_id: row.try_get("", "contest_id")?,
            user_id: row.try_get("", "user_id")?,
            problem_id: row.try_get("", "problem_id")?,
            submitted_at_us: row.try_get("", "submitted_at_us")?,
            status: serde_json::from_value(json!(status))
                .map_err(|e| DbErr::Custom(e.to_string()))?,
            verdict: verdict
                .map(|v| serde_json::from_value(json!(v)))
                .transpose()
                .map_err(|e| DbErr::Custom(e.to_string()))?,
            key: row.try_get("", "state_key")?,
            metadata: row.try_get("", "metadata")?,
            withheld: outcome != "released",
            slot_held: outcome == "pending" && slot_key.is_some(),
            slot_key,
            result: public_result
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| DbErr::Custom(e.to_string()))?,
        });
    }
    Ok(records)
}

/// Read only the generation and outstanding record, regardless of slot history.
async fn slot_state(
    db: &impl ConnectionTrait,
    plugin_id: &str,
    key: &str,
) -> Result<(Option<i32>, Option<i32>), DbErr> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT
         (SELECT submission_id FROM submission_feedback WHERE plugin_id = $1 AND slot_key = $2
          ORDER BY submission_id DESC LIMIT 1) AS generation,
         (SELECT submission_id FROM submission_feedback WHERE plugin_id = $1 AND slot_key = $2
          AND outcome = 'pending' LIMIT 1) AS pending",
            [plugin_id.into(), key.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("Missing slot observation".into()))?;
    Ok((row.try_get("", "generation")?, row.try_get("", "pending")?))
}

pub async fn observe_slot(
    db: &DatabaseConnection,
    plugin_id: &str,
    key: &str,
) -> Result<SubmissionSlotObservation, DbErr> {
    let (generation, pending) = slot_state(db, plugin_id, key).await?;
    let pending = if let Some(id) = pending {
        query(
            db,
            plugin_id,
            &SubmissionFeedbackQuery {
                submission_ids: Some(vec![id]),
                limit: 1,
                ..Default::default()
            },
        )
        .await?
        .into_iter()
        .find(|r| r.slot_held)
    } else {
        None
    };
    Ok(SubmissionSlotObservation {
        token: SubmissionSlotToken::new(key.to_owned(), generation),
        pending,
    })
}

/// Serialize competing releases on the feedback row. Raw judgements are never
/// modified. Missing/foreign rows and conflicting final releases are errors.
pub async fn update(
    db: &DatabaseConnection,
    plugin_id: &str,
    id: i32,
    action: &SubmissionFeedbackAction,
) -> Result<FeedbackUpdateOutcome, DbErr> {
    let txn = db.begin().await?;
    let locked = txn
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT submission_id FROM submission_feedback WHERE plugin_id = $1
         AND submission_id = $2 FOR UPDATE",
            [plugin_id.into(), id.into()],
        ))
        .await?;
    if locked.is_none() {
        return Err(DbErr::Custom(
            "Feedback record not found for this plugin".into(),
        ));
    }
    let mut record = query(
        &txn,
        plugin_id,
        &SubmissionFeedbackQuery {
            submission_ids: Some(vec![id]),
            limit: 1,
            ..Default::default()
        },
    )
    .await?
    .pop()
    .ok_or_else(|| DbErr::Custom("Feedback record not found".into()))?;
    let result = record
        .apply_action(action)
        .map_err(|e| DbErr::Custom(e.into()))?;
    if result == FeedbackUpdateOutcome::Applied {
        let outcome = if !record.withheld {
            "released"
        } else {
            "retired"
        };
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE submission_feedback SET outcome = $1, public_result = $2
             WHERE plugin_id = $3 AND submission_id = $4",
            [
                outcome.into(),
                record
                    .result
                    .as_ref()
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(|e| DbErr::Custom(e.to_string()))?
                    .into(),
                plugin_id.into(),
                id.into(),
            ],
        ))
        .await?;
    }
    txn.commit().await?;
    Ok(result)
}

#[derive(Clone, Debug)]
pub enum Feedback {
    Pending,
    Published(SubmissionFeedbackResult),
}

pub async fn load(state: &AppState, ids: &[i32]) -> Result<HashMap<i32, Feedback>, AppError> {
    let mut feedback = HashMap::new();
    if ids.is_empty() {
        return Ok(feedback);
    }
    let placeholders = (1..=ids.len())
        .map(|n| format!("${n}"))
        .collect::<Vec<_>>()
        .join(",");
    let rows = state.db.query_all_raw(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT submission_id, outcome, public_result FROM submission_feedback WHERE submission_id IN ({placeholders}) AND (outcome <> 'released' OR public_result IS NOT NULL)"),
        ids.iter().copied().map(Into::into))).await?;
    for row in rows {
        let id = row.try_get("", "submission_id")?;
        let outcome: String = row.try_get("", "outcome")?;
        let value = if outcome != "released" {
            Feedback::Pending
        } else {
            let result: Value = row.try_get("", "public_result")?;
            Feedback::Published(
                serde_json::from_value(result).map_err(|e| AppError::Internal(e.to_string()))?,
            )
        };
        feedback.insert(id, value);
    }
    Ok(feedback)
}

/// Presentation overlays use submission API fields, never contest-specific verdicts.
pub fn project(value: &mut Value, feedback: Feedback) {
    match feedback {
        Feedback::Pending => {
            value["status"] = json!("Running");
            if value.get("result").is_some() {
                value["result"] = Value::Null;
            }
            for key in ["verdict", "score", "time_used", "memory_used", "judged_at"] {
                if value.get(key).is_some() {
                    value[key] = Value::Null;
                }
            }
        }
        Feedback::Published(result) => {
            apply_public_result(value, &result);
            if value["result"].is_object() {
                apply_public_result(&mut value["result"], &result);
            }
        }
    }
}
fn apply_public_result(value: &mut Value, result: &SubmissionFeedbackResult) {
    if value.get("verdict").is_some() {
        value["verdict"] = json!(result.verdict);
    }
    if value.get("score").is_some() {
        value["score"] = json!(result.score);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn withholding_hides_details_and_keeps_polling() {
        let mut value = json!({"id":7,"status":"Judged","result":{"verdict":"Accepted","test_case_results":[{}]}});
        project(&mut value, Feedback::Pending);
        assert_eq!(value, json!({"id":7,"status":"Running","result":null}));
    }
    #[test]
    fn public_results_support_arbitrary_plugin_verdicts() {
        let mut value = json!({"status":"Judged","result":{"verdict":"Accepted","score":1}});
        project(
            &mut value,
            Feedback::Published(SubmissionFeedbackResult {
                verdict: broccoli_server_sdk::types::Verdict::Other("ExampleResult".into()),
                score: 7.0,
            }),
        );
        assert_eq!(value["result"]["verdict"], "ExampleResult");
        assert_eq!(value["result"]["score"], 7.0);
    }
}
