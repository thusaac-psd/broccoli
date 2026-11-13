//! Load probe: a flood of slow plugin route calls, like many clients polling a
//! slow plugin page, while other users sign in and read. Ignored by default;
//! run it with
//!
//! ```bash
//! BROCCOLI_FLOOD_CALLS=520 cargo test -p server --release --test integration \
//!   plugin_flood -- --ignored --nocapture
//! ```
//!
//! It prints login and read latencies taken while the flood is queued, the
//! thread growth, and how the flood calls ended.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use plugin_core::admission::CallClass;
use serde_json::json;

use crate::common::{TestApp, routes};

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .map(|t| t.count())
        .unwrap_or(0)
}

fn percentile(samples: &mut [Duration], p: f64) -> Duration {
    samples.sort();
    let index = ((samples.len() as f64 - 1.0) * p).round() as usize;
    samples[index]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "load probe; run with --ignored"]
async fn a_slow_plugin_flood_leaves_login_and_reads_responsive() {
    let calls = env_usize("BROCCOLI_FLOOD_CALLS", 520);
    let burn = env_usize("BROCCOLI_FLOOD_BURN_MILLIONS", 200);
    let app = Arc::new(TestApp::spawn_with_plugins().await);
    let credentials = json!({ "username": "prober", "password": "prober-password" });
    let token = app
        .create_authenticated_user("prober", "prober-password")
        .await;
    let route = routes::plugin_proxy("server-plugin", &format!("burn/{burn}"));

    let single = Instant::now();
    assert_eq!(app.get_without_token(&route).await.status, 200);
    let one_call = single.elapsed();

    let baseline_threads = threads();
    let ok = Arc::new(AtomicUsize::new(0));
    let busy = Arc::new(AtomicUsize::new(0));
    let other = Arc::new(AtomicUsize::new(0));
    let flood_started = Instant::now();
    let flood: Vec<_> = (0..calls)
        .map(|_| {
            let (app, route) = (app.clone(), route.clone());
            let (ok, busy, other) = (ok.clone(), busy.clone(), other.clone());
            tokio::spawn(async move {
                match app.get_without_token(&route).await.status {
                    200 => ok.fetch_add(1, Ordering::Relaxed),
                    503 => busy.fetch_add(1, Ordering::Relaxed),
                    _ => other.fetch_add(1, Ordering::Relaxed),
                };
            })
        })
        .collect();
    // Measure once the flood is queued at admission.
    let admission = app.state.plugins.executor().admission().clone();
    let tokens = admission.capacity().tokens;
    while admission.queued(CallClass::Interactive) + tokens < calls
        && ok.load(Ordering::Relaxed) + busy.load(Ordering::Relaxed) == 0
    {
        tokio::task::yield_now().await;
    }
    let peak_threads = threads();

    let mut logins = Vec::new();
    for _ in 0..5 {
        let start = Instant::now();
        let res = app.post_without_token(routes::LOGIN, &credentials).await;
        assert_eq!(res.status, 200, "{}", res.text);
        logins.push(start.elapsed());
    }
    let mut reads = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let res = app.get_with_token(routes::ME, &token).await;
        assert_eq!(res.status, 200, "{}", res.text);
        reads.push(start.elapsed());
    }
    let during = threads();

    for call in flood {
        call.await.unwrap();
    }
    let drained = flood_started.elapsed();

    println!(
        "flood: calls={calls} burn={burn}M one_call={one_call:?} drained_in={drained:?} \
         ok={} busy_503={} other={}",
        ok.load(Ordering::Relaxed),
        busy.load(Ordering::Relaxed),
        other.load(Ordering::Relaxed),
    );
    println!(
        "threads: before={baseline_threads} flood_queued={peak_threads} while_measuring={during}"
    );
    println!(
        "login: p50={:?} max={:?}; read /auth/me: p50={:?} p99={:?}",
        percentile(&mut logins, 0.5),
        percentile(&mut logins, 1.0),
        percentile(&mut reads, 0.5),
        percentile(&mut reads, 0.99),
    );
    assert_eq!(other.load(Ordering::Relaxed), 0, "a flood call failed hard");
}
