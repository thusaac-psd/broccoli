pub mod claim;
pub mod fanout;
pub mod lease;
pub mod operation_reaper;
pub mod permits;
pub mod plugin_timer;
pub mod queue_depth;
pub mod steal;
pub mod sweeper;
pub mod system_error_retry;

use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::info;

use crate::config::ServerConfig;
use crate::state::AppState;

/// The `status = 'Queued'` predicate as an inline SQL literal.
///
/// The partial `idx_<table>_queued` indexes (migration
/// `m0014_queued_partial_indexes`) are only usable when the planner can prove
/// the query's predicate implies the index's, which a bound `$n` parameter
/// does not allow once Postgres switches a prepared statement to its generic
/// plan. Every scan over the `Queued` rows therefore filters with this literal
/// instead of a bound status value.
pub(crate) const QUEUED_PREDICATE: &str = "status = 'Queued'";

/// [`QUEUED_PREDICATE`] as a SeaORM filter.
pub(crate) fn is_queued() -> sea_orm::sea_query::SimpleExpr {
    sea_orm::sea_query::Expr::cust(QUEUED_PREDICATE)
}

/// The contest types this replica has a handler for right now, sorted.
pub(crate) async fn local_contest_types(state: &AppState) -> Vec<String> {
    let mut types: Vec<String> = state
        .registries
        .contest_type_registry
        .read()
        .await
        .keys()
        .cloned()
        .collect();
    types.sort();
    types
}

/// SQL predicate: whether this replica should take judging work whose
/// contest type is the SQL expression `contest_type`. It should when it has a
/// handler for that type (`local_types`, a placeholder bound to
/// [`local_contest_types`]), or when no live replica does: then whoever takes
/// it fails it visibly with `NO_HANDLER_REGISTERED` instead of it waiting
/// forever.
///
/// Replicas do not always agree on which plugins are loaded (one is still
/// converging onto an enable, or failed to load a plugin its peers run
/// fine), and a replica that takes work it cannot judge only fails it and
/// burns its retry budget. Each replica publishes the types it handles in its
/// `plugin_replica` row (see [`crate::plugin_fleet`]); liveness is that row's
/// heartbeat against the database clock, so no server clock is involved.
/// Shared by every select that hands out judging work: the claim fiber, the
/// steal scans, and the SystemError retry.
pub(crate) fn prefer_local_handler(contest_type: &str, local_types: &str) -> String {
    format!(
        "({contest_type} = ANY({local_types}) OR NOT EXISTS (\
           SELECT 1 FROM plugin_replica r \
            WHERE r.heartbeat_at > NOW() - make_interval(secs => {live}) \
              AND r.contest_types @> jsonb_build_array({contest_type})))",
        live = crate::plugin_fleet::LIVE_WINDOW_SECS,
    )
}

/// [`prefer_local_handler`] for a row of `table`: `submission` and
/// `code_run` rows carry their contest type; a judgement takes its parent
/// submission's.
pub(crate) fn prefer_local_handler_for(table: &str, local_types: &str) -> String {
    if table == "submission_judgement" {
        format!(
            "EXISTS (SELECT 1 FROM submission s \
                      WHERE s.id = submission_judgement.submission_id \
                        AND {})",
            prefer_local_handler("s.contest_type", local_types)
        )
    } else {
        prefer_local_handler(&format!("{table}.contest_type"), local_types)
    }
}

pub struct DispatcherDeps {
    pub state: AppState,
    pub redis_client: Option<redis::Client>,
    pub server_id: String,
    pub operation_result_queue_base: String,
    pub config: ServerConfig,
}

pub struct Dispatcher {
    cancel: Option<watch::Sender<bool>>,
    handles: Vec<JoinHandle<()>>,
}

