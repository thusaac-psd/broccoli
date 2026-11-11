pub mod operation_dlq;
pub mod operation_result;

pub use operation_dlq::consume_operation_dlq;
pub use operation_result::consume_operation_results;

use broccoli_server_sdk::types::sanitize_text_field;
use common::SubmissionStatus;
use futures::FutureExt;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

/// Run a `process_messages` handler future under a panic guard.
///
/// The vendored `broccoli_queue::process_messages(Some(N))` spawns N detached
/// consume tasks into a `FuturesUnordered` that it NEVER repolls, so a task that
/// panics is not drained and its slot is never refilled: one panicking handler
/// permanently removes a consume slot. For the DLQ consumer (`Some(1)`) that
/// silently halts all failed-job persistence; for the result consumer it quietly
/// erodes throughput. Catching the panic here keeps the consume task alive.
///
/// A caught panic returns `Ok(())` (a clean acknowledge), NOT `Err`: a panic is
/// almost always deterministic for the message, so rejecting it would only burn
/// the retry budget re-panicking before it lands in a never-drained failed
/// queue. Acknowledging drops the poison message without wedging or leaking it.
/// The panic is logged loudly.
pub(crate) async fn guard_handler<F>(handler: &'static str, fut: F) -> Result<(), mq::BroccoliError>
where
    F: std::future::Future<Output = Result<(), mq::BroccoliError>>,
{
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(result) => result,
        Err(_panic) => {
            tracing::error!(
                handler,
                "Consumer handler panicked; message acknowledged and dropped, consumer kept alive"
            );
            Ok(())
        }
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
    async fn passes_ok_and_err_through_unchanged() {
        assert!(guard_handler("t", async { Ok(()) }).await.is_ok());
        assert!(
            guard_handler("t", async { Err(mq::BroccoliError::Job("boom".into())) })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn converts_a_handler_panic_into_a_clean_ack() {
        let result = guard_handler("t", async {
            panic!("handler blew up");
            #[allow(unreachable_code)]
            Ok::<(), mq::BroccoliError>(())
        })
        .await;
        assert!(
            result.is_ok(),
            "a panicking handler must be caught and acknowledged, not propagated"
        );
    }
}
