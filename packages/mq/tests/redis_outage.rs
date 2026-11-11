//! Regression test for the vendored `broccoli_queue` connection patch (see
//! `VENDOR-PATCHES.md`). Upstream retried a failed pool check-out forever with a
//! backoff growing to 300 s and never surfaced the error, so during a Redis
//! outage every MQ call blocked silently and kept blocking for minutes after
//! Redis came back. Now a call fails within seconds, and the first call after
//! Redis returns succeeds.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use testcontainers::ImageExt;
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

#[derive(Clone, Serialize, Deserialize)]
struct Msg {
    id: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn outage_surfaces_an_error_and_recovery_is_immediate() {
    // A fixed host port so the restarted container is reachable at the same URL.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let container = Redis::default()
        .with_mapped_port(port, ContainerPort::Tcp(6379))
        .start()
        .await
        .expect("start redis");
    let mq = mq::init_mq(mq::MqConfig {
        url: format!("redis://127.0.0.1:{port}"),
        pool_size: 2,
    })
    .await
    .expect("init mq");
    let q = "outage_q";
    mq.publish::<Msg>(
        q,
        None,
        &Msg {
            id: "before".into(),
        },
        None,
    )
    .await
    .expect("publish while up");

    container.stop().await.expect("stop redis");

    let started = Instant::now();
    let during = tokio::time::timeout(
        Duration::from_secs(30),
        mq.publish::<Msg>(
            q,
            None,
            &Msg {
                id: "during".into(),
            },
            None,
        ),
    )
    .await
    .expect("a publish during an outage must return, not block");
    assert!(during.is_err(), "a publish during an outage must error");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "the error must surface within seconds, took {:?}",
        started.elapsed()
    );
    let consume = tokio::time::timeout(Duration::from_secs(30), mq.try_consume::<Msg>(q, None))
        .await
        .expect("a consume during an outage must return, not block");
    assert!(consume.is_err(), "a consume during an outage must error");

    container.start().await.expect("restart redis");
    // Give the server a moment to accept connections again.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let attempt = Instant::now();
        match mq
            .publish::<Msg>(q, None, &Msg { id: "after".into() }, None)
            .await
        {
            Ok(_) => break,
            Err(e) if Instant::now() < deadline => {
                assert!(
                    attempt.elapsed() < Duration::from_secs(15),
                    "each call is bounded: {e}"
                );
            }
            Err(e) => panic!("publish never recovered after Redis came back: {e}"),
        }
    }
}
