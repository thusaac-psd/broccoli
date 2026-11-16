//! The per-address sign-in quota counts failed sign-ins only, and holds
//! across the fleet: replicas behind a round-robin gateway share one count,
//! kept in Redis.

use std::net::{IpAddr, Ipv4Addr};

use crate::common::{SpawnOptions, TestApp, routes};
use serde_json::json;
use server::services::ip_quota::FAILED_LOGIN;

/// The address every request of the test client comes from.
const CLIENT: Option<IpAddr> = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));

/// Count `n` failed sign-ins from the test client without signing in.
async fn fail_sign_ins(app: &TestApp, n: u64) {
    for _ in 0..n {
        FAILED_LOGIN.record(&app.state.mq, CLIENT).await;
    }
}

#[tokio::test]
async fn failed_sign_ins_are_capped_per_address_across_every_replica() {
    let a = TestApp::spawn().await;
    let b = a.spawn_replica(SpawnOptions::default()).await;
    let wrong = json!({ "username": "nobody_here", "password": "wrong-password" });

    fail_sign_ins(&a, FAILED_LOGIN.limit - 2).await;
    for replica in [&a, &b] {
        let res = replica.post_without_token(routes::LOGIN, &wrong).await;
        assert_eq!(res.status, 401, "{}", res.text);
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

/// However many contestants sign in from one address, as behind a venue's
/// NAT, their successful sign-ins never use up the address's quota.
#[tokio::test]
async fn successful_sign_ins_are_not_counted_per_address() {
    let app = TestApp::spawn().await;
    app.create_authenticated_user("nat_alice", "password123")
        .await;
    fail_sign_ins(&app, FAILED_LOGIN.limit - 1).await;

    let right = json!({ "username": "nat_alice", "password": "password123" });
    for attempt in 0..3 {
        let res = app.post_without_token(routes::LOGIN, &right).await;
        assert_eq!(res.status, 200, "sign-in {attempt}: {}", res.text);
    }

    let wrong = json!({ "username": "nat_alice", "password": "wrong-password" });
    let res = app.post_without_token(routes::LOGIN, &wrong).await;
    assert_eq!(res.status, 401, "{}", res.text);
    let res = app.post_without_token(routes::LOGIN, &right).await;
    assert_eq!(
        res.status, 429,
        "the address's failures are spent: {}",
        res.text
    );
}
