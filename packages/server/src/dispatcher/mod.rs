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
        // The claim fiber (UP#38) is **independent** of the lease/steal
        // toggle: it implements the durable-accept path that UP#37's
        // `Queued`-on-POST relies on, and turning it off without also
        // reverting UP#37 strands rows. We therefore spawn it as long as
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
                cancel_rx.clone(),
            )));
        } else {
            info!(
                "Claim fiber disabled by config. UP#37's Queued rows will not be promoted \
                 to Pending until the fiber is re-enabled — this is intended only as an \
                 incident-response escape hatch."
            );
        }

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
                deps.config.lease_ttl_secs,
                deps.config.steal_scan_interval_secs,
                deps.config.steal_batch_size,
                deps.config.max_dispatch_retries,
                cancel_rx.clone(),
            )));

            // SystemError-retry reaper: bounded re-judge of plugin-finalized
            // SystemError verdicts (a system condition, never the contestant's
            // code), path-agnostic across batch + interactive judging. Shares the
            // lease/steal toggle because it re-dispatches like the steal does.
            handles.push(tokio::spawn(system_error_retry::run(
                deps.state,
                deps.server_id.clone(),
                deps.config.max_system_error_retries,
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
            info!("Lease refresh and steal scanning disabled by config");
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
