pub mod operation_dlq;
pub mod operation_result;

pub use operation_dlq::consume_operation_dlq;
pub use operation_result::consume_operation_results;

use broccoli_server_sdk::types::sanitize_text_field;
use common::SubmissionStatus;
use futures::FutureExt;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

/// Run one message's handler under a panic guard, so a handler that panics
/// on one message does not end the consumer loop for the life of the process.
/// A panic is logged and the message is left as the handler left it.
pub(crate) async fn guard_handler<F>(handler: &'static str, fut: F)
where
    F: std::future::Future<Output = ()>,
{
    if std::panic::AssertUnwindSafe(fut)
        .catch_unwind()
        .await
        .is_err()
    {
        tracing::error!(
            handler,
            "Consumer handler panicked; the consumer keeps running"
        );
    }
}

pub async fn mark_submission_system_error<C: ConnectionTrait>(
    conn: &C,
    submission_id: i32,
    error_code: &str,
    error_message: &str,
) -> anyhow::Result<()> {
    mark_submission_system_error_with_epoch(conn, submission_id, error_code, error_message, None)
        .await
}

pub async fn mark_submission_system_error_with_epoch<C: ConnectionTrait>(
    conn: &C,
    submission_id: i32,
    error_code: &str,
    error_message: &str,
    judge_epoch: Option<i32>,
) -> anyhow::Result<()> {
    let safe_code = sanitize_text_field(error_code);
    let safe_message = sanitize_text_field(error_message);
    let (sql, values) = if let Some(epoch) = judge_epoch {
        (
            r#"UPDATE submission SET status = $1, error_code = $2, error_message = $3
               WHERE id = $4 AND judge_epoch = $5
                 AND status NOT IN ('Judged', 'CompilationError', 'SystemError')"#,
            vec![
                SubmissionStatus::SystemError.to_string().into(),
                safe_code.as_ref().to_string().into(),
                safe_message.as_ref().to_string().into(),
                submission_id.into(),
                epoch.into(),
            ],
        )
    } else {
        (
            r#"UPDATE submission SET status = $1, error_code = $2, error_message = $3
               WHERE id = $4"#,
            vec![
                SubmissionStatus::SystemError.to_string().into(),
                safe_code.as_ref().to_string().into(),
                safe_message.as_ref().to_string().into(),
                submission_id.into(),
            ],
        )
    };

    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .await?;

    // Mirror the system-error onto the submission's current judgement so
    // the versioned row stays in sync with the denormalized cache. Best
    // effort: a missing judgement (legacy submissions before backfill)
    // simply matches zero rows. The epoch guard mirrors the submission
    // update so a stale call from a retried worker does not flip a
    // judgement that has already been re-dispatched.
    let safe_code = sanitize_text_field(error_code);
    let safe_message = sanitize_text_field(error_message);
    let (jsql, jvalues) = if let Some(epoch) = judge_epoch {
        (
            r#"UPDATE submission_judgement
               SET status = $1, error_code = $2, error_message = $3,
                   is_finalized = TRUE, finalized_at = NOW()
               WHERE submission_id = $4 AND is_current = TRUE AND is_finalized = FALSE
                 AND judge_epoch = $5"#,
            vec![
                SubmissionStatus::SystemError.to_string().into(),
                safe_code.as_ref().to_string().into(),
                safe_message.as_ref().to_string().into(),
                submission_id.into(),
                epoch.into(),
            ],
        )
    } else {
        (
            r#"UPDATE submission_judgement
               SET status = $1, error_code = $2, error_message = $3,
                   is_finalized = TRUE, finalized_at = NOW()
               WHERE submission_id = $4 AND is_current = TRUE AND is_finalized = FALSE"#,
            vec![
                SubmissionStatus::SystemError.to_string().into(),
                safe_code.as_ref().to_string().into(),
                safe_message.as_ref().to_string().into(),
                submission_id.into(),
            ],
        )
    };
    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        jsql,
        jvalues,
    ))
    .await?;
    Ok(())
}

pub async fn mark_code_run_system_error<C: ConnectionTrait>(
    conn: &C,
    code_run_id: i32,
    error_code: &str,
    error_message: &str,
) -> anyhow::Result<()> {
    mark_code_run_system_error_with_epoch(conn, code_run_id, error_code, error_message, None).await
}

pub async fn mark_code_run_system_error_with_epoch<C: ConnectionTrait>(
    conn: &C,
    code_run_id: i32,
    error_code: &str,
    error_message: &str,
    judge_epoch: Option<i32>,
) -> anyhow::Result<()> {
    let safe_code = sanitize_text_field(error_code);
    let safe_message = sanitize_text_field(error_message);
    let (sql, values) = if let Some(epoch) = judge_epoch {
        (
            r#"UPDATE code_run SET status = $1, error_code = $2, error_message = $3
               WHERE id = $4 AND judge_epoch = $5
                 AND status NOT IN ('Judged', 'CompilationError', 'SystemError')"#,
            vec![
                SubmissionStatus::SystemError.to_string().into(),
                safe_code.as_ref().to_string().into(),
                safe_message.as_ref().to_string().into(),
                code_run_id.into(),
                epoch.into(),
            ],
        )
    } else {
        (
            r#"UPDATE code_run SET status = $1, error_code = $2, error_message = $3
               WHERE id = $4"#,
            vec![
                SubmissionStatus::SystemError.to_string().into(),
                safe_code.as_ref().to_string().into(),
                safe_message.as_ref().to_string().into(),
                code_run_id.into(),
            ],
        )
    };

    conn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        values,
    ))
    .await?;
    Ok(())
}

#[cfg(test)]
mod guard_tests {
    use super::guard_handler;

    #[tokio::test]
    async fn a_panicking_handler_does_not_propagate() {
        guard_handler("t", async { panic!("handler blew up") }).await;
    }
}
