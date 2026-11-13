use std::future::Future;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo, TokioTimer};
use hyper_util::server::conn::auto::Builder as HttpBuilder;
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpListener;
use tower::ServiceExt;
use tracing::{info, warn};

const HTTP1_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const GRACEFUL_SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const ACCEPT_ERROR_RETRY_DELAY: Duration = Duration::from_secs(1);

pub async fn serve_with_shutdown(listener: TcpListener, app: axum::Router) -> anyhow::Result<()> {
    serve_with_graceful_shutdown(listener, app, shutdown_signal()).await
}

pub async fn serve_with_graceful_shutdown<F>(
    listener: TcpListener,
    app: axum::Router,
    signal: F,
) -> anyhow::Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    let (signal_tx, signal_rx) = tokio::sync::watch::channel(());
    tokio::spawn(async move {
        signal.await;
        drop(signal_rx);
    });

    let (close_tx, close_rx) = tokio::sync::watch::channel(());

    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = signal_tx.closed() => break,
        };
        let (stream, remote_addr) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(%error, "Failed to accept TCP connection; retrying");
                tokio::time::sleep(ACCEPT_ERROR_RETRY_DELAY).await;
                continue;
            }
        };

        let service =
            app.clone()
                .into_service()
                .map_request(move |mut request: Request<Incoming>| {
                    request.extensions_mut().insert(ConnectInfo(remote_addr));
                    request.map(Body::new)
                });
        let hyper_service = TowerToHyperService::new(service);
        let signal_tx = signal_tx.clone();
        let close_rx = close_rx.clone();

        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let mut builder = HttpBuilder::new(TokioExecutor::new());
            builder
                .http1()
                .timer(TokioTimer::new())
                .header_read_timeout(HTTP1_HEADER_READ_TIMEOUT);
            builder.http2().enable_connect_protocol();

            let conn = builder.serve_connection_with_upgrades(io, hyper_service);
            tokio::pin!(conn);
            let shutdown = signal_tx.closed();
            tokio::pin!(shutdown);
            let mut started_shutdown = false;

            loop {
                tokio::select! {
                    result = conn.as_mut() => {
                        if let Err(error) = result {
                            tracing::trace!(%error, "Failed to serve connection");
                        }
                        break;
                    }
                    _ = &mut shutdown, if !started_shutdown => {
                        started_shutdown = true;
                        conn.as_mut().graceful_shutdown();
                    }
                }
            }

            drop(close_rx);
        });
    }

    drop(close_rx);
    drop(listener);

    if tokio::time::timeout(GRACEFUL_SHUTDOWN_DRAIN_TIMEOUT, close_tx.closed())
        .await
        .is_err()
    {
        warn!("Graceful shutdown timed out after 30s; exiting with open connections");
    } else {
        info!("Graceful shutdown completed");
    }

    Ok(())
}

async fn shutdown_signal() {
    common::shutdown::signal().await;
    info!("graceful shutdown started");
}

pub fn pending_shutdown_signal() -> impl Future<Output = ()> + Send + 'static {
    std::future::pending()
}
