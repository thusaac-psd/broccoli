use crate::common::{TestApp, routes};
use serde_json::json;

mod problem_config {
    use super::*;

    #[tokio::test]
    async fn admin_can_list_problem_config_empty() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin1", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let res = app
            .get_with_token(&routes::problem_config(problem_id), &token)
            .await;

        assert_eq!(res.status, 200);
        let entries = res.body.as_array().unwrap();
        assert!(!entries.is_empty(), "declared namespaces are listed");
        assert!(
            entries.iter().all(|e| e["updated_at"].is_null()),
            "nothing stored yet: {entries:?}"
        );
    }

    #[tokio::test]
    async fn admin_can_upsert_and_get_problem_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin2", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let body = json!({"config": {"tolerance": 0.001}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "checker");
        assert_eq!(res.body["config"]["tolerance"], 0.001);

        let res = app
            .get_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "checker");
        assert_eq!(res.body["config"]["tolerance"], 0.001);
    }

    #[tokio::test]
    async fn upsert_overwrites_existing_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin3", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let body1 = json!({"config": {"v": 1}});
        app.put_with_token(
            &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
            &body1,
            &token,
        )
        .await;

        let body2 = json!({"config": {"v": 2}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &body2,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["config"]["v"], 2);

        let res = app
            .get_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &token,
            )
            .await;
        assert_eq!(res.body["config"]["v"], 2);
    }

    #[tokio::test]
    async fn admin_can_delete_problem_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin4", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let body = json!({"config": {"k": "v"}});
        app.put_with_token(
            &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
            &body,
            &token,
        )
        .await;

        let res = app
            .delete_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &token,
            )
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn list_returns_all_namespaces() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin5", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        app.put_with_token(
            &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
            &json!({"config": {"a": 1}}),
            &token,
        )
        .await;
        app.put_with_token(
            &routes::problem_config_ns(problem_id, "test-plugin", "scoring"),
            &json!({"config": {"b": 2}}),
            &token,
        )
        .await;

        let res = app
            .get_with_token(&routes::problem_config(problem_id), &token)
            .await;

        assert_eq!(res.status, 200);
        let mut stored: Vec<&str> = res
            .body
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| !e["updated_at"].is_null())
            .map(|e| e["namespace"].as_str().unwrap())
            .collect();
        stored.sort_unstable();
        assert_eq!(stored, ["checker", "scoring"]);
    }

    #[tokio::test]
    async fn upsert_returns_404_for_missing_problem() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin6", "password123", "admin")
            .await;

        let body = json!({"config": {"k": "v"}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(99999, "test-plugin", "checker"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn get_returns_404_for_missing_namespace() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin7", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let res = app
            .get_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "nonexistent"),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn delete_returns_404_for_missing_namespace() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin8", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let res = app
            .delete_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "nonexistent"),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod namespace_validation {
    use super::*;

    #[tokio::test]
    async fn rejects_empty_namespace() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin9", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let res = app
            .get_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", ""),
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn rejects_namespace_too_long() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin24", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let long_namespace = "a".repeat(129);
        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", &long_namespace),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn rejects_namespace_with_special_chars() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin10", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "bad.name"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn accepts_namespace_with_hyphens_and_underscores() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin11", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "my-checker_v2"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "my-checker_v2");
    }
}

mod permissions {
    use super::*;

    #[tokio::test]
    async fn user_without_permission_cannot_access_problem_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let admin = app
            .create_user_with_role("admin12", "password123", "admin")
            .await;
        let user = app.create_authenticated_user("user1", "password123").await;
        let problem_id = app.create_problem(&admin, "P1").await;

        let res = app
            .get_with_token(&routes::problem_config(problem_id), &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &body,
                &user,
            )
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn user_without_permission_cannot_access_contest_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let admin = app
            .create_user_with_role("admin13", "password123", "admin")
            .await;
        let user = app.create_authenticated_user("user2", "password123").await;
        let contest_id = app.create_contest(&admin, "C1", true, true).await;

        let res = app
            .get_with_token(&routes::contest_config(contest_id), &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }
}

mod contest_config {
    use super::*;

    #[tokio::test]
    async fn admin_can_upsert_and_get_contest_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin14", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;

        let body = json!({"config": {"type": "icpc"}});
        let res = app
            .put_with_token(
                &routes::contest_config_ns(contest_id, "test-plugin", "contest-type"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "contest-type");
        assert_eq!(res.body["config"]["type"], "icpc");

        let res = app
            .get_with_token(
                &routes::contest_config_ns(contest_id, "test-plugin", "contest-type"),
                &token,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["config"]["type"], "icpc");
    }

