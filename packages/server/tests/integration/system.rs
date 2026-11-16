use crate::common::{TestApp, routes};

/// `system:view` is the sole gate on `handlers/system.rs`'s three endpoints:
/// this pins that gate.
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
        assert_eq!(workers_res.body["queue_state"], "reachable");
        assert_eq!(workers_res.body["workers_stale"], false);

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
            intake: common::worker::Intake::Accepting,
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
        let redis = common::test_util::redis_image()
            .start()
            .await
            .expect("start redis");
        let port = redis.get_host_port_ipv4(6379).await.unwrap();
        let url = format!("redis://127.0.0.1:{port}");
        let mut conn = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let app = TestApp::spawn_with_options(SpawnOptions {
            redis_url: Some(url),
            ..Default::default()
        })
        .await;
        let admin = app
            .create_user_with_role("liveness_admin", "pass1234", "admin")
            .await;
        // Written right before the read, so only the request itself passes
        // between them. Fresh: the full TTL. Old: its TTL says it was
        // written 12s ago; still live until it expires.
        write_heartbeat(&mut conn, "fresh", TTL.as_secs()).await;
        write_heartbeat(&mut conn, "old", TTL.as_secs() - 12).await;
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
        assert_eq!(by_id("fresh")["accepting_work"], true, "{}", res.text);
        assert!(by_id("old")["seconds_since_last_seen"].as_u64().unwrap() >= 11);
    }

    #[tokio::test]
    async fn an_unreachable_queue_is_reported_with_the_last_workers_marked_stale() {
        let redis = common::test_util::redis_image()
            .start()
            .await
            .expect("start redis");
        let port = redis.get_host_port_ipv4(6379).await.unwrap();
        let url = format!("redis://127.0.0.1:{port}");
        let mut conn = redis::Client::open(url.as_str())
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let app = TestApp::spawn_with_options(SpawnOptions {
            redis_url: Some(url),
            ..Default::default()
        })
        .await;
        let admin = app
            .create_user_with_role("outage_admin", "pass1234", "admin")
            .await;
        write_heartbeat(&mut conn, "w1", TTL.as_secs()).await;

        let res = app.get_with_token(routes::SYSTEM_OVERVIEW, &admin).await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(res.body["queue_state"], "reachable", "{}", res.text);
        assert_eq!(res.body["workers_stale"], false, "{}", res.text);
        assert_eq!(res.body["workers"][0]["id"], "w1", "{}", res.text);

        redis.stop().await.expect("stop redis");
        let res = app.get_with_token(routes::SYSTEM_OVERVIEW, &admin).await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(res.body["queue_state"], "unreachable", "{}", res.text);
        assert_eq!(res.body["workers_stale"], true, "{}", res.text);
        assert_eq!(
            res.body["workers"][0]["id"], "w1",
            "the last list read, not an empty one: {}",
            res.text
        );
        assert_eq!(res.body["queues"], serde_json::json!([]), "{}", res.text);

        // The worker list says the same: what it read last, marked stale.
        let res = app.get_with_token(routes::SYSTEM_WORKERS, &admin).await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(res.body["queue_state"], "unreachable", "{}", res.text);
        assert_eq!(res.body["workers_stale"], true, "{}", res.text);
        assert_eq!(res.body["workers"][0]["id"], "w1", "{}", res.text);
    }
}

/// The overview tells a queued backlog from work in flight and from work
/// parked behind a disabled contest type, and says when no live worker takes
/// work, the state in which the claim gate claims nothing.
mod backlog {
    use super::*;
    use crate::common::SpawnOptions;
    use crate::common::worker_heartbeat as heartbeat;
    use common::SubmissionStatus;
    use common::worker::Intake;
    use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, Set};
    use server::entity::{submission, user};

    #[tokio::test]
    async fn the_overview_counts_queued_parked_and_in_flight_work_and_a_fleet_that_takes_none() {
        let app = TestApp::spawn_with_options(SpawnOptions {
            disable_claim_fiber: true,
            no_workers: true,
            ..Default::default()
        })
        .await;
        let admin = app
            .create_user_with_role("backlog_admin", "pass1234", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "Backlog").await;
        let user_id = user::Entity::find()
            .filter(user::Column::Username.eq("backlog_admin"))
            .one(&app.db)
            .await
            .unwrap()
            .unwrap()
            .id;
        app.db
            .execute_unprepared(
                "INSERT INTO plugin (id, is_enabled, governs) VALUES \
                 ('backlog-parked', false, '{\"visibility\": false, \"contest_types\": [\"parked-x\"], \"problem_types\": []}')",
            )
            .await
            .unwrap();
        let mut ids = Vec::new();
        for contest_type in ["standard", "standard", "standard", "parked-x"] {
            let row = app
                .insert_submission(submission::ActiveModel {
                    files: Set(serde_json::json!([])),
                    language: Set("cpp".into()),
                    user_id: Set(user_id),
                    problem_id: Set(problem_id),
                    contest_id: Set(None),
                    contest_type: Set(contest_type.into()),
                    status: Set(SubmissionStatus::Queued),
                    judge_epoch: Set(0),
                    created_at: Set(chrono::Utc::now()),
                    ..Default::default()
                })
                .await;
            ids.push(row.id);
        }
        app.put_in_flight(ids[0], SubmissionStatus::Running, "srv", None, None)
            .await;
        let overview = async || {
            let res = app.get_with_token(routes::SYSTEM_OVERVIEW, &admin).await;
            assert_eq!(res.status, 200, "{}", res.text);
            res
        };

        let res = overview().await;
        assert_eq!(res.body["intake"], "no_workers", "{}", res.text);
        assert_eq!(res.body["submissions_queued"], 2, "{}", res.text);
        assert_eq!(res.body["submissions_parked"], 1, "{}", res.text);
        assert_eq!(res.body["submissions_in_progress"], 1, "{}", res.text);

        let refusing = Intake::from_missing(vec!["broccoli-compare".into()]);
        app.state
            .worker_slots
            .observe(&[heartbeat(refusing.clone())], 1);
        let res = overview().await;
        assert_eq!(res.body["intake"], "none_accepting", "{}", res.text);

        app.state
            .worker_slots
            .observe(&[heartbeat(refusing), heartbeat(Intake::Accepting)], 1);
        let res = overview().await;
        assert_eq!(res.body["intake"], "accepting", "{}", res.text);
    }
}
