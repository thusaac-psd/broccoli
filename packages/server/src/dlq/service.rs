use common::db::DatabaseConnection;
use std::collections::HashMap;

use chrono::Utc;
use common::{DlqEnvelope, DlqErrorCode, DlqMessageType};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, sea_query::LockType,
};

use crate::entity::dead_letter_message;
use crate::models::shared::PageWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveResult {
    Resolved,
    NotFound,
    AlreadyResolved,
}

#[derive(Debug, Clone)]
pub struct DlqStats {
    pub total_unresolved: u64,
    pub total_resolved: u64,
    pub operation_task_count: u64,
    pub stuck_submission_count: u64,
    pub stuck_code_run_count: u64,
    pub stuck_submission_judgement_count: u64,
    pub unresolved_by_error_code: HashMap<String, u64>,
}

/// What a stuck-work dead letter is about. Each subject has at most one
/// row, keyed by [`DlqSubject::message_id`]: dead-lettering the same subject
/// again reopens that row and appends the occurrence to its retry history.
/// A submission's row is resolved automatically when its current judgement
/// ends with a verdict other than a system error (see `services::judging`),
/// leaving `resolved_by` NULL; nothing re-judges a stuck deferred judgement
/// or code run, so an operator resolves those. An operator resolution always
/// names the operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DlqSubject {
    /// A submission whose current judgement spent its dispatch retries.
    Submission(i32),
    /// A non-current (deferred rejudge) judgement that spent its retries.
    Judgement(i32),
    /// A code run that spent its retries.
    CodeRun(i32),
}

impl DlqSubject {
    fn prefix(self) -> &'static str {
        match self {
            DlqSubject::Submission(_) => "stuck-submission-",
            DlqSubject::Judgement(_) => "stuck-submission-judgement-",
            DlqSubject::CodeRun(_) => "stuck-code-run-",
        }
    }

    fn id(self) -> i32 {
        match self {
            DlqSubject::Submission(id) | DlqSubject::Judgement(id) | DlqSubject::CodeRun(id) => id,
        }
    }

    pub fn message_type(self) -> DlqMessageType {
        match self {
            DlqSubject::Submission(_) => DlqMessageType::StuckSubmission,
            DlqSubject::Judgement(_) => DlqMessageType::StuckSubmissionJudgement,
            DlqSubject::CodeRun(_) => DlqMessageType::StuckCodeRun,
        }
    }

    pub fn message_id(self) -> String {
        format!("{}{}", self.prefix(), self.id())
    }

    /// SQL for the message id of the subject of this kind whose id is the
    /// SQL expression `id`.
    pub(crate) fn sql_message_id(self, id: &str) -> String {
        format!("('{}' || {id})", self.prefix())
    }
}

pub struct DlqService<'a, C: ConnectionTrait> {
    conn: &'a C,
}

impl<'a, C: ConnectionTrait> DlqService<'a, C> {
    pub fn new(conn: &'a C) -> Self {
        Self { conn }
    }

    pub async fn send_to_dlq(
        &self,
        envelope: &DlqEnvelope,
    ) -> Result<dead_letter_message::Model, DbErr> {
        let first_failed_at = envelope
            .retry_history
            .first()
            .map(|r| r.timestamp)
            .unwrap_or_else(Utc::now);

        let model = dead_letter_message::ActiveModel {
            message_id: Set(envelope.message_id.clone()),
            message_type: Set(envelope.message_type.to_string()),
            submission_id: Set(envelope.submission_id),
            payload: Set(envelope.payload.clone()),
            error_message: Set(envelope.error_message.clone()),
            error_code: Set(envelope.error_code.to_string()),
            retry_count: Set(envelope.retry_history.len() as i32),
            retry_history: Set(serde_json::to_value(&envelope.retry_history).unwrap_or_default()),
            first_failed_at: Set(first_failed_at),
            created_at: Set(Utc::now()),
            resolved: Set(false),
            resolved_at: Set(None),
            resolved_by: Set(None),
            ..Default::default()
        };

        self.insert_entry(&envelope.message_id, model).await
    }