    #[tokio::test]
    async fn admin_can_delete_contest_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin15", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;

        let body = json!({"config": {"k": "v"}});
        app.put_with_token(
            &routes::contest_config_ns(contest_id, "test-plugin", "scoring"),
            &body,
            &token,
        )
        .await;

        let res = app
            .delete_with_token(
                &routes::contest_config_ns(contest_id, "test-plugin", "scoring"),
                &token,
            )
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::contest_config_ns(contest_id, "test-plugin", "scoring"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn upsert_returns_404_for_missing_contest() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin16", "password123", "admin")
            .await;

        let body = json!({"config": {"k": "v"}});
        let res = app
            .put_with_token(
                &routes::contest_config_ns(99999, "test-plugin", "scoring"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod contest_problem_config {
    use super::*;

    #[tokio::test]
    async fn admin_can_upsert_and_get_contest_problem_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin17", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;
        let problem_id = app.create_problem(&token, "P1").await;
        app.add_problem_to_contest(contest_id, problem_id, &token)
            .await;

        let body = json!({"config": {"time_limit": 2000}});
        let res = app
            .put_with_token(
                &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "limits");
        assert_eq!(res.body["config"]["time_limit"], 2000);

        let res = app
            .get_with_token(
                &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
                &token,
            )
            .await;
        assert_eq!(res.status, 200);
        assert_eq!(res.body["config"]["time_limit"], 2000);
    }

    #[tokio::test]
    async fn admin_can_delete_contest_problem_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin18", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;
        let problem_id = app.create_problem(&token, "P1").await;
        app.add_problem_to_contest(contest_id, problem_id, &token)
            .await;

        let body = json!({"config": {"k": "v"}});
        app.put_with_token(
            &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
            &body,
            &token,
        )
        .await;

        let res = app
            .delete_with_token(
                &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
                &token,
            )
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn admin_can_list_contest_problem_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin23", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;
        let problem_id = app.create_problem(&token, "P1").await;
        app.add_problem_to_contest(contest_id, problem_id, &token)
            .await;

        app.put_with_token(
            &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
            &json!({"config": {"t": 1}}),
            &token,
        )
        .await;

        let res = app
            .get_with_token(
                &routes::contest_problem_config(contest_id, problem_id),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        let stored: Vec<&serde_json::Value> = res
            .body
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| !e["updated_at"].is_null())
            .collect();
        assert_eq!(stored.len(), 1, "{}", res.text);
        assert_eq!(stored[0]["namespace"], "limits");
    }

    #[tokio::test]
    async fn returns_404_for_missing_contest_problem() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin19", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;

        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::contest_problem_config_ns(contest_id, 99999, "test-plugin", "limits"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }
}

mod plugin_global_config {
    use super::*;

    #[tokio::test]
    async fn admin_can_list_plugin_global_config_empty() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("pgc_admin1", "password123", "admin")
            .await;

        let res = app
            .get_with_token(&routes::plugin_global_config("my-plugin"), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn admin_can_upsert_and_get_plugin_global_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("pgc_admin2", "password123", "admin")
            .await;

        let body = json!({"config": {"compiler": "/usr/bin/g++"}});
        let res = app
            .put_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "settings");
        assert_eq!(res.body["config"]["compiler"], "/usr/bin/g++");

        let res = app
            .get_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["namespace"], "settings");
        assert_eq!(res.body["config"]["compiler"], "/usr/bin/g++");
    }

    #[tokio::test]
    async fn upsert_overwrites_existing() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("pgc_admin3", "password123", "admin")
            .await;

        let body1 = json!({"config": {"v": 1}});
        app.put_with_token(
            &routes::plugin_global_config_ns("my-plugin", "settings"),
            &body1,
            &token,
        )
        .await;