impl Dispatcher {
    pub fn spawn(deps: DispatcherDeps) -> Self {
        // The claim fiber is **independent** of the lease/steal
        // toggle: it implements the durable-accept path that
        // `Queued`-on-POST relies on, and turning it off strands every
        // row the POST handlers write. We therefore spawn it as long as
        // `claim_fiber_enabled` is set, regardless of the lease/steal
        // master switch.
        let lease_steal_enabled = deps.config.dispatcher_lease_steal_enabled;
        let claim_enabled = deps.config.claim_fiber_enabled;

        let (cancel_tx, cancel_rx) = watch::channel(false);
        let mut handles = Vec::new();

        // The plugin-timer delivery loop is **independent** of both the claim
        // fiber and the lease/steal toggle: those govern submission judging,
        // this delivers plugin-scheduled `[[server.timers]]` callbacks. It is
        // therefore spawned unconditionally and BEFORE the early-return below
        // — a deployment that disables lease/steal and the claim fiber still
        // wants its plugins' timers to fire.
        handles.push(tokio::spawn(plugin_timer::run(
            deps.state.clone(),
            plugin_timer::TimerConfig {
                tick_interval_secs: deps.config.plugin_timer_tick_interval_secs,
                lease_secs: deps.config.plugin_timer_lease_secs,
                batch: deps.config.plugin_timer_batch,
                max_attempts: deps.config.plugin_timer_max_attempts,
                call_budget: deps.state.plugins.get_config().call_timeout(),
            },
            cancel_rx.clone(),
        )));

        if !lease_steal_enabled && !claim_enabled {
            info!(
                "Dispatcher submission-judging fibers fully disabled by config (lease/steal \
                 off, claim fiber off). Submissions written with status='Queued' will \
                 accumulate until the claim fiber is re-enabled. The plugin-timer delivery \
                 loop still runs regardless of this switch."
            );
            return Self {
                cancel: Some(cancel_tx),
                handles,
            };
        }

        // Capture the operation-reaper inputs before the lease/steal block below
        // conditionally moves `deps.state` / `deps.redis_client`. The reaper is
        // independent of the lease/steal toggle (that governs submission judging,
        // not the operation MQ) but needs both a Redis client and an MQ handle.
        let reaper_redis = deps.redis_client.clone();
        let reaper_mq = deps.state.mq.clone();
        let reaper_shared_queue = deps.state.config.mq.operation_queue_name.clone();
        let reaper_cancel = cancel_rx.clone();

        if claim_enabled {
            // Fail loudly at startup on a 0 batch-size rather than logging
            // a warning per poll-tick forever. Operators who want the fiber
            // off should set `server.claim_fiber_enabled = false`; a 0
            // batch is almost always a typo on a different knob.
            assert!(
                deps.config.claim_batch_size > 0,
                "server.claim_batch_size must be > 0 when claim_fiber_enabled is true; \
                 set claim_fiber_enabled=false to stop the fiber instead"
            );
            handles.push(tokio::spawn(claim::run(
                deps.state.clone(),
                deps.server_id.clone(),
                deps.config.claim_poll_interval_ms,
                deps.config.claim_batch_size,
                deps.config.max_in_flight_submissions,
                cancel_rx.clone(),
            )));
        } else {
            info!(
                "Claim fiber disabled by config. Queued rows will not be promoted \
                 to Pending until the fiber is re-enabled; this is intended only as an \
                 incident-response escape hatch."
            );
        }

        // SystemError-retry reaper: bounded re-judge of plugin-finalized
        // SystemError verdicts (a system condition, never the contestant's
        // code), path-agnostic across batch + interactive judging.
        //
        // Deliberately NOT behind the lease/steal toggle, which it used to share.
        // That toggle defaults off and is `false` in the shipped release env
        // files, so the reaper never ran in the configuration people deploy:
        // every SystemError stood as the contestant's verdict, contradicting
        // this fiber's own invariant that none is ever abandoned. Measured on
        // a real 4-worker stack: 62 SystemError verdicts under a concurrent
        // burst, zero re-judged.
        //
        // It does not need the lease/steal fibers. It writes lease columns on
        // requeue only so a running steal leaves a re-judging row alone; with
        // steal off those columns are inert. Its re-dispatch is a direct
        // `dispatch_submission_to_plugin_with_judgement` call, and every write
        // is epoch-gated, so concurrent replicas cannot double-requeue.
        handles.push(tokio::spawn(system_error_retry::run(
            deps.state.clone(),
            deps.server_id.clone(),
            deps.config.max_system_error_retries,
            cancel_rx.clone(),
        )));

        if lease_steal_enabled {
            handles.push(tokio::spawn(lease::run(
                deps.state.db.clone(),
                deps.server_id.clone(),
                deps.config.lease_refresh_interval_secs,
                cancel_rx.clone(),
            )));

            handles.push(tokio::spawn(steal::run(
                deps.state.clone(),
                deps.server_id.clone(),
                steal::Reclaim::Stale {
                    lease_ttl_secs: deps.config.lease_ttl_secs,
                },
                deps.config.steal_scan_interval_secs,
                deps.config.steal_batch_size,
                deps.config.max_dispatch_retries,
                cancel_rx.clone(),
            )));

            if let Some(redis_client) = deps.redis_client {
                handles.push(tokio::spawn(sweeper::run(
                    redis_client,
                    deps.operation_result_queue_base,
                    deps.config.sweep_interval_secs,
                    deps.config.sweeper_dry_run,
                    cancel_rx,
                )));
            } else {
                info!("Reply-queue sweeper disabled because Redis client is unavailable");
            }
        } else {
            // Without lease refresh a live peer's heartbeat goes stale too, so
            // stealing by lease age is off. Work a server explicitly released
            // (graceful shutdown, or a restart's boot recovery) is owned by
            // no one, though, and is still re-dispatched promptly.
            handles.push(tokio::spawn(steal::run(
                deps.state.clone(),
                deps.server_id.clone(),
                steal::Reclaim::Released,
                deps.config.steal_scan_interval_secs,
                deps.config.steal_batch_size,
                deps.config.max_dispatch_retries,
                cancel_rx.clone(),
            )));
            info!(
                "Lease refresh and steal scanning disabled by config; only released work \
                 is reclaimed"
            );
        }

        if deps.config.operation_reaper_enabled {
            match (reaper_redis, reaper_mq) {
                (Some(redis_client), Some(mq)) => {
                    handles.push(tokio::spawn(operation_reaper::run(
                        redis_client,
                        mq,
                        operation_reaper::ReaperConfig {
                            shared_queue: reaper_shared_queue,
                            interval_secs: deps.config.operation_reaper_interval_secs,
                            grace_secs: deps.config.operation_reaper_grace_secs,
                            max_requeues_per_tick: deps
                                .config
                                .operation_reaper_max_requeues_per_tick,
                            dry_run: deps.config.operation_reaper_dry_run,
                        },
                        reaper_cancel,
                    )));
                }
                _ => info!(
                    "Operation reaper disabled because the Redis client or MQ handle is unavailable"
                ),
            }
        }

        info!(
            server_id = %deps.server_id,
            lease_steal_enabled,
            claim_enabled,
            claim_poll_interval_ms = deps.config.claim_poll_interval_ms,
            claim_batch_size = deps.config.claim_batch_size,
            lease_ttl_secs = deps.config.lease_ttl_secs,
            lease_refresh_interval_secs = deps.config.lease_refresh_interval_secs,
            steal_scan_interval_secs = deps.config.steal_scan_interval_secs,
            steal_batch_size = deps.config.steal_batch_size,
            sweep_interval_secs = deps.config.sweep_interval_secs,
            sweeper_dry_run = deps.config.sweeper_dry_run,
            max_dispatch_retries = deps.config.max_dispatch_retries,
            max_system_error_retries = deps.config.max_system_error_retries,
            "Dispatcher background tasks started"
        );

        Self {
            cancel: Some(cancel_tx),
            handles,
        }
    }

    pub async fn shutdown(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(true);
        }

        for handle in self.handles.drain(..) {
            let _ = handle.await;
        }
    }

    pub fn abort(&mut self) {
        if let Some(tx) = self.cancel.take() {
            let _ = tx.send(true);
        }

        for handle in self.handles.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.abort();
    }
}

#[cfg(test)]
mod queued_predicate_tests {
    use common::SubmissionStatus;

    #[test]
    fn queued_predicate_names_the_queued_status() {
        assert_eq!(
            super::QUEUED_PREDICATE,
            format!("status = '{}'", SubmissionStatus::Queued.as_str())
        );
    }
}
