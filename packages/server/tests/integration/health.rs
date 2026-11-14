use crate::common::TestApp;

mod healthz {
    use super::*;

    #[tokio::test]
    async fn returns_200_when_db_and_mq_are_up() {
        let app = TestApp::spawn().await;
        let resp = app.get_without_token("/healthz").await;

        assert_eq!(resp.status, 200, "body: {}", resp.text);

        assert_eq!(
            resp.body.get("status").and_then(|v| v.as_str()),
            Some("ok"),
            "body: {}",
            resp.text
        );
        assert_eq!(
            resp.body.get("db").and_then(|v| v.as_str()),
            Some("ok"),
            "body: {}",
            resp.text
        );
        assert_eq!(
            resp.body.get("mq").and_then(|v| v.as_str()),
            Some("ok"),
            "body: {}",
            resp.text
        );

        let version = resp.body.get("version").and_then(|v| v.as_str()).unwrap();
        assert!(!version.is_empty(), "version should be non-empty");
        assert!(
            resp.body.get("git_sha").and_then(|v| v.as_str()).is_some(),
            "git_sha should be present"
        );
    }
}

mod api_health {
    use super::*;

    #[tokio::test]
    async fn returns_200_with_same_body_shape_as_healthz() {
        let app = TestApp::spawn().await;
        let resp = app.get_without_token("/api/v1/health").await;

        assert_eq!(resp.status, 200, "body: {}", resp.text);
        assert_eq!(resp.body.get("status").and_then(|v| v.as_str()), Some("ok"));
        assert_eq!(resp.body.get("db").and_then(|v| v.as_str()), Some("ok"));
        assert_eq!(resp.body.get("mq").and_then(|v| v.as_str()), Some("ok"));
        assert!(resp.body.get("version").is_some());
        assert!(resp.body.get("git_sha").is_some());
    }
}

mod livez {
    use super::*;
    use std::time::{Duration, Instant};

    /// `/livez` makes no dependency calls, so it keeps answering 200 while the
    /// database is unreachable; `/healthz` reports the outage. A load balancer
    /// probing `/livez` therefore does not eject every replica at once when
    /// shared infrastructure blips.
    #[tokio::test]
    async fn stays_alive_when_the_database_is_down() {
        let app = TestApp::spawn().await;
        // Every clone shares one pool, so this takes the server's DB away.
        app.db.clone().close().await.expect("close pool");

        let live = app.get_without_token("/livez").await;
        assert_eq!(live.status, 200, "body: {}", live.text);
        assert_eq!(
            live.body.get("status").and_then(|v| v.as_str()),
            Some("alive")
        );

        let deep = app.get_without_token("/healthz").await;
        assert_eq!(deep.status, 503, "body: {}", deep.text);
        assert_eq!(deep.body.get("db").and_then(|v| v.as_str()), Some("down"));
    }

    /// The deep check probes concurrently within one budget: an MQ endpoint
    /// that never answers costs at most the budget, not budget-per-probe, and
    /// stays under a typical 2 s load-balancer probe timeout.
    #[tokio::test]
    async fn deep_check_is_bounded_when_a_dependency_hangs() {
        let app = TestApp::spawn().await;
        // A listener that accepts but never speaks RESP: the PING hangs.
        let silent = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = silent.local_addr().unwrap();
        let _accept = tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = silent.accept().await {
                held.push(sock);
            }
        });
        let mq = mq::Mq::lazy(&format!("redis://{addr}"), "silent").unwrap();

        let started = Instant::now();
        let deps = server::handlers::health::probe_dependencies(&app.db, &mq).await;
        let elapsed = started.elapsed();

        assert_eq!(deps.mq(), "down");
        assert_eq!(deps.db(), "ok");
        assert!(!deps.healthy());
        assert!(elapsed < Duration::from_millis(1900), "took {elapsed:?}");
    }
}
