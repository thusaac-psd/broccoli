use crate::common::{TestApp, routes};

/// `system:view` is the sole gate on `handlers/system.rs`'s three endpoints -
/// this pins that gate (previously untested) for the guard test's
/// `visibility-bypass-audited` comment above that file's entity import.
mod system_view_permission {
    use super::*;

    #[tokio::test]
    async fn contestant_cannot_list_workers() {
        let app = TestApp::spawn().await;
        let contestant = app
            .create_user_with_role("contestant_sys1", "pass1234", "contestant")
            .await;

        let res = app
            .get_with_token(routes::SYSTEM_WORKERS, &contestant)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_list_queues() {
        let app = TestApp::spawn().await;
        let contestant = app
            .create_user_with_role("contestant_sys2", "pass1234", "contestant")
            .await;

        let res = app.get_with_token(routes::SYSTEM_QUEUES, &contestant).await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn contestant_cannot_view_system_overview() {
        let app = TestApp::spawn().await;
        let contestant = app
            .create_user_with_role("contestant_sys3", "pass1234", "contestant")
            .await;

        let res = app
            .get_with_token(routes::SYSTEM_OVERVIEW, &contestant)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn unauthenticated_user_cannot_view_system_overview() {
        let app = TestApp::spawn().await;

        let res = app.get_without_token(routes::SYSTEM_OVERVIEW).await;
        assert_eq!(res.status, 401);
    }

    #[tokio::test]
    async fn admin_can_view_system_overview() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_sys1", "pass1234", "admin")
            .await;

        let res = app.get_with_token(routes::SYSTEM_OVERVIEW, &admin).await;
        assert_eq!(res.status, 200, "system overview failed: {}", res.text);
        assert!(res.body["workers"].is_array());
        assert!(res.body["queues"].is_array());
        assert!(res.body["submissions_in_progress"].is_u64());
        assert!(res.body["dlq_unresolved_count"].is_u64());
    }

    #[tokio::test]
    async fn admin_can_list_workers_and_queues() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin_sys2", "pass1234", "admin")
            .await;

        let workers_res = app.get_with_token(routes::SYSTEM_WORKERS, &admin).await;
        assert_eq!(workers_res.status, 200);
        assert!(workers_res.body["workers"].is_array());

        let queues_res = app.get_with_token(routes::SYSTEM_QUEUES, &admin).await;
        assert_eq!(queues_res.status, 200);
        assert!(queues_res.body["queues"].is_array());
    }
}

/// Worker liveness is read from the heartbeat key's TTL on the Redis clock,
/// so a worker whose own clock is far off is still live while it keeps
/// writing heartbeats.
mod worker_liveness {
    use super::*;
    use crate::common::SpawnOptions;
    use common::liveness::{NodeRole, TTL};
    use common::worker::HeartbeatPayload;
    use redis::AsyncCommands;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::redis::Redis;

    async fn write_heartbeat(conn: &mut redis::aio::MultiplexedConnection, id: &str, ttl: u64) {
        // The worker's clock is an hour behind the server's.
        let skewed = chrono::Utc::now() - chrono::Duration::hours(1);
        let payload = HeartbeatPayload {
            id: id.to_string(),
            started_at: skewed,
            last_seen: skewed,
            in_flight: 0,
            max_concurrency: Some(2),
            fairness_mode: "unknown".to_string(),
            sandbox_backend: "test".to_string(),
            version: "test".to_string(),
            hostname: None,
            ip_addresses: Vec::new(),
            os: None,
            arch: None,
            cpu_count: None,
            pid: None,
        };
        let _: () = conn
            .set_ex(
                NodeRole::Worker.key(&common::config::default_operation_queue_name(), id),
                serde_json::to_string(&payload).unwrap(),
                ttl,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_fresh_heartbeat_is_live_whatever_the_worker_clock_says() {
        let redis = Redis::default().start().await.expect("start redis");
        let port = redis.get_host_port_ipv4(6379).await.unwrap();
        let url = format!("redis://127.0.0.1:{port}");
        let mut conn = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        // Fresh: just written with the full TTL.
        write_heartbeat(&mut conn, "fresh", TTL.as_secs()).await;
        // Old: its TTL says it was written 12s ago; still live until it expires.
        write_heartbeat(&mut conn, "old", TTL.as_secs() - 12).await;

        let app = TestApp::spawn_with_options(SpawnOptions {
            redis_url: Some(url),
            ..Default::default()
        })
        .await;
        let admin = app
            .create_user_with_role("liveness_admin", "pass1234", "admin")
            .await;
        let res = app.get_with_token(routes::SYSTEM_WORKERS, &admin).await;
        assert_eq!(res.status, 200, "{}", res.text);
        let workers = res.body["workers"].as_array().unwrap();
        let by_id = |id: &str| {
            workers
                .iter()
                .find(|w| w["id"] == id)
                .unwrap_or_else(|| panic!("worker {id} listed: {}", res.text))
        };
        assert!(by_id("fresh")["seconds_since_last_seen"].as_u64().unwrap() <= 2);
        assert!(by_id("old")["seconds_since_last_seen"].as_u64().unwrap() >= 11);
    }
}
