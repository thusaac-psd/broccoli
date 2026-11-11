//! The bootstrap admin is created once and then left alone, except on an
//! explicit or a no-admin-left recovery.

use broccoli_server_sdk::permissions as perm;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use serde_json::json;
use server::config::BootstrapConfig;
use server::entity::{user, user_role};

use crate::common::{TestApp, routes};

fn config(reset_admin: bool) -> BootstrapConfig {
    BootstrapConfig {
        admin_username: "boot".into(),
        admin_password: "configured-pass".into(),
        reset_admin,
    }
}

async fn boot(app: &TestApp, reset_admin: bool) {
    server::seed::ensure_bootstrap_admin(&app.db, &config(reset_admin))
        .await
        .expect("bootstrap");
}

async fn login_status(app: &TestApp, password: &str) -> u16 {
    app.post_without_token(
        routes::LOGIN,
        &json!({"username": "boot", "password": password}),
    )
    .await
    .status
}

async fn boot_id(app: &TestApp) -> i32 {
    user::Entity::find()
        .filter(user::Column::Username.eq("boot"))
        .one(&app.db)
        .await
        .unwrap()
        .unwrap()
        .id
}

async fn holds_admin_role(app: &TestApp) -> bool {
    user_role::Entity::find_by_id((boot_id(app).await, "admin".to_string()))
        .one(&app.db)
        .await
        .unwrap()
        .is_some()
}

/// Boot, then change the bootstrap admin's password through the API.
async fn boot_and_change_password(app: &TestApp) {
    boot(app, false).await;
    let token = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "boot", "password": "configured-pass"}),
        )
        .await
        .body["token"]
        .as_str()
        .unwrap()
        .to_string();
    let res = app
        .patch_with_token(
            &routes::user(boot_id(app).await),
            &json!({"password": "changed-in-ui"}),
            &token,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
}

#[tokio::test]
async fn creates_the_admin_when_missing() {
    let app = TestApp::spawn().await;
    boot(&app, false).await;
    assert_eq!(login_status(&app, "configured-pass").await, 200);
    assert!(holds_admin_role(&app).await);
}

#[tokio::test]
async fn a_restart_does_not_reset_a_changed_password_or_regrant_the_role() {
    let app = TestApp::spawn().await;
    boot_and_change_password(&app).await;

    // Another admin exists, and the bootstrap admin's role was revoked on
    // purpose.
    let other = app
        .create_user_with_role("other_admin", "password123", "admin")
        .await;
    let res = app
        .delete_with_token(&routes::user_role(boot_id(&app).await, "admin"), &other)
        .await;
    assert_eq!(res.status, 204, "{}", res.text);

    boot(&app, false).await;
    assert_eq!(login_status(&app, "changed-in-ui").await, 200);
    assert_ne!(login_status(&app, "configured-pass").await, 200);
    assert!(!holds_admin_role(&app).await, "role was re-granted");
}

#[tokio::test]
async fn recovers_when_no_active_user_holds_system_admin() {
    let app = TestApp::spawn().await;
    boot_and_change_password(&app).await;

    // Nobody holds system:admin any more (done behind the API's back, the way
    // a broken deployment could end up).
    app.db
        .execute_unprepared(&format!(
            "DELETE FROM role_permission WHERE permission = '{}'",
            perm::SYSTEM_ADMIN
        ))
        .await
        .unwrap();
    app.db
        .execute_unprepared(r#"DELETE FROM user_role WHERE role = 'admin'"#)
        .await
        .unwrap();

    boot(&app, false).await;
    assert_ne!(login_status(&app, "changed-in-ui").await, 200);
    let login = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": "boot", "password": "configured-pass"}),
        )
        .await;
    assert_eq!(login.status, 200, "{}", login.text);
    let permissions = login.body["permissions"].as_array().unwrap();
    for needed in [perm::SYSTEM_ADMIN, perm::USER_MANAGE, perm::ROLE_MANAGE] {
        assert!(
            permissions.iter().any(|p| p == needed),
            "recovered admin lacks {needed}: {permissions:?}"
        );
    }
}

#[tokio::test]
async fn reset_admin_forces_recovery_and_ends_old_sessions() {
    let app = TestApp::spawn().await;
    boot_and_change_password(&app).await;
    app.create_user_with_role("other_admin", "password123", "admin")
        .await;
    // A live session from before the reset.
    assert_eq!(login_status(&app, "changed-in-ui").await, 200);

    boot(&app, true).await;
    let sessions = server::entity::refresh_token::Entity::find()
        .filter(server::entity::refresh_token::Column::UserId.eq(boot_id(&app).await))
        .all(&app.db)
        .await
        .unwrap();
    assert!(sessions.is_empty(), "pre-reset sessions survived the reset");
    assert_eq!(login_status(&app, "configured-pass").await, 200);
    assert_ne!(login_status(&app, "changed-in-ui").await, 200);
}

#[tokio::test]
async fn a_deleted_bootstrap_account_is_not_recreated_while_an_admin_exists() {
    let app = TestApp::spawn().await;
    boot(&app, false).await;
    let other = app
        .create_user_with_role("other_admin", "password123", "admin")
        .await;
    let res = app
        .delete_with_token(&routes::user(boot_id(&app).await), &other)
        .await;
    assert_eq!(res.status, 204, "{}", res.text);

    boot(&app, false).await;
    assert_ne!(
        login_status(&app, "configured-pass").await,
        200,
        "the deleted bootstrap account came back"
    );
}
