use sea_orm::ConnectionTrait;
use sea_orm_migration::MigratorTrait;
use serde_json::json;

use crate::common::{TestApp, routes};

#[tokio::test]
async fn registration_stores_names_and_keeps_username_login() {
    let app = TestApp::spawn().await;
    for (username, name, expected) in [
        ("named", json!("  第一队 🌱  "), "第一队 🌱"),
        ("blank", json!("  "), "blank"),
        ("legacy", json!(null), "legacy"),
    ] {
        let registered = app
            .post_without_token(
                routes::REGISTER,
                &json!({
                    "username": username, "display_name": name, "password": "securepass"
                }),
            )
            .await;
        assert_eq!(registered.status, 201, "{}", registered.text);
        assert_eq!(registered.body["display_name"], expected);
        let login = app
            .post_without_token(
                routes::LOGIN,
                &json!({
                    "username": username, "password": "securepass"
                }),
            )
            .await;
        assert_eq!(login.status, 200, "{}", login.text);
        assert_eq!(login.body["username"], username);
        assert_eq!(login.body["display_name"], expected);
        let profile = app
            .get_with_token(routes::ME, login.body["token"].as_str().unwrap())
            .await;
        assert_eq!(profile.body["display_name"], expected);
    }
}

#[tokio::test]
async fn admin_and_contest_creation_share_names_without_overwriting_existing_accounts() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("admin", "securepass", "admin")
        .await;
    let single = app
        .post_with_token(
            routes::USERS,
            &json!({
                "username": "existing", "display_name": "原队名", "password": "original_pass"
            }),
            &admin,
        )
        .await;
    assert_eq!(single.status, 201, "{}", single.text);
    assert_eq!(single.body["display_name"], "原队名");

    let batch = app
        .post_with_token(
            "/api/v1/users/bulk",
            &json!({"users": [
                {"username": "team1", "display_name": "同名队"},
                {"username": "team2", "display_name": "同名队"},
                {"username": "default_name"}
            ]}),
            &admin,
        )
        .await;
    assert_eq!(batch.status, 201, "{}", batch.text);
    for (created, expected) in
        batch
            .body
            .as_array()
            .unwrap()
            .iter()
            .zip(["同名队", "同名队", "default_name"])
    {
        assert_eq!(created["display_name"], expected);
        let login = app
            .post_without_token(
                routes::LOGIN,
                &json!({
                    "username": created["username"], "password": created["password"]
                }),
            )
            .await;
        assert_eq!(login.status, 200, "{}", login.text);
    }

    let contest_id = app
        .create_contest(&admin, "Display names", true, true)
        .await;
    let manager = app
        .create_user_with_permissions("manager", "securepass", &["contest:manage"])
        .await;
    let enrolled = app
        .post_with_token(
            &routes::contest_participants_bulk(contest_id),
            &json!({"create_users": [
                {"username": "existing", "display_name": "不能覆盖", "password": "new_password"},
                {"username": "new_team", "display_name": "新队名"}
            ]}),
            &manager,
        )
        .await;
    assert_eq!(enrolled.status, 200, "{}", enrolled.text);
    assert_eq!(enrolled.body["added"][0]["display_name"], "原队名");
    assert_eq!(enrolled.body["created"][0]["display_name"], "新队名");
    let unchanged_login = app
        .post_without_token(
            routes::LOGIN,
            &json!({
                "username": "existing", "password": "original_pass"
            }),
        )
        .await;
    assert_eq!(unchanged_login.status, 200, "{}", unchanged_login.text);
    let participants = app
        .get_with_token(&routes::contest_participants(contest_id), &manager)
        .await;
    assert_eq!(participants.status, 200);
    assert!(
        participants
            .body
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["display_name"].is_string())
    );
}

