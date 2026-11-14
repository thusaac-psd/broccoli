use std::time::Duration;

use anyhow::Context;
use server::config::AppConfig;
use server::runtime::ServerRuntime;

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

    // Prints the plugin group role of a database name and exits, so an
    // installer can admit the plugin roles in pg_hba.conf with the name the
    // server derives (see `plugin_db::schema::group_role`).
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--plugin-group-role") {
        let database = args
            .get(i + 1)
            .context("--plugin-group-role takes a database name")?;
        println!("{}", server::plugin_db::schema::group_role(database));
        return Ok(());
    }

    // Prints the OpenAPI document and exits. The document does not depend on
    // configuration, so none is read.
    if std::env::args().any(|a| a == "--openapi") {
        println!("{}", server::openapi_json());
        return Ok(());
    }

    // Before anything opens sockets or files. See `common::rlimit` for why the
    // default soft limit of 1024 is not enough for a server.
    let nofile = common::rlimit::raise_nofile_limit();

    let app_config = AppConfig::load().context("Failed to load configuration")?;

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("broccoli-server")
        .build()
        .context("Failed to build tokio runtime")?;

    // `bundle export|import`: see `server::bundle::cli`.
    if args.get(1).map(String::as_str) == Some("bundle") {
        let code = tokio_runtime.block_on(server::bundle::cli::run(&app_config, &args[2..]));
        std::process::exit(code);
    }

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
