//! The per-address sign-in quota holds across the fleet: replicas behind a
//! round-robin gateway share one count, kept in Redis.

use crate::common::{SpawnOptions, TestApp, routes};
use serde_json::json;

#[tokio::test]
async fn ten_sign_ins_a_minute_per_address_across_every_replica() {
    let a = TestApp::spawn_with_options(SpawnOptions {
        rate_limit_auth: true,
        ..Default::default()
    })
    .await;
    let b = a
        .spawn_replica(SpawnOptions {
            rate_limit_auth: true,
            ..Default::default()
        })
        .await;
    let wrong = json!({ "username": "nobody_here", "password": "wrong-password" });

    for i in 0..10 {
        let replica = if i % 2 == 0 { &a } else { &b };
        let res = replica.post_without_token(routes::LOGIN, &wrong).await;
        assert_eq!(res.status, 401, "attempt {i}: {}", res.text);
    }
    for replica in [&a, &b] {
        let res = replica
            .client
            .post(replica.url(routes::LOGIN))
            .json(&wrong)
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 429);
        let retry_after: u64 = res.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=60).contains(&retry_after), "{retry_after}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["code"], "RATE_LIMITED");
    }
}