#[tokio::test]
async fn only_user_managers_can_rename_and_names_do_not_follow_username_changes() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_permissions("admin", "securepass", &["user:manage"])
        .await;
    let player = app.create_authenticated_user("player", "securepass").await;
    let manager = app
        .create_user_with_permissions("manager", "securepass", &["contest:manage"])
        .await;
    let player_id = app.get_with_token(routes::ME, &player).await.id();
    for token in [&player, &manager] {
        let own_id = app.get_with_token(routes::ME, token).await.id();
        for id in [own_id, player_id] {
            let denied = app
                .patch_with_token(&routes::user(id), &json!({"display_name": "改名"}), token)
                .await;
            assert_eq!(denied.status, 403, "{}", denied.text);
        }
    }
    let admin_id = app.get_with_token(routes::ME, &admin).await.id();
    for id in [admin_id, player_id] {
        let updated = app
            .patch_with_token(
                &routes::user(id),
                &json!({"display_name": " 显示名 "}),
                &admin,
            )
            .await;
        assert_eq!(updated.status, 200, "{}", updated.text);
        assert_eq!(updated.body["display_name"], "显示名");
    }
    let renamed = app
        .patch_with_token(
            &routes::user(player_id),
            &json!({"username": "renamed"}),
            &admin,
        )
        .await;
    assert_eq!(renamed.status, 200);
    assert_eq!(renamed.body["display_name"], "显示名");
    let profile = app.get_with_token(routes::ME, &player).await;
    assert_eq!(profile.body["username"], "renamed");
    assert_eq!(profile.body["display_name"], "显示名");
}

#[tokio::test]
async fn invalid_names_reject_the_whole_write() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("admin", "securepass", "admin")
        .await;
    let id = app.get_with_token(routes::ME, &admin).await.id();
    let before = app.get_with_token(routes::USERS, &admin).await;
    for name in [
        json!(""),
        json!("  "),
        json!("张".repeat(65)),
        json!("bad\nname"),
        json!(123),
    ] {
        let rejected = app
            .patch_with_token(
                &routes::user(id),
                &json!({
                    "username": "must_not_change", "display_name": name
                }),
                &admin,
            )
            .await;
        assert_eq!(rejected.status, 400, "{}", rejected.text);
        assert_eq!(rejected.body["code"], "VALIDATION_ERROR");
    }
    let batch = app
        .post_with_token(
            "/api/v1/users/bulk",
            &json!({"users": [
                {"username": "must_not_exist"}, {"username": "invalid", "display_name": "bad\nname"}
            ]}),
            &admin,
        )
        .await;
    assert_eq!(batch.status, 400, "{}", batch.text);
    let users = app.get_with_token(routes::USERS, &admin).await;
    assert_eq!(users.body, before.body);
}

#[tokio::test]
async fn legacy_database_upgrade_and_restarts_preserve_account_names() {
    let app = TestApp::spawn().await;
    let token = app.create_authenticated_user("legacy", "securepass").await;
    server::migration::Migrator::down(&app.db, Some(1))
        .await
        .unwrap();
    let upgraded = server::database::init_db(&app.state.config.database.url)
        .await
        .unwrap();
    let profile = app.get_with_token(routes::ME, &token).await;
    assert_eq!(profile.status, 200, "{}", profile.text);
    assert_eq!(profile.body["display_name"], "legacy");
    upgraded
        .execute_unprepared(
            r#"UPDATE "user" SET display_name = '新名字' WHERE username = 'legacy'"#,
        )
        .await
        .unwrap();
    upgraded.close().await.unwrap();
    let restarted = server::database::init_db(&app.state.config.database.url)
        .await
        .unwrap();
    let login = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "legacy", "password": "securepass"}),
        )
        .await;
    assert_eq!(login.status, 200, "{}", login.text);
    assert_eq!(login.body["display_name"], "新名字");
    for invalid in ["NULL", "''", "'   '"] {
        assert!(
            restarted
                .execute_unprepared(&format!(r#"UPDATE "user" SET display_name = {invalid}"#))
                .await
                .is_err()
        );
    }
    restarted.close().await.unwrap();
}
