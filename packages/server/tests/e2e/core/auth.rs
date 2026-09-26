use serde_json::json;

use crate::common::E2eTestApp;

mod registration {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn new_user_can_register() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/auth/register",
                &json!({"username": "auth_reg1", "password": "securepass123"}),
            )
            .await;

        assert_eq!(res.status, 201);
        assert!(res.body["id"].is_number());
        assert_eq!(res.body["username"], "auth_reg1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn duplicate_username_returns_409() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let body = json!({"username": "auth_dup1", "password": "securepass123"});

        let first = app.post_without_token("/api/v1/auth/register", &body).await;
        assert_eq!(first.status, 201);

        let res = app.post_without_token("/api/v1/auth/register", &body).await;

        assert_eq!(res.status, 409);
        assert_eq!(res.body["code"], "USERNAME_TAKEN");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_username_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/auth/register",
                &json!({"username": "", "password": "securepass123"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn whitespace_only_username_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/auth/register",
                &json!({"username": "   ", "password": "securepass123"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_password_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/auth/register",
                &json!({"username": "auth_emptypw1", "password": ""}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn short_password_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/auth/register",
                &json!({"username": "auth_shortpw1", "password": "short"}),
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_fields_returns_validation_error() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token("/api/v1/auth/register", &json!({"username": "auth_nopw1"}))
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }
}

mod login {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn registered_user_can_login_and_receive_token() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let body = json!({"username": "auth_login1", "password": "securepass123"});

        let reg = app.post_without_token("/api/v1/auth/register", &body).await;
        assert_eq!(reg.status, 201);

        let res = app.post_without_token("/api/v1/auth/login", &body).await;

        assert_eq!(res.status, 200);
        assert!(res.body["token"].is_string());
        assert_eq!(res.body["username"], "auth_login1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_password_returns_invalid_credentials() {
        let app = E2eTestApp::spawn_without_plugins().await;

        app.post_without_token(
            "/api/v1/auth/register",
            &json!({"username": "auth_wrongpw1", "password": "securepass123"}),
        )
        .await;

        let res = app
            .post_without_token(
                "/api/v1/auth/login",
                &json!({"username": "auth_wrongpw1", "password": "wrongpassword"}),
            )
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "INVALID_CREDENTIALS");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nonexistent_user_returns_invalid_credentials() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .post_without_token(
                "/api/v1/auth/login",
                &json!({"username": "auth_nouser1", "password": "securepass123"}),
            )
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "INVALID_CREDENTIALS");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_user_receives_contestant_role() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let body = json!({"username": "auth_role1", "password": "securepass123"});

        app.post_without_token("/api/v1/auth/register", &body).await;
        let res = app.post_without_token("/api/v1/auth/login", &body).await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["roles"], json!(["contestant"]));
    }
}

mod authenticated_access {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn valid_token_can_access_me() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_authenticated_user("auth_me1", "securepass123")
            .await;

        let res = app.get_with_token("/api/v1/auth/me", &token).await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["username"], "auth_me1");
        assert!(res.body["id"].is_number());
        assert!(res.body["roles"].is_array());
        assert!(res.body["permissions"].is_array());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_token_returns_token_invalid() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app
            .get_with_token("/api/v1/auth/me", "not-a-valid-jwt-token")
            .await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_INVALID");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_token_returns_token_missing() {
        let app = E2eTestApp::spawn_without_plugins().await;

        let res = app.get_without_token("/api/v1/auth/me").await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }
}

mod permissions {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_create_problem() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("auth_perm1", "securepass123", "contestant")
            .await;

        let res = app
            .post_with_token(
                "/api/v1/problems",
                &json!({
                    "title": "Forbidden Problem",
                    "content": "desc",
                    "time_limit": 1000,
                    "memory_limit": 262144,
                    "problem_type": "batch",
                    "default_contest_type": app.contest_type,
                    "checker_format": "exact",
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_list_users() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("auth_perm2", "securepass123", "contestant")
            .await;

        let res = app.get_with_token("/api/v1/users", &token).await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_create_contest() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("auth_perm3", "securepass123", "contestant")
            .await;

        let res = app
            .post_with_token(
                "/api/v1/contests",
                &json!({
                    "title": "Forbidden Contest",
                    "description": "desc",
                    "start_time": "2099-01-01T00:00:00Z",
                    "end_time": "2099-01-02T00:00:00Z",
                    "is_public": false,
                }),
                &token,
            )
            .await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn contestant_cannot_access_dlq() {
        let app = E2eTestApp::spawn_without_plugins().await;
        let token = app
            .create_user_with_role("auth_perm4", "securepass123", "contestant")
            .await;

        let res = app.get_with_token("/api/v1/dlq", &token).await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}