        let body2 = json!({"config": {"v": 2}});
        let res = app
            .put_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &body2,
                &token,
            )
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body["config"]["v"], 2);

        let res = app
            .get_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &token,
            )
            .await;
        assert_eq!(res.body["config"]["v"], 2);
    }

    #[tokio::test]
    async fn admin_can_delete_plugin_global_config() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("pgc_admin4", "password123", "admin")
            .await;

        let body = json!({"config": {"k": "v"}});
        app.put_with_token(
            &routes::plugin_global_config_ns("my-plugin", "settings"),
            &body,
            &token,
        )
        .await;

        let res = app
            .delete_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &token,
            )
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
        assert_eq!(res.body["code"], "NOT_FOUND");
    }

    #[tokio::test]
    async fn permission_denied_for_non_admin() {
        let app = TestApp::spawn_with_config_schemas().await;
        let user = app
            .create_authenticated_user("pgc_user1", "password123")
            .await;

        let res = app
            .get_with_token(&routes::plugin_global_config("my-plugin"), &user)
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");

        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::plugin_global_config_ns("my-plugin", "settings"),
                &body,
                &user,
            )
            .await;
        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn rejects_invalid_plugin_id() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("pgc_admin5", "password123", "admin")
            .await;

        let body = json!({"config": {}});
        let res = app
            .put_with_token(
                &routes::plugin_global_config_ns("bad.plugin.id", "settings"),
                &body,
                &token,
            )
            .await;

        assert_eq!(res.status, 400);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }

    #[tokio::test]
    async fn list_returns_multiple_namespaces() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("pgc_admin6", "password123", "admin")
            .await;

        app.put_with_token(
            &routes::plugin_global_config_ns("my-plugin", "settings"),
            &json!({"config": {"a": 1}}),
            &token,
        )
        .await;
        app.put_with_token(
            &routes::plugin_global_config_ns("my-plugin", "features"),
            &json!({"config": {"b": 2}}),
            &token,
        )
        .await;

        let res = app
            .get_with_token(&routes::plugin_global_config("my-plugin"), &token)
            .await;

        assert_eq!(res.status, 200);
        assert_eq!(res.body.as_array().unwrap().len(), 2);
    }
}

mod cascade_deletion {
    use super::*;

    #[tokio::test]
    async fn deleting_problem_cascades_config_deletion() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin20", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&token, "P1").await;

        app.put_with_token(
            &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
            &json!({"config": {"k": "v"}}),
            &token,
        )
        .await;

        let res = app
            .delete_with_token(&routes::problem(problem_id), &token)
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn deleting_contest_cascades_config_deletion() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin21", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;

        app.put_with_token(
            &routes::contest_config_ns(contest_id, "test-plugin", "scoring"),
            &json!({"config": {"k": "v"}}),
            &token,
        )
        .await;

        let res = app
            .delete_with_token(&routes::contest(contest_id), &token)
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::contest_config_ns(contest_id, "test-plugin", "scoring"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn removing_contest_problem_cascades_config_deletion() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin22", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;
        let problem_id = app.create_problem(&token, "P1").await;
        app.add_problem_to_contest(contest_id, problem_id, &token)
            .await;

        app.put_with_token(
            &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
            &json!({"config": {"k": "v"}}),
            &token,
        )
        .await;

        let res = app
            .delete_with_token(&routes::contest_problem(contest_id, problem_id), &token)
            .await;
        assert_eq!(res.status, 204);

        let res = app
            .get_with_token(
                &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }

    #[tokio::test]
    async fn bulk_removing_contest_problems_cascades_config_deletion() {
        let app = TestApp::spawn_with_config_schemas().await;
        let token = app
            .create_user_with_role("admin23", "password123", "admin")
            .await;
        let contest_id = app.create_contest(&token, "C1", true, true).await;
        let problem_id = app.create_problem(&token, "P1").await;
        app.add_problem_to_contest(contest_id, problem_id, &token)
            .await;

        app.put_with_token(
            &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
            &json!({"config": {"k": "v"}}),
            &token,
        )
        .await;

        let res = app
            .delete_with_body_and_token(
                &routes::contest_problems_bulk(contest_id),
                &json!({"problem_ids": [problem_id]}),
                &token,
            )
            .await;
        assert_eq!(res.status, 200);

        // The bulk path must clear the association's config just like the single
        // remove does; otherwise re-adding the problem resurrects stale config.
        let res = app
            .get_with_token(
                &routes::contest_problem_config_ns(contest_id, problem_id, "test-plugin", "limits"),
                &token,
            )
            .await;
        assert_eq!(res.status, 404);
    }
}

/// Every config write is checked against the schema the plugin declares for
/// that namespace at that scope; nothing that breaks it is stored.
mod schema_validation {
    use super::*;
    use crate::common::TestResponse;

    struct Scoped {
        app: TestApp,
        token: String,
        problem_id: i32,
        contest_id: i32,
    }

    impl Scoped {
        async fn new(name: &str) -> Self {
            let app = TestApp::spawn_with_config_schemas().await;
            let token = app
                .create_user_with_role(name, "password123", "admin")
                .await;
            let problem_id = app.create_problem(&token, "P1").await;
            let contest_id = app.create_contest(&token, "C1", true, true).await;
            app.add_problem_to_contest(contest_id, problem_id, &token)
                .await;
            Self {
                app,
                token,
                problem_id,
                contest_id,
            }
        }

