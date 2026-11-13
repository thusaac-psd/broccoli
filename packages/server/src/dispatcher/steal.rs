//! Lease-steal scanner: requeues in-flight work whose owner stopped
//! refreshing its lease, so the claim fiber dispatches it again. Runs only
//! with lease/steal on, when every live server refreshes its leases.

use std::time::Duration;

use tokio::sync::watch;
use tracing::{error, info};

use crate::services::judging::{self, Abandoned, RetryBudget, Unit};
use crate::state::AppState;

pub async fn run(
    state: AppState,
    server_id: String,
    lease_ttl_secs: u64,
    interval_secs: u64,
    batch_size: u32,
    max_dispatch_retries: u32,
    mut cancel: watch::Receiver<bool>,
) {
    let abandoned = Abandoned::LeaseLapsed { lease_ttl_secs };
    let budget = RetryBudget::new(max_dispatch_retries);
    let mut interval = tokio::time::interval(Duration::from_secs(std::cmp::max(interval_secs, 1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let scan = scan_once(&state, abandoned, budget, batch_size);
                if let Some(Err(e)) = common::deadline::bounded("dispatcher.steal", common::deadline::BACKGROUND_STEP, scan).await {
                    error!(server_id = %server_id, error = %e, "Steal scan failed");
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

/// One scan over both units. Leaves work to a replica that can judge it
/// (see `dispatcher::prefer_local_handler`).
async fn scan_once(
    state: &AppState,
    abandoned: Abandoned,
    budget: RetryBudget,
    batch_size: u32,
) -> anyhow::Result<()> {
    let local = super::local_contest_types(state).await;
    for unit in [Unit::Judgement, Unit::CodeRun] {
        let reclaimed =
            judging::reclaim_abandoned(&state.db, unit, abandoned, budget, batch_size, &local)
                .await?;
        if reclaimed.requeued > 0 || reclaimed.exhausted > 0 {
            info!(
                ?unit,
                requeued = reclaimed.requeued,
                exhausted = reclaimed.exhausted,
                "Took over work whose lease lapsed"
            );
        }
    }
    Ok(())
}
