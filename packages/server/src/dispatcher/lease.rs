//! Lease-refresh fiber: keeps the lease of every in-flight unit this server
//! owns fresh, so the steal scanner leaves its work alone.

use std::time::Duration;

use common::db::DatabaseConnection;
use tokio::sync::watch;
use tracing::{error, info};

use crate::services::judging;

pub async fn run(
    db: DatabaseConnection,
    server_id: String,
    interval_secs: u64,
    mut cancel: watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let refresh = judging::refresh_leases(&db, &server_id);
                match common::deadline::bounded("dispatcher.lease", common::deadline::BACKGROUND_STEP, refresh).await {
                    Some(Ok(counts)) => {
                        if counts.judgements > 0 || counts.code_runs > 0 {
                            info!(server_id = %server_id, judgements = counts.judgements, code_runs = counts.code_runs, "Refreshed owned judging leases");
                        }
                    }
                    Some(Err(e)) => error!(server_id = %server_id, error = %e, "Lease refresh failed"),
                    None => {}
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
