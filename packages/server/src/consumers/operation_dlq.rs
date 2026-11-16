use std::time::{Duration, Instant};

use common::DlqEnvelope;
use mq::{Delivery, GroupConsumer};
use sea_orm::{DatabaseConnection, TransactionTrait};
use tracing::{error, info, warn};

use crate::dlq::DlqService;

/// Entries read per batch.
const READ_BATCH: usize = 16;
/// Deliveries after which a dead letter that still cannot be written is
/// dropped: its write fails deterministically (a value Postgres rejects) or
/// panics. Reclaims come every [`mq::CLAIM_IDLE`], so this rides out about ten
/// minutes of database outage first. The envelope is logged in full, so it
/// can still be recovered by hand.
const MAX_PERSIST_DELIVERIES: u32 = 20;

/// Deadline of one persist. Under [`mq::CLAIM_IDLE`], so an entry is not
/// reclaimed while it is being written; a write that does outlast it is
/// harmless because `send_to_dlq` is idempotent on `message_id`.
const PERSIST_DEADLINE: Duration = Duration::from_secs(20);

async fn persist_operation_dlq(
    db: &DatabaseConnection,
    envelope: &DlqEnvelope,
) -> anyhow::Result<()> {
    let txn = db.begin().await?;
    DlqService::new(&txn).send_to_dlq(envelope).await?;
    txn.commit().await?;
    Ok(())
}

/// Write dead letters from the queue into `dead_letter_message`, for the life
/// of the process. An entry is acknowledged only once it is written; one whose
/// write fails stays pending and is reclaimed by any server after
/// [`mq::CLAIM_IDLE`], so a database outage delays dead letters without losing
/// them.
pub async fn consume_operation_dlq(db: DatabaseConnection, consumer: GroupConsumer<DlqEnvelope>) {
    info!(stream = %consumer.streams()[0], "Starting operation DLQ consumer");
    let mut reader = consumer.reader();
    let mut next_reclaim = Instant::now();
    loop {
        let batch = if Instant::now() >= next_reclaim {
            next_reclaim = Instant::now() + mq::REFRESH_INTERVAL;
            consumer.reclaim(READ_BATCH).await
        } else {
            reader.read(READ_BATCH).await
        };
        let batch = match batch {
            Ok(batch) => batch,
            Err(e) => {
                warn!(error = %e, "Reading dead letters failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        for delivery in batch {
            crate::consumers::guard_handler("operation_dlq", handle(&db, &consumer, delivery))
                .await;
        }
    }
}

async fn handle(
    db: &DatabaseConnection,
    consumer: &GroupConsumer<DlqEnvelope>,
    delivery: Delivery<DlqEnvelope>,
) {
    if delivery.deliveries > MAX_PERSIST_DELIVERIES {
        error!(
            deliveries = delivery.deliveries,
            envelope = %delivery.raw_payload(),
            "Dropping a dead letter that could not be written {MAX_PERSIST_DELIVERIES} times"
        );
        ack(consumer, &delivery).await;
        return;
    }
    let envelope = match &delivery.payload {
        Ok(envelope) => envelope,
        Err(e) => {
            // No consumer can read it either: drop it, loudly.
            error!(
                error = %e,
                payload = %e.raw,
                "Dropping an undecodable dead letter"
            );
            ack(consumer, &delivery).await;
            return;
        }
    };
    let persisted = common::deadline::bounded(
        "operation_dlq.persist",
        PERSIST_DEADLINE,
        persist_operation_dlq(db, envelope),
    )
    .await
    .unwrap_or_else(|| Err(anyhow::anyhow!("persist timed out")));
    match persisted {
        Ok(()) => {
            info!(
                message_id = %envelope.message_id,
                error_code = ?envelope.error_code,
                "Persisted operation DLQ envelope"
            );
            ack(consumer, &delivery).await;
        }
        Err(e) => warn!(
            message_id = %envelope.message_id,
            deliveries = delivery.deliveries,
            error = %e,
            "Persisting a dead letter failed; it is retried once it is reclaimed"
        ),
    }
}

async fn ack(consumer: &GroupConsumer<DlqEnvelope>, delivery: &Delivery<DlqEnvelope>) {
    match consumer.ack(&delivery.entry).await {
        Ok(mq::Settled::Done) => {}
        Ok(mq::Settled::HandedOver) => {
            warn!("Another server took this dead letter over while it was written")
        }
        Err(e) => warn!(error = %e, "Acknowledging a dead letter failed; it will be written again"),
    }
}
