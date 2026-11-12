//! SystemError-retry reaper.
//!
//! A `SystemError` reflects a system-side condition (contention, a lost
//! operation result, a dispatch failure, a stuck job), never the
//! contestant's own code, so it must not stand as the contestant's verdict.
//! This fiber reconciles from the durable terminal state, whatever path
//! finalized it: it requeues current judgements that finalized as a
//! `SystemError` in any shape while their submission has system-error
//! retries left (`max_system_error_retries`), and the claim fiber judges them
//! again. Only when that budget is spent does the `SystemError` stand. See
//! [`crate::services::judging::retry_system_errors`].

use std::time::Duration;

use tokio::sync::watch;
use tracing::{error, info};

use crate::services::judging::{self, RetryBudget};
use crate::state::AppState;

/// How often to scan for retryable SystemError verdicts. Kept short so a
/// transient failure recovers quickly; the scan is a cheap indexed query.
const SCAN_INTERVAL_SECS: u64 = 5;
/// Maximum judgements requeued per scan. Deliberately small, so a backlog of
/// retries drains a few per tick instead of crowding out live submissions in
/// the claim queue.
const SCAN_BATCH_SIZE: u32 = 8;

pub async fn run(
    state: AppState,
    server_id: String,
    max_system_error_retries: u32,
    mut cancel: watch::Receiver<bool>,
) {
    let budget = RetryBudget::new(max_system_error_retries);
    let mut interval = tokio::time::interval(Duration::from_secs(SCAN_INTERVAL_SECS));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let scan = scan_once(&state, budget);
                match common::deadline::bounded("dispatcher.system_error_retry", common::deadline::BACKGROUND_STEP, scan).await {
                    Some(Ok(0)) | None => {}
                    Some(Ok(requeued)) => info!(server_id = %server_id, requeued, "Requeued SystemError verdicts for a bounded retry"),
                    Some(Err(e)) => error!(server_id = %server_id, error = %e, "SystemError-retry scan failed"),
                }
            }
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return;
                }
            }
        }
    }
}

/// One scan. Rejudging on a replica without a handler for the contest type
/// would only fail again, so a judgement is left to a replica that has one
/// (see `dispatcher::prefer_local_handler`).
async fn scan_once(state: &AppState, budget: RetryBudget) -> anyhow::Result<u64> {
    let local = super::local_contest_types(state).await;
    Ok(judging::retry_system_errors(&state.db, budget, SCAN_BATCH_SIZE, &local).await?)
}
