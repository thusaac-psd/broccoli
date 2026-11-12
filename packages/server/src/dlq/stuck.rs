//! Stuck-job detector: the safety net behind the lease steal. It requeues
//! in-flight work with no heartbeat for `stuck_job_timeout_secs` or
//! dispatched more than `max_inflight_secs` ago (which also catches a wedged
//! job whose live server keeps its lease fresh), and finalizes work whose
//! dispatch-retry budget is spent as `SystemError` with a DLQ entry. Every
//! age is measured on the database clock. See
//! [`crate::services::judging::reclaim_abandoned`].

use std::time::Duration;

use common::{DlqConfig, StatusPhase};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use tracing::{error, info, warn};

use crate::services::judging::{self, Abandoned, RetryBudget, Unit};
use crate::state::AppState;

/// Queued work older than this is reported as a dispatcher-health signal. It
/// is left alone: a deep durable queue is backlog, not a failure.
const QUEUED_OBSERVABILITY_THRESHOLD_SECS: u64 = 5 * 60;
/// Units each detection pass takes over per unit kind.
const DETECT_BATCH_SIZE: u32 = 256;

pub async fn run_stuck_job_detector(state: AppState, config: DlqConfig) {
    info!(
        timeout_secs = config.stuck_job_timeout_secs,
        scan_interval_secs = config.stuck_job_scan_interval_secs,
        max_inflight_secs = config.max_inflight_secs,
        "Starting stuck job detector"
    );

    let mut interval = tokio::time::interval(Duration::from_secs(
        config.stuck_job_scan_interval_secs.max(1),
    ));
    loop {
        interval.tick().await;
        let scan = detect_once(&state, &config);
        if let Some(Err(e)) = common::deadline::bounded(
            "dlq.stuck_job_detector",
            common::deadline::BACKGROUND_STEP,
            scan,
        )
        .await
        {
            error!(error = %e, "Stuck job detection failed");
        }
    }
}

/// One detection pass, as the detector loop runs it. `pub` so integration
/// tests can drive it directly.
pub async fn detect_once(state: &AppState, config: &DlqConfig) -> anyhow::Result<()> {
    observe_old_queued_backlog(&state.db).await?;
    let abandoned = Abandoned::Stuck {
        timeout_secs: config.stuck_job_timeout_secs,
        max_inflight_secs: (config.max_inflight_secs != 0).then_some(config.max_inflight_secs),
    };
    let budget = RetryBudget::new(state.config.server.max_dispatch_retries);
    // Recovery requeues for the claim fiber, which leaves work to a replica
    // that can judge it; selecting the same way keeps a replica from burning
    // retries on work it cannot judge.
    let local = crate::dispatcher::local_contest_types(state).await;
    for unit in [Unit::Judgement, Unit::CodeRun] {
        let reclaimed = judging::reclaim_abandoned(
            &state.db,
            unit,
            abandoned,
            budget,
            DETECT_BATCH_SIZE,
            &local,
        )
        .await?;
        if reclaimed.requeued > 0 || reclaimed.exhausted > 0 {
            warn!(
                ?unit,
                requeued = reclaimed.requeued,
                exhausted = reclaimed.exhausted,
                "Took over stuck work"
            );
        }
    }
    Ok(())
}

async fn observe_old_queued_backlog(db: &DatabaseConnection) -> anyhow::Result<()> {
    for unit in [Unit::Judgement, Unit::CodeRun] {
        let table = match unit {
            Unit::Judgement => "submission_judgement",
            Unit::CodeRun => "code_run",
        };
        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT COUNT(*)::bigint AS n FROM {table} \
                     WHERE status IN {queued} \
                       AND queued_at < NOW() - make_interval(secs => $1)",
                    queued = StatusPhase::Queued.sql_list(),
                ),
                [(QUEUED_OBSERVABILITY_THRESHOLD_SECS as f64).into()],
            ))
            .await?;
        let count: i64 = row.map(|r| r.try_get("", "n")).transpose()?.unwrap_or(0);
        if count > 0 {
            warn!(
                ?unit,
                count,
                threshold_secs = QUEUED_OBSERVABILITY_THRESHOLD_SECS,
                "Queued work is older than the dispatcher observability threshold; leaving backlog intact"
            );
        }
    }
    Ok(())
}