        /// The config route of `plugin/namespace` at each scope, by name.
        fn routes(&self, plugin: &str, namespace: &str) -> [(&'static str, String); 4] {
            [
                ("plugin", routes::plugin_global_config_ns(plugin, namespace)),
                (
                    "problem",
                    routes::problem_config_ns(self.problem_id, plugin, namespace),
                ),
                (
                    "contest",
                    routes::contest_config_ns(self.contest_id, plugin, namespace),
                ),
                (
                    "contest_problem",
                    routes::contest_problem_config_ns(
                        self.contest_id,
                        self.problem_id,
                        plugin,
                        namespace,
                    ),
                ),
            ]
        }

        async fn put(&self, route: &str, config: serde_json::Value) -> TestResponse {
            self.app
                .put_with_token(route, &json!({ "config": config }), &self.token)
                .await
        }

        async fn assert_nothing_stored(&self, route: &str) {
            let res = self.app.get_with_token(route, &self.token).await;
            assert_eq!(res.status, 404, "{route} stored a config: {}", res.text);
        }
    }

    fn field_paths(res: &TestResponse) -> Vec<String> {
        res.body["details"]["fields"]
            .as_array()
            .unwrap_or_else(|| panic!("no field errors: {}", res.text))
            .iter()
            .map(|f| f["path"].as_str().unwrap().to_string())
            .collect()
    }

    #[tokio::test]
    async fn a_config_that_breaks_its_schema_is_refused_at_every_scope() {
        let t = Scoped::new("sv_admin1").await;
        let [_, problem, contest, contest_problem] = t.routes("test-plugin", "penalty");
        let settings = routes::plugin_global_config_ns("my-plugin", "settings");
        for (route, config, path) in [
            (&settings, json!({"v": "1"}), "v"),
            (
                &problem.1,
                json!({"penalty_minutes": "20"}),
                "penalty_minutes",
            ),
            (
                &contest.1,
                json!({"penalty_minutes": -1}),
                "penalty_minutes",
            ),
            (
                &contest.1,
                json!({"penalty_minutes": 99999}),
                "penalty_minutes",
            ),
            (&contest_problem.1, json!({"mode": "bogus"}), "mode"),
            (&contest_problem.1, json!({"bogus": 1}), "bogus"),
        ] {
            let res = t.put(route, config.clone()).await;
            assert_eq!(res.status, 400, "{route} accepted {config}: {}", res.text);
            assert_eq!(res.body["code"], "VALIDATION_ERROR", "{}", res.text);
            assert_eq!(field_paths(&res), [path], "{route} {config}");
            t.assert_nothing_stored(route).await;
        }
    }

    #[tokio::test]
    async fn a_valid_partial_config_is_stored_as_given() {
        let t = Scoped::new("sv_admin2").await;
        for (_, route) in t.routes("test-plugin", "penalty").into_iter().skip(1) {
            let res = t.put(&route, json!({"penalty_minutes": 15})).await;
            assert_eq!(res.status, 200, "{route}: {}", res.text);
            assert_eq!(res.body["config"], json!({"penalty_minutes": 15}));
        }
    }

    #[tokio::test]
    async fn an_unknown_plugin_or_namespace_is_not_found_at_every_scope() {
        let t = Scoped::new("sv_admin3").await;
        for (plugin, namespace) in [
            ("nope", "x"),
            ("test-plugin", "nope"),
            ("my-plugin", "nope"),
        ] {
            for (scope, route) in t.routes(plugin, namespace) {
                let res = t.put(&route, json!({})).await;
                assert_eq!(
                    res.status, 404,
                    "{scope} {plugin}/{namespace}: {}",
                    res.text
                );
                assert_eq!(
                    res.body["code"], "CONFIG_NAMESPACE_NOT_FOUND",
                    "{}",
                    res.text
                );
            }
        }
    }

    #[tokio::test]
    async fn a_namespace_is_not_found_at_a_scope_it_is_not_declared_for() {
        let t = Scoped::new("sv_admin4").await;
        // `checker` is problem-only, `settings` plugin-only.
        let [plugin, _, contest, contest_problem] = t.routes("test-plugin", "checker");
        let [_, problem, _, _] = t.routes("my-plugin", "settings");
        for (scope, route) in [plugin, contest, contest_problem, problem] {
            let res = t.put(&route, json!({})).await;
            assert_eq!(res.status, 404, "{scope}: {}", res.text);
            assert_eq!(
                res.body["code"], "CONFIG_NAMESPACE_NOT_FOUND",
                "{}",
                res.text
            );
        }
    }
}