    /// Dead-letters `subject`: inserts its row, or reopens the existing one
    /// with the new error and payload, appends this occurrence to its retry
    /// history and restamps `last_failed_at`.
    pub async fn dead_letter(
        &self,
        subject: DlqSubject,
        submission_id: Option<i32>,
        payload: serde_json::Value,
        error_code: DlqErrorCode,
        error_message: &str,
    ) -> Result<(), DbErr> {
        self.conn
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                r#"INSERT INTO dead_letter_message AS d
                       (message_id, direction, submission_id, payload, error_message, error_code,
                        retry_count, retry_history, first_failed_at, created_at, resolved)
                   VALUES ($1, $2, $3, $4, $5, $6, 1,
                           jsonb_build_array(jsonb_build_object(
                               'attempt', 1, 'error', $5::text, 'timestamp', NOW())),
                           NOW(), NOW(), FALSE)
                   ON CONFLICT (message_id) DO UPDATE SET
                       payload = EXCLUDED.payload,
                       error_message = EXCLUDED.error_message,
                       error_code = EXCLUDED.error_code,
                       retry_history = d.retry_history || jsonb_build_array(jsonb_build_object(
                           'attempt', jsonb_array_length(d.retry_history) + 1,
                           'error', EXCLUDED.error_message,
                           'timestamp', NOW())),
                       retry_count = jsonb_array_length(d.retry_history) + 1,
                       last_failed_at = NOW(),
                       resolved = FALSE,
                       resolved_at = NULL,
                       resolved_by = NULL"#,
                [
                    subject.message_id().into(),
                    subject.message_type().to_string().into(),
                    submission_id.into(),
                    payload.into(),
                    error_message.into(),
                    error_code.to_string().into(),
                ],
            ))
            .await?;
        Ok(())
    }

    async fn insert_entry(
        &self,
        message_id: &str,
        model: dead_letter_message::ActiveModel,
    ) -> Result<dead_letter_message::Model, DbErr> {
        // ON CONFLICT DO NOTHING, NOT a bare insert. A duplicate message_id (broker
        // redelivery / lost-ack - exactly what the DLQ must survive) would raise a
        // unique-constraint violation, and a violation ABORTS the surrounding
        // Postgres transaction; the recovery read that follows would then fail with
        // 25P02 on the aborted txn, so every transactional caller's idempotent
        // retry breaks. `exec_without_returning` with `do_nothing` instead leaves
        // the txn valid and returns `RecordNotInserted` on conflict; either way we
        // read the row back (the one just inserted, or the pre-existing duplicate).
        match dead_letter_message::Entity::insert(model)
            .on_conflict(
                sea_orm::sea_query::OnConflict::column(dead_letter_message::Column::MessageId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec_without_returning(self.conn)
            .await
        {
            Ok(_) | Err(DbErr::RecordNotInserted) => {}
            Err(e) => return Err(e),
        }
        dead_letter_message::Entity::find()
            .filter(dead_letter_message::Column::MessageId.eq(message_id))
            .one(self.conn)
            .await?
            .ok_or_else(|| {
                DbErr::Custom("dead_letter_message row missing after upsert".to_string())
            })
    }

    pub async fn list(
        &self,
        message_type: Option<DlqMessageType>,
        resolved: Option<bool>,
        window: PageWindow,
    ) -> Result<(Vec<dead_letter_message::Model>, u64), DbErr> {
        let mut query = dead_letter_message::Entity::find();

        if let Some(mt) = message_type {
            query = query.filter(dead_letter_message::Column::MessageType.eq(mt.to_string()));
        }

        if let Some(res) = resolved {
            query = query.filter(dead_letter_message::Column::Resolved.eq(res));
        }

        let total = query.clone().count(self.conn).await?;

        let messages = query
            .order_by_desc(dead_letter_message::Column::LastFailedAt)
            .offset(window.offset())
            .limit(window.limit())
            .all(self.conn)
            .await?;

        Ok((messages, total))
    }

    pub async fn get_by_id(&self, id: i32) -> Result<Option<dead_letter_message::Model>, DbErr> {
        dead_letter_message::Entity::find_by_id(id)
            .one(self.conn)
            .await
    }

    pub async fn get_by_id_for_update(
        &self,
        id: i32,
    ) -> Result<Option<dead_letter_message::Model>, DbErr> {
        dead_letter_message::Entity::find_by_id(id)
            .lock(LockType::Update)
            .one(self.conn)
            .await
    }

    /// Records an operator's retry of message `id` in its retry history. The
    /// message stays open until its subject recovers.
    pub async fn record_retry(&self, id: i32, operator_id: i32) -> Result<(), DbErr> {
        self.conn
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                r#"UPDATE dead_letter_message d SET
                       retry_history = d.retry_history || jsonb_build_array(jsonb_build_object(
                           'attempt', jsonb_array_length(d.retry_history) + 1,
                           'error', 'Retried by operator ' || $2::text,
                           'timestamp', NOW())),
                       retry_count = jsonb_array_length(d.retry_history) + 1
                   WHERE d.id = $1"#,
                [id.into(), operator_id.to_string().into()],
            ))
            .await?;
        Ok(())
    }

    /// Resolves `subject`'s message, if it has an open one, on behalf of
    /// operator `resolved_by`.
    pub async fn resolve_subject(
        &self,
        subject: DlqSubject,
        resolved_by: i32,
    ) -> Result<(), DbErr> {
        dead_letter_message::Entity::update_many()
            .col_expr(
                dead_letter_message::Column::Resolved,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                dead_letter_message::Column::ResolvedAt,
                sea_orm::sea_query::Expr::cust("NOW()"),
            )
            .col_expr(
                dead_letter_message::Column::ResolvedBy,
                sea_orm::sea_query::Expr::value(Some(resolved_by)),
            )
            .filter(dead_letter_message::Column::MessageId.eq(subject.message_id()))
            .filter(dead_letter_message::Column::Resolved.eq(false))
            .exec(self.conn)
            .await?;
        Ok(())
    }

    /// Resolves message `id` on behalf of operator `resolved_by`.
    pub async fn resolve(&self, id: i32, resolved_by: i32) -> Result<ResolveResult, DbErr> {
        let update = dead_letter_message::Entity::update_many()
            .col_expr(
                dead_letter_message::Column::Resolved,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                dead_letter_message::Column::ResolvedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .col_expr(
                dead_letter_message::Column::ResolvedBy,
                sea_orm::sea_query::Expr::value(Some(resolved_by)),
            )
            .filter(dead_letter_message::Column::Id.eq(id))
            .filter(dead_letter_message::Column::Resolved.eq(false));

        let update_result = update.exec(self.conn).await?;

        if update_result.rows_affected > 0 {
            return Ok(ResolveResult::Resolved);
        }

        let exists = dead_letter_message::Entity::find_by_id(id)
            .one(self.conn)
            .await?
            .is_some();

        if exists {
            Ok(ResolveResult::AlreadyResolved)
        } else {
            Ok(ResolveResult::NotFound)
        }
    }

    pub async fn stats(&self) -> Result<DlqStats, DbErr> {
        let total_resolved = dead_letter_message::Entity::find()
            .filter(dead_letter_message::Column::Resolved.eq(true))
            .count(self.conn)
            .await?;

        let unresolved_data: Vec<(String, String)> = dead_letter_message::Entity::find()
            .select_only()
            .column(dead_letter_message::Column::MessageType)
            .column(dead_letter_message::Column::ErrorCode)
            .filter(dead_letter_message::Column::Resolved.eq(false))
            .into_tuple()
            .all(self.conn)
            .await?;

        let total_unresolved = unresolved_data.len() as u64;
        let mut operation_task_count = 0u64;
        let mut stuck_submission_count = 0u64;
        let mut stuck_code_run_count = 0u64;
        let mut stuck_submission_judgement_count = 0u64;
        let mut unresolved_by_error_code: HashMap<String, u64> = HashMap::new();

        for (message_type, error_code) in unresolved_data {
            match message_type.as_str() {
                "operation_task" => operation_task_count += 1,
                "stuck_submission" => stuck_submission_count += 1,
                "stuck_code_run" => stuck_code_run_count += 1,
                "stuck_submission_judgement" => stuck_submission_judgement_count += 1,
                _ => {}
            }
            *unresolved_by_error_code.entry(error_code).or_insert(0) += 1;
        }

        Ok(DlqStats {
            total_unresolved,
            total_resolved,
            operation_task_count,
            stuck_submission_count,
            stuck_code_run_count,
            stuck_submission_judgement_count,
            unresolved_by_error_code,
        })
    }

    /// Resolves messages `ids` on behalf of operator `resolved_by`.
    pub async fn resolve_many(&self, ids: &[i32], resolved_by: i32) -> Result<u64, DbErr> {
        let result = dead_letter_message::Entity::update_many()
            .col_expr(
                dead_letter_message::Column::Resolved,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                dead_letter_message::Column::ResolvedAt,
                sea_orm::sea_query::Expr::value(Utc::now()),
            )
            .col_expr(
                dead_letter_message::Column::ResolvedBy,
                sea_orm::sea_query::Expr::value(Some(resolved_by)),
            )
            .filter(dead_letter_message::Column::Id.is_in(ids.to_vec()))
            .filter(dead_letter_message::Column::Resolved.eq(false))
            .exec(self.conn)
            .await?;

        Ok(result.rows_affected)
    }
}

pub fn dlq_service(db: &DatabaseConnection) -> DlqService<'_, DatabaseConnection> {
    DlqService::new(db)
}
