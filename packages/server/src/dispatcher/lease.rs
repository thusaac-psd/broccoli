use std::time::Duration;

use common::SubmissionStatus;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement, sea_query::Expr,
};
use tokio::sync::watch;
use tracing::{error, info};

use crate::entity::{code_run, submission};

const LEASED_STATUSES: [SubmissionStatus; 3] = [
    SubmissionStatus::Pending,
    SubmissionStatus::Compiling,
    SubmissionStatus::Running,
];

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
                match common::deadline::bounded("dispatcher.lease", common::deadline::BACKGROUND_STEP, refresh_owned_leases(&db, &server_id)).await {
                    Some(Ok((submissions, code_runs, judgements))) => {
                        if submissions > 0 || code_runs > 0 || judgements > 0 {
                            info!(server_id = %server_id, submissions, code_runs, judgements, "Refreshed owned judging leases");
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

pub async fn refresh_owned_leases(
    db: &DatabaseConnection,
    server_id: &str,
) -> Result<(u64, u64, u64), sea_orm::DbErr> {
    let now = chrono::Utc::now();

    let submissions = submission::Entity::update_many()
        .col_expr(submission::Column::LeaseHeartbeatAt, Expr::value(Some(now)))
        .filter(submission::Column::OwnerServerId.eq(server_id))
        .filter(submission::Column::Status.is_in(LEASED_STATUSES))
        .exec(db)
        .await?
        .rows_affected;

    let code_runs = code_run::Entity::update_many()
        .col_expr(code_run::Column::LeaseHeartbeatAt, Expr::value(Some(now)))
        .filter(code_run::Column::OwnerServerId.eq(server_id))
        .filter(code_run::Column::Status.is_in(LEASED_STATUSES))
        .exec(db)
        .await?
        .rows_affected;

    let judgements = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"
            UPDATE submission_judgement j
               SET lease_heartbeat_at = $1
             WHERE j.owner_server_id = $2
               AND j.is_current = TRUE
               AND j.is_finalized = FALSE
               AND j.status IN ('Pending', 'Compiling', 'Running')
               AND j.version = (
                   SELECT MAX(j2.version)
                     FROM submission_judgement j2
                    WHERE j2.submission_id = j.submission_id
               )
            "#,
            [now.into(), server_id.into()],
        ))
        .await?
        .rows_affected();

    Ok((submissions, code_runs, judgements))
}
