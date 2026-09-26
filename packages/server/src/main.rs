use std::time::Duration;

use anyhow::Context;
use server::config::AppConfig;
use server::runtime::ServerRuntime;
use tracing::info;

// Use jemalloc instead of the system (glibc) allocator. Under concurrent judging
// of large-testcase problems glibc retains freed mmap'd allocations and inflates
// RSS far past the live working set; jemalloc returns memory to the OS
// aggressively, keeping RSS close to the actual in-use set. (See the dep note in
// Cargo.toml; `profiling` feature enables `_RJEM_MALLOC_CONF=prof:true` diagnosis.)
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("broccoli-server {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    // Before anything opens sockets or files. See `common::rlimit` for why the
    // default soft limit of 1024 is not enough for a server.
    let nofile = common::rlimit::raise_nofile_limit();

    let app_config = AppConfig::load().context("Failed to load configuration")?;

    if app_config.jwt_secret_is_weak() {
        tracing::warn!(
            min_recommended_bytes = server::config::MIN_RECOMMENDED_JWT_SECRET_BYTES,
            "auth.jwt_secret is shorter than the recommended minimum; a weak HS256 secret makes all access tokens forgeable (full account takeover). Set a long, random secret."
        );
    }

    let max_blocking_threads = app_config.server.effective_max_blocking_threads();
    info!(
        max_blocking_threads,
        configured = ?app_config.server.max_blocking_threads,
        "Sizing tokio blocking-thread pool"
    );

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(max_blocking_threads)
        .thread_name("broccoli-server")
        .build()
        .context("Failed to build tokio runtime")?;

    if std::env::args().any(|a| a == "--healthcheck") {
        let exit_code = tokio_runtime.block_on(run_healthcheck(&app_config));
        std::process::exit(exit_code);
    }

    tokio_runtime.block_on(async move {
        let server_runtime = ServerRuntime::build(app_config).await?;
        nofile.log();
        server_runtime.serve().await
    })
}

/// Hits the local `/healthz` endpoint and returns a process exit code:
/// `0` on a 2xx response, `1` on any other status or transport error.
///
/// Used by the Docker `HEALTHCHECK` directive in `Dockerfile.server`. Skips
/// observability initialization so the probe stays fast and silent.
///
/// Prefers the port of `server.healthz_listen` (the dedicated runtime, UP#14e)
/// when configured, so the in-container probe gets the same isolation
/// benefits as external probes. Falls back to the main listener port
/// otherwise.
async fn run_healthcheck(app_config: &AppConfig) -> i32 {
    let port = app_config
        .server
        .healthz_listen
        .as_deref()
        .and_then(parse_port_from_listen_addr)
        .unwrap_or(app_config.server.port);
    let url = format!("http://127.0.0.1:{}/healthz", port);

    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("healthcheck failed: {e}");
            return 1;
        }
    };

    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => 0,
        Ok(resp) => {
            eprintln!("healthcheck failed: status {}", resp.status());
            1
        }
        Err(e) => {
            eprintln!("healthcheck failed: {e}");
            1
        }
    }
}

/// Extracts the port from a `host:port` listen string. Requires a full,
/// well-formed `SocketAddr` - `0.0.0.0:9091`, `[::]:9091`, `[::1]:8080`. An
/// unbracketed IPv6 like `"::1"` is rejected (returns `None`) so the caller
/// falls back to the main listener port rather than silently probing port 1.
fn parse_port_from_listen_addr(addr: &str) -> Option<u16> {
    addr.parse::<std::net::SocketAddr>().ok().map(|s| s.port())
}

#[cfg(test)]
mod tests {
    use super::parse_port_from_listen_addr;

    #[test]
    fn parses_ipv4_listen_addr() {
        assert_eq!(parse_port_from_listen_addr("0.0.0.0:9091"), Some(9091));
        assert_eq!(parse_port_from_listen_addr("127.0.0.1:1234"), Some(1234));
    }

    #[test]
    fn parses_ipv6_listen_addr() {
        assert_eq!(parse_port_from_listen_addr("[::]:9091"), Some(9091));
        assert_eq!(parse_port_from_listen_addr("[::1]:8080"), Some(8080));
    }

    #[test]
    fn rejects_invalid_input() {
        assert_eq!(parse_port_from_listen_addr("not-an-addr"), None);
        assert_eq!(parse_port_from_listen_addr("host:not-a-port"), None);
        // Unbracketed IPv6 - would previously yield Some(1), now correctly None.
        assert_eq!(parse_port_from_listen_addr("::1"), None);
        // Bare host with no port.
        assert_eq!(parse_port_from_listen_addr("127.0.0.1"), None);
    }
}
