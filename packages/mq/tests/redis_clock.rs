//! Regression tests for the vendored `broccoli_queue` Redis-clock patch (see
//! `VENDOR-PATCHES.md`). Upstream scored a published message with the
//! publisher's wall clock and gated consumption on the consumer's wall clock, so
//! a message from a host whose clock ran ahead stayed invisible for the skew.
//! Both sides now read Redis `TIME`, so the only clock involved is Redis's.

use serde::{Deserialize, Serialize};
use testcontainers::runners::AsyncRunner;
use testcontainers_modules::redis::Redis;

#[derive(Clone, Serialize, Deserialize)]
struct Msg {
    id: String,
}

async fn redis_time_ns(conn: &mut redis::aio::MultiplexedConnection) -> f64 {
    let (secs, micros): (i64, i64) = redis::cmd("TIME").query_async(conn).await.unwrap();
    secs as f64 * 1e9 + micros as f64 * 1e3
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_score_and_consume_gate_use_redis_time() {
    let container = Redis::default().start().await.expect("start redis");
    let port = container.get_host_port_ipv4(6379).await.expect("port");
    let url = format!("redis://127.0.0.1:{port}");
    let mq = mq::init_mq(mq::MqConfig {
        url: url.clone(),
        pool_size: 2,
    })
    .await
    .expect("init mq");
    let client = redis::Client::open(url).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();

    // The publish score is `priority x Redis TIME` at publish.
    let q = "clock_q";
    let before = redis_time_ns(&mut conn).await;
    let published = mq
        .publish::<Msg>(q, None, &Msg { id: "m1".into() }, None)
        .await
        .expect("publish");
    let after = redis_time_ns(&mut conn).await;
    let score: f64 = redis::cmd("ZSCORE")
        .arg(q)
        .arg(published.task_id.to_string())
        .query_async(&mut conn)
        .await
        .unwrap();
    // Doubles at ~8.8e18 resolve to ~1 us, so allow a little slack.
    let slack = 1e4;
    assert!(
        score >= 5.0 * before - slack && score <= 5.0 * after + slack,
        "score {score} must be 5 x Redis TIME within [{before}, {after}]"
    );
    assert!(mq.try_consume::<Msg>(q, None).await.unwrap().is_some());

    // The consume gate compares against Redis TIME: a message scored 30 s in
    // Redis's future is held back, one scored in its past is delivered. The
    // second case is also the upgrade path: a message published by the old
    // code (score = 5 x publisher clock) is still consumed.
    for (id, offset_secs, visible) in [("future", 30.0, false), ("legacy", -1.0, true)] {
        let qn = format!("clock_q_{id}");
        let task_id = format!("task-{id}");
        let payload = serde_json::to_string(&Msg { id: id.into() }).unwrap();
        let now = redis_time_ns(&mut conn).await;
        let _: () = redis::cmd("HSET")
            .arg(&task_id)
            .arg("task_id")
            .arg(&task_id)
            .arg("payload")
            .arg(&payload)
            .arg("attempts")
            .arg("0")
            .arg("priority")
            .arg("5")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("ZADD")
            .arg(&qn)
            .arg(5.0 * (now + offset_secs * 1e9))
            .arg(&task_id)
            .query_async(&mut conn)
            .await
            .unwrap();
        let got = mq.try_consume::<Msg>(&qn, None).await.unwrap();
        assert_eq!(got.is_some(), visible, "message {id}");
    }
}
