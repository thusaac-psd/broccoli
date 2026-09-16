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
