use std::sync::Arc;
use std::time::Duration;

use common::DlqEnvelope;
use mq::{BrokerMessage, Mq};
use sea_orm::{DatabaseConnection, TransactionTrait};
use tracing::{error, info, warn};

use crate::dlq::DlqService;

/// Retries before a DLQ envelope that cannot be persisted is finally rejected.
/// The `operation_dlq` queue is already a dead-letter queue, so rejecting a
/// message here dead-letters it to a secondary, never-drained queue -- i.e.
/// permanent silent loss of a failed-operation record. A transient DB error
/// (failover, brief network blip) must therefore be absorbed by retrying,
/// not by giving up. The backoff caps at 30s, so these attempts span several
/// minutes of unavailability before the last-resort reject.
const MAX_PERSIST_ATTEMPTS: u32 = 10;

async fn persist_operation_dlq(
    db: &DatabaseConnection,
    envelope: &DlqEnvelope,
) -> anyhow::Result<()> {
    let txn = db.begin().await?;
    DlqService::new(&txn).send_to_dlq(envelope).await?;
    txn.commit().await?;
    Ok(())
}

pub async fn consume_operation_dlq(db: DatabaseConnection, mq: Arc<Mq>, queue_name: String) {
    info!(queue = %queue_name, "Starting operation DLQ consumer");

    let result = mq
        // Concurrency `Some(1)`, NOT `None`: the `None` code path in
        // `process_messages` propagates a transient consume error out with `?`,
        // which returns from this function and - since the caller only
        // `spawn`s it with no reconnect loop - permanently halts DLQ
        // persistence (silent failed-job loss) on a single Redis hiccup. The
        // `Some(n)` path instead loops and retries the consume forever, the
        // same self-healing the result and worker consumers rely on. `1` keeps
        // DLQ handling sequential, matching the previous behaviour.
        .process_messages(
            &queue_name,
            Some(1),
            None,
            move |message: BrokerMessage<DlqEnvelope>| {
                let db = db.clone();
                async move {
                    crate::consumers::guard_handler("operation_dlq", async move {
                        let envelope = message.payload;
                        let message_id = envelope.message_id.clone();

                        // Retry the whole persist txn on any DB error: rejecting
                        // this message would dead-letter it to a never-drained
                        // queue, so a transient DB error must not lose the
                        // envelope. Only give up after a generous window.
                        let mut attempt = 0u32;
                        loop {
                            let persisted = common::deadline::bounded(
                                "operation_dlq.persist",
                                common::deadline::BACKGROUND_STEP,
                                persist_operation_dlq(&db, &envelope),
                            )
                            .await
                            .unwrap_or_else(|| Err(anyhow::anyhow!("persist timed out")));
                            match persisted {
                                Ok(()) => {
                                    info!(
                                        message_id = %message_id,
                                        error_code = ?envelope.error_code,
                                        "Persisted operation DLQ envelope"
                                    );
                                    return Ok(());
                                }
                                Err(e) if attempt + 1 < MAX_PERSIST_ATTEMPTS => {
                                    attempt += 1;
                                    let backoff = Duration::from_secs(
                                        (1u64 << attempt.min(5)).min(30),
                                    );
                                    warn!(
                                        message_id = %message_id,
                                        attempt,
                                        error = %e,
                                        "Operation DLQ persistence failed; retrying after backoff"
                                    );
                                    tokio::time::sleep(backoff).await;
                                }
                                Err(e) => {
                                    error!(
                                        message_id = %message_id,
                                        error = %e,
                                        "Operation DLQ persistence failed after all retries; rejecting"
                                    );
                                    return Err(mq::BroccoliError::Job(format!(
                                        "DB persistence failed after {MAX_PERSIST_ATTEMPTS} attempts: {e}"
                                    )));
                                }
                            }
                        }
                    })
                    .await
                }
            },
        )
        .await;

    if let Err(e) = result {
        error!(error = %e, "Operation DLQ consumer stopped unexpectedly");
    }
}
