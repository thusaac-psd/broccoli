//! The failed sign-in quota counts failures per account and client address,
//! across the fleet: replicas behind a round-robin gateway share one log,
//! kept in Redis. Other accounts' failures never refuse a sign-in.

use std::net::{IpAddr, Ipv4Addr};

use crate::common::{SpawnOptions, TestApp, routes};
use serde_json::json;
use server::config::AuthConfig;
use server::services::failed_sign_ins::FailedSignIns;

/// The address every request of the test client comes from.
const CLIENT: Option<IpAddr> = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));

/// The quota every test here runs with.
const LIMIT: u32 = 10;

fn options() -> SpawnOptions {
    SpawnOptions {
        login_failure_limit: Some(LIMIT),
        ..Default::default()
    }
}

fn quota() -> FailedSignIns {
    FailedSignIns::of(&AuthConfig {
        login_failure_limit: LIMIT,
        ..Default::default()
    })
}

async fn sign_in(app: &TestApp, username: &str, password: &str) -> reqwest::Response {
    app.client
        .post(app.url(routes::LOGIN))
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await
        .unwrap()
}

#[tokio::test]
async fn failed_sign_ins_are_capped_per_account_and_address_across_every_replica() {
    let a = TestApp::spawn_with_options(options()).await;
    let b = a.spawn_replica(options()).await;
    a.create_authenticated_user("carol", "password123").await;

    for attempt in 0..LIMIT {
        let replica = if attempt % 2 == 0 { &a } else { &b };
        let res = sign_in(replica, "carol", "wrong-password").await;
        assert_eq!(res.status(), 401, "attempt {attempt}");
    }
    for replica in [&a, &b] {
        let res = sign_in(replica, "Carol", "wrong-password").await;
        assert_eq!(res.status(), 429, "the count is per account, in any case");
        let retry_after: u64 = res.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=60).contains(&retry_after), "{retry_after}");
        let body: serde_json::Value = res.json().await.unwrap();
        assert_eq!(body["code"], "RATE_LIMITED");
    }
    // While its own count is spent, even the account's right password waits:
    // answering it would tell a guesser it was right.
    assert_eq!(sign_in(&a, "carol", "password123").await.status(), 429);

    // The count is per address too.
    let elsewhere = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)));
    quota()
        .check(&a.state.mq, elsewhere, "carol")
        .await
        .expect("another address is not refused");
}

/// However many accounts fail from one address, as behind a venue's NAT, a
/// right password is never refused for them.
#[tokio::test]
async fn other_accounts_failures_never_refuse_a_sign_in() {
    let app = TestApp::spawn_with_options(options()).await;
    app.create_authenticated_user("nat_alice", "password123")
        .await;
    let names: Vec<String> = (0..150).map(|i| format!("typo_{i}")).collect();
    let typos = names
        .iter()
        .map(|name| sign_in(&app, name, "wrong-password"));
    for res in futures::future::join_all(typos).await {
        assert_eq!(res.status(), 401);
    }
    for _ in 0..LIMIT {
        assert_eq!(
            sign_in(&app, "nat_bob", "wrong-password").await.status(),
            401
        );
    }
    for attempt in 0..3 {
        let res = sign_in(&app, "nat_alice", "password123").await;
        assert_eq!(res.status(), 200, "sign-in {attempt}");
    }
}

/// A success clears its account's failures, so a contestant who finally
/// types the right password starts clean.
#[tokio::test]
async fn a_success_clears_its_accounts_failures() {
    let app = TestApp::spawn_with_options(options()).await;
    app.create_authenticated_user("dave", "password123").await;
    for _ in 0..LIMIT - 1 {
        assert_eq!(sign_in(&app, "dave", "wrong-password").await.status(), 401);
    }
    assert_eq!(sign_in(&app, "dave", "password123").await.status(), 200);
    for attempt in 0..LIMIT {
        let res = sign_in(&app, "dave", "wrong-password").await;
        assert_eq!(res.status(), 401, "attempt {attempt} after the success");
    }
}

/// A burst of wrong passwords for one account from one address, all checked
/// before any is verified, learns exactly the limit of failures between them:
/// every other attempt answers 429, however they interleave.
#[tokio::test]
async fn a_parallel_burst_learns_at_most_the_limit_of_failures() {
    let app = TestApp::spawn_with_options(options()).await;
    app.create_authenticated_user("erin", "password123").await;

    let attempts = (0..150).map(|_| sign_in(&app, "erin", "wrong-password"));
    let statuses: Vec<u16> = futures::future::join_all(attempts)
        .await
        .iter()
        .map(|res| res.status().as_u16())
        .collect();
    let learned = statuses.iter().filter(|s| **s == 401).count();
    let refused = statuses.iter().filter(|s| **s == 429).count();
    assert_eq!((learned, refused), (LIMIT as usize, 150 - LIMIT as usize));

    // The same bound holds when every attempt is checked first and settled
    // after, the widest interleaving a burst can have.
    let quota = quota();
    let mq = &app.state.mq;
    for _ in 0..150 {
        quota.check(mq, CLIENT, "frank").await.unwrap();
    }
    let mut learned = 0;
    for _ in 0..150 {
        if quota.settle(mq, CLIENT, "frank", false).await.is_ok() {
            learned += 1;
        }
    }
    assert_eq!(learned, LIMIT);
    assert!(
        quota.settle(mq, CLIENT, "frank", true).await.is_err(),
        "a right password verified while the count is spent answers 429"
    );
}
