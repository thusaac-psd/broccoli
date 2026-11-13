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

    // Prints the OpenAPI document and exits, without connecting to anything.
    // The document does not depend on the configured values, but loading the
    // configuration keeps one startup path.
    if std::env::args().any(|a| a == "--openapi") {
        let app_config = AppConfig::load().context("Failed to load configuration")?;
        println!("{}", server::openapi_json(&app_config));
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

    let result = tokio_runtime.block_on(async move {
        let server_runtime = ServerRuntime::build(app_config).await?;
        nofile.log();
        server_runtime.serve().await
    });
    // `serve` has drained HTTP, the dispatcher and plugin calls, released this
    // server's in-flight work and flushed telemetry. Exit without tearing the
    // runtime down: a plugin call that outlived the drain would otherwise
    // panic inside its runtime bridge, turning a normal stop into a crash
    // for the supervisor.
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("{e:?}");
            std::process::exit(1)
        }
    }
}

/// Hits the local `/healthz` endpoint and returns a process exit code:
/// `0` on a 2xx response, `1` on any other status or transport error.
///
/// Used by the Docker `HEALTHCHECK` directive in `Dockerfile.server`. Skips
/// observability initialization so the probe stays fast and silent.
async fn run_healthcheck(app_config: &AppConfig) -> i32 {
    let url = format!("http://127.0.0.1:{}/healthz", app_config.server.port);

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
