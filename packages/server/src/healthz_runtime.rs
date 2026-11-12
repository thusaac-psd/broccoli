//! Dedicated tokio runtime + listener for `/livez`, `/healthz` and `/metrics`.
//!
//! Under deliberate api-runtime saturation (large submission bursts, scheduler
//! thrash, blocking work pinned to the main runtime's worker threads), the
//! liveness probe served on the main router can stall well past any sensible
//! probe timeout - defeating its entire purpose as an out-of-band signal. To
//! sidestep that, this module hosts a tiny axum app with only `/livez`,
//! `/healthz` (both liveness-only) and `/metrics` on its **own** `tokio::runtime::Runtime` running on its **own**
//! `std::thread`, bound to its **own** TCP listener (and therefore its own
//! port).
//!
//! The main runtime's saturation can no longer block the probe path because
//! the two runtimes share nothing but the [`AppState`] handles (Arc'd
//! database pool, Arc'd Redis client). Those handles are safe to share across
//! runtimes - they're already designed for arbitrary task counts and do not
//! pin themselves to a specific runtime.
//!
//! Activation is opt-in via `server.healthz_listen`. When `None`, this module
//! is unused and `/livez`, `/healthz` and `/metrics` are served only on the
//! main router (where `/healthz` is the deep dependency check).

use std::net::SocketAddr;
use std::thread::{self, JoinHandle};

use anyhow::Context;
use tokio::runtime::Builder;
use tracing::info;

use crate::handlers;
use crate::metrics_handler;
use crate::state::AppState;

/// Spawns a dedicated tokio runtime on a separate OS thread that serves a
/// minimal axum app (only `/livez`, `/healthz` and `/metrics`) on `addr`.
///
/// The TCP listener is **bound synchronously on the calling runtime** so
/// startup failures (port already in use, insufficient permissions) surface
/// to the caller via `anyhow::Result` rather than disappearing into the
/// spawned thread. Once binding succeeds, the listener's file descriptor is
/// handed off to the dedicated runtime which converts it into a non-blocking
/// `tokio::net::TcpListener` and starts `axum::serve`.
///
/// Returns a `std::thread::JoinHandle<()>`: the parent does not normally
/// `.join()` it (the thread runs for the process lifetime and is reaped by
/// the OS on exit). It exists primarily so the caller can hold ownership and
/// drop it explicitly during shutdown if desired.
pub fn spawn_healthz_runtime(
    addr: SocketAddr,
    worker_threads: usize,
    app_state: AppState,
) -> anyhow::Result<JoinHandle<()>> {
    // Bind synchronously on the calling thread so the caller sees bind errors
    // immediately (the spawned thread would otherwise swallow them).
    let std_listener = std::net::TcpListener::bind(addr)
        .with_context(|| format!("Failed to bind healthz listener to {}", addr))?;
    std_listener
        .set_nonblocking(true)
        .context("Failed to set healthz listener non-blocking")?;
    let bound_addr = std_listener.local_addr().unwrap_or(addr);

    info!(
        addr = %bound_addr,
        worker_threads,
        "Starting dedicated /livez + /healthz + /metrics runtime"
    );

    let handle = thread::Builder::new()
        .name("broccoli-healthz".to_string())
        .spawn(move || {
            let runtime = match Builder::new_multi_thread()
                .worker_threads(worker_threads)
                .enable_all()
                .thread_name("broccoli-healthz")
                .build()
            {
                Ok(rt) => rt,
                Err(err) => {
                    tracing::error!(
                        error = %err,
                        "Failed to build dedicated healthz tokio runtime; \
                         the dedicated /healthz + /metrics listener will not start. \
                         Probes on the main router still work."
                    );
                    return;
                }
            };

            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(err) => {
                        tracing::error!(
                            error = %err,
                            "Failed to convert healthz listener to tokio TcpListener"
                        );
                        return;
                    }
                };

                // Both probe paths are liveness-only here (no DB/Redis ping);
                // `/healthz` keeps its historical meaning on this listener. See
                // `handlers::health::livez` for the rationale.
                let app = axum::Router::new()
                    .route("/livez", axum::routing::get(handlers::health::livez))
                    .route("/healthz", axum::routing::get(handlers::health::livez))
                    .route("/metrics", axum::routing::get(metrics_handler))
                    .with_state(app_state);

                info!(addr = %bound_addr, "Dedicated healthz runtime is serving");

                if let Err(err) = axum::serve(listener, app).await {
                    tracing::error!(error = %err, "Dedicated healthz runtime exited with error");
                }
            });
        })
        .context("Failed to spawn dedicated healthz OS thread")?;

    Ok(handle)
}
