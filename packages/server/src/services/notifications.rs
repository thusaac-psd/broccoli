use broccoli_server_sdk::types::PublishNotification;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement, TransactionTrait};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Notification {
    /// Decimal strings keep PostgreSQL BIGINT cursors lossless in JavaScript.
    pub id: String,
    pub key: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

pub async fn publish(
    db: &DatabaseConnection,
    plugin_id: &str,
    message: &PublishNotification,
) -> Result<(), DbErr> {
    message.validate().map_err(DbErr::Custom)?;
    let mut recipients = message.user_ids.clone();
    recipients.sort_unstable();
    recipients.dedup();
    let txn = db.begin().await?;
    // Serialize allocation AND commit for each recipient's stream. Sequence
    // numbers alone are not commit ordered: otherwise a slow insert of id N
    // could commit after a client has advanced its cursor past N+1 and be lost.
    // Sorted locks also prevent deadlocks when the recipient sets overlap.
    for user_id in &recipients {
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            [format!("notification:{}:{user_id}", message.contest_id).into()],
        ))
        .await?;
    }
    for user_id in recipients {
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO plugin_notification (plugin_id, contest_id, user_id, event_key, kind, payload)
             VALUES ($1,$2,$3,$4,$5,$6)
             ON CONFLICT (plugin_id, contest_id, user_id, event_key) DO NOTHING",
            [plugin_id.into(), message.contest_id.into(), user_id.into(), message.key.clone().into(),
             message.kind.clone().into(), message.payload.clone().into()],
        )).await?;
    }
    txn.commit().await
}

pub async fn watermark(
    db: &DatabaseConnection,
    contest_id: i32,
    user_id: i32,
    plugin_id: &str,
) -> Result<i64, DbErr> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COALESCE(MAX(id), 0) AS cursor FROM plugin_notification
         WHERE contest_id = $1 AND user_id = $2 AND plugin_id = $3",
            [contest_id.into(), user_id.into(), plugin_id.into()],
        ))
        .await?
        .ok_or_else(|| DbErr::Custom("Missing notification cursor".into()))?;
    row.try_get("", "cursor")
}

pub async fn after(
    db: &DatabaseConnection,
    contest_id: i32,
    user_id: i32,
    plugin_id: &str,
    cursor: i64,
) -> Result<Vec<Notification>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id, event_key, kind, payload FROM plugin_notification
         WHERE contest_id = $1 AND user_id = $2 AND plugin_id = $3 AND id > $4
         ORDER BY id LIMIT 100",
            [
                contest_id.into(),
                user_id.into(),
                plugin_id.into(),
                cursor.into(),
            ],
        ))
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok(Notification {
                id: row.try_get::<i64>("", "id")?.to_string(),
                key: row.try_get("", "event_key")?,
                kind: row.try_get("", "kind")?,
                payload: row.try_get("", "payload")?,
            })
        })
        .collect()
}
