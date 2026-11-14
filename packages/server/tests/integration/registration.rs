//! Self-registration is closed on a fresh install, and an admin opens and
//! closes it for the whole fleet at runtime.

use crate::common::{SpawnOptions, TestApp, routes};
use serde_json::json;

const SETTING: &str = "/api/v1/admin/settings/registration";
const STATUS: &str = "/api/v1/auth/registration";

#[tokio::test]
async fn registration_is_closed_until_an_admin_opens_it_fleet_wide() {
    let closed = || SpawnOptions {
        registration_closed: true,
        ..Default::default()
    };
    let app = TestApp::spawn_with_options(closed()).await;
    let replica = app.spawn_replica(closed()).await;
    let alice = json!({ "username": "self_signup", "password": "password123" });

    let status = app.get_without_token(STATUS).await;
    assert_eq!(status.body, json!({ "open": false }), "{}", status.text);
    let refused = app.post_without_token(routes::REGISTER, &alice).await;
    assert_eq!(refused.status, 403, "{}", refused.text);
    assert_eq!(
        refused.body["code"], "REGISTRATION_CLOSED",
        "{}",
        refused.text
    );

    // The fixture makes its admin by registering, behind the API's back.
    server::services::registration::set_open(&app.db, true)
        .await
        .unwrap();
    let admin = app
        .create_user_with_role("reg_admin", "password123", "admin")
        .await;
    server::services::registration::set_open(&app.db, false)
        .await
        .unwrap();
    let opened = app
        .put_with_token(SETTING, &json!({ "open": true }), &admin)
        .await;
    assert_eq!(opened.status, 200, "{}", opened.text);
    // Another replica sees it at once.
    let created = replica.post_without_token(routes::REGISTER, &alice).await;
    assert_eq!(created.status, 201, "{}", created.text);

    let closed = app
        .put_with_token(SETTING, &json!({ "open": false }), &admin)
        .await;
    assert_eq!(closed.status, 200, "{}", closed.text);
    let bob = json!({ "username": "late_signup", "password": "password123" });
    let refused = replica.post_without_token(routes::REGISTER, &bob).await;
    assert_eq!(refused.status, 403, "{}", refused.text);
}

#[tokio::test]
async fn only_a_user_manager_switches_registration() {
    let app = TestApp::spawn().await;
    let contestant = app
        .create_user_with_role("reg_contestant", "password123", "contestant")
        .await;
    let res = app
        .put_with_token(SETTING, &json!({ "open": false }), &contestant)
        .await;
    assert_eq!(res.status, 403, "{}", res.text);
    assert_eq!(res.body["code"], "PERMISSION_DENIED", "{}", res.text);
}
