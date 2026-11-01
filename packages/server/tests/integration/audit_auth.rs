//! Regression tests for the user-management privilege ceiling (kernel audit
//! 2026-11: B3, B16, B18).
//!
//! `assign_role`/`revoke_role` refuse to touch a role holding a permission the
//! caller lacks. `update_user`/`delete_user` used to check only `user:manage`,
//! so a `user:manage`-only principal could reset an admin's password and log in
//! as them, or soft-delete every admin. They now apply the same subset rule to
//! the target account's effective permissions.

use serde_json::json;

use crate::common::{TestApp, TestResponse, routes};

async fn login(app: &TestApp, username: &str, password: &str) -> TestResponse {
    app.post_without_token(
        routes::LOGIN,
        &json!({"username": username, "password": password}),
    )
    .await
}

async fn user_id(app: &TestApp, token: &str) -> i32 {
    app.get_with_token(routes::ME, token).await.id()
}

#[tokio::test]
async fn user_manager_cannot_take_over_an_admin() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("the_admin", "admin_pass", "admin")
        .await;
    let admin_id = user_id(&app, &admin).await;
    let manager = app
        .create_user_with_permissions("manager", "manager_pass", &["user:manage"])
        .await;

    for body in [
        json!({"password": "hijacked_pass"}),
        json!({"username": "renamed_admin"}),
        json!({"display_name": "Renamed"}),
    ] {
        let res = app
            .patch_with_token(&routes::user(admin_id), &body, &manager)
            .await;
        assert_eq!(
            res.status, 403,
            "PATCH {body} should be refused: {}",
            res.text
        );
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    assert_eq!(login(&app, "the_admin", "hijacked_pass").await.status, 401);
    let original = login(&app, "the_admin", "admin_pass").await;
    assert_eq!(
        original.status, 200,
        "admin credentials changed: {}",
        original.text
    );
    assert_eq!(original.body["display_name"], "the_admin");
}

#[tokio::test]
async fn user_manager_cannot_delete_an_admin() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("the_admin", "admin_pass", "admin")
        .await;
    let admin_id = user_id(&app, &admin).await;
    let manager = app
        .create_user_with_permissions("manager", "manager_pass", &["user:manage"])
        .await;

    let res = app
        .delete_with_token(&routes::user(admin_id), &manager)
        .await;
    assert_eq!(res.status, 403, "{}", res.text);
    assert_eq!(res.body["code"], "PERMISSION_DENIED");

    assert_eq!(login(&app, "the_admin", "admin_pass").await.status, 200);
}

/// The ceiling is a subset rule over the target's effective permissions, not a
/// special case for `admin`: a target holding any permission the caller lacks
/// is out of reach.
#[tokio::test]
async fn user_manager_cannot_modify_a_user_holding_a_permission_it_lacks() {
    let app = TestApp::spawn().await;
    let setter = app
        .create_user_with_role("setter", "setter_pass", "problem_setter")
        .await;
    let setter_id = user_id(&app, &setter).await;
    let manager = app
        .create_user_with_permissions(
            "manager",
            "manager_pass",
            &["user:manage", "submission:submit"],
        )
        .await;

    let patch = app
        .patch_with_token(
            &routes::user(setter_id),
            &json!({"password": "hijacked_pass"}),
            &manager,
        )
        .await;
    assert_eq!(patch.status, 403, "{}", patch.text);
    assert_eq!(patch.body["code"], "PERMISSION_DENIED");

    let delete = app
        .delete_with_token(&routes::user(setter_id), &manager)
        .await;
    assert_eq!(delete.status, 403, "{}", delete.text);
    assert_eq!(delete.body["code"], "PERMISSION_DENIED");

    assert_eq!(login(&app, "setter", "setter_pass").await.status, 200);
}

#[tokio::test]
async fn user_manager_can_still_manage_users_within_its_ceiling() {
    let app = TestApp::spawn().await;
    // A contestant holds `submission:submit`, so the manager must too.
    let manager = app
        .create_user_with_permissions(
            "manager",
            "manager_pass",
            &["user:manage", "submission:submit"],
        )
        .await;
    let manager_id = user_id(&app, &manager).await;
    let contestant = app
        .create_authenticated_user("contestant", "old_pass_123")
        .await;
    let contestant_id = user_id(&app, &contestant).await;

    let reset = app
        .patch_with_token(
            &routes::user(contestant_id),
            &json!({"password": "new_pass_123"}),
            &manager,
        )
        .await;
    assert_eq!(reset.status, 200, "{}", reset.text);
    assert_eq!(login(&app, "contestant", "new_pass_123").await.status, 200);

    // Acting on oneself never crosses one's own ceiling.
    let rename_self = app
        .patch_with_token(
            &routes::user(manager_id),
            &json!({"display_name": "Manager"}),
            &manager,
        )
        .await;
    assert_eq!(rename_self.status, 200, "{}", rename_self.text);

    let delete = app
        .delete_with_token(&routes::user(contestant_id), &manager)
        .await;
    assert_eq!(delete.status, 204, "{}", delete.text);
}

#[tokio::test]
async fn system_admin_bypasses_the_ceiling() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("the_admin", "admin_pass", "admin")
        .await;
    // Holds a permission the admin role does not.
    let exotic = app
        .create_user_with_permissions("exotic", "exotic_pass", &["experimental:feature"])
        .await;
    let exotic_id = user_id(&app, &exotic).await;

    let reset = app
        .patch_with_token(
            &routes::user(exotic_id),
            &json!({"password": "new_pass_123"}),
            &admin,
        )
        .await;
    assert_eq!(reset.status, 200, "{}", reset.text);
    let delete = app
        .delete_with_token(&routes::user(exotic_id), &admin)
        .await;
    assert_eq!(delete.status, 204, "{}", delete.text);
}

#[tokio::test]
async fn last_system_admin_cannot_be_deleted() {
    let app = TestApp::spawn().await;
    let first = app
        .create_user_with_role("first_admin", "admin_pass", "admin")
        .await;
    let first_id = user_id(&app, &first).await;

    let res = app.delete_with_token(&routes::user(first_id), &first).await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "CONFLICT");
    assert_eq!(login(&app, "first_admin", "admin_pass").await.status, 200);

    // With a second admin, one may go - after which the survivor is the last.
    let second = app
        .create_user_with_role("second_admin", "admin_pass", "admin")
        .await;
    let second_id = user_id(&app, &second).await;
    let res = app
        .delete_with_token(&routes::user(first_id), &second)
        .await;
    assert_eq!(res.status, 204, "{}", res.text);

    let res = app
        .delete_with_token(&routes::user(second_id), &second)
        .await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "CONFLICT");
    assert_eq!(login(&app, "second_admin", "admin_pass").await.status, 200);
}

/// Two admins deleting each other at once must not both succeed: deletions are
/// serialized, so the second sees the first's commit and refuses (409), or its
/// caller is already gone (401).
#[tokio::test]
async fn concurrent_admin_deletions_leave_one_admin() {
    let app = TestApp::spawn().await;
    let a = app
        .create_user_with_role("admin_a", "admin_pass", "admin")
        .await;
    let b = app
        .create_user_with_role("admin_b", "admin_pass", "admin")
        .await;
    let (a_path, b_path) = (
        routes::user(user_id(&app, &a).await),
        routes::user(user_id(&app, &b).await),
    );

    let (a_deletes_b, b_deletes_a) = tokio::join!(
        app.delete_with_token(&b_path, &a),
        app.delete_with_token(&a_path, &b),
    );
    let statuses = [a_deletes_b.status, b_deletes_a.status];
    assert_eq!(
        statuses.iter().filter(|&&s| s == 204).count(),
        1,
        "exactly one deletion should win: {} / {}",
        a_deletes_b.text,
        b_deletes_a.text
    );
    assert!(
        statuses.iter().all(|s| [204, 401, 409].contains(s)),
        "unexpected statuses {statuses:?}: {} / {}",
        a_deletes_b.text,
        b_deletes_a.text
    );
    let survivors = [
        login(&app, "admin_a", "admin_pass").await.status,
        login(&app, "admin_b", "admin_pass").await.status,
    ];
    assert_eq!(survivors.iter().filter(|&&s| s == 200).count(), 1);
}

#[tokio::test]
async fn update_user_validates_username_and_password() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("the_admin", "admin_pass", "admin")
        .await;
    let victim = app.create_authenticated_user("victim", "victim_pass").await;
    let victim_id = user_id(&app, &victim).await;

    for body in [
        json!({"username": ""}),
        json!({"username": "   "}),
        json!({"username": "has space"}),
        json!({"username": "bad-name"}),
        json!({"username": "x".repeat(33)}),
        json!({"password": ""}),
        json!({"password": "short"}),
        json!({"password": "x".repeat(129)}),
    ] {
        let res = app
            .patch_with_token(&routes::user(victim_id), &body, &admin)
            .await;
        assert_eq!(res.status, 400, "PATCH {body}: {}", res.text);
        assert_eq!(res.body["code"], "VALIDATION_ERROR");
    }
    assert_eq!(login(&app, "victim", "victim_pass").await.status, 200);

    // Valid usernames are stored trimmed, matching account creation.
    let res = app
        .patch_with_token(
            &routes::user(victim_id),
            &json!({"username": "  renamed_victim  "}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
    assert_eq!(res.body["username"], "renamed_victim");
}

#[tokio::test]
async fn update_user_to_a_taken_username_is_a_conflict() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("the_admin", "admin_pass", "admin")
        .await;
    app.create_authenticated_user("alice", "alice_pass").await;
    let bob = app.create_authenticated_user("bob", "bob_pass_1").await;
    let bob_id = user_id(&app, &bob).await;

    let res = app
        .patch_with_token(&routes::user(bob_id), &json!({"username": "alice"}), &admin)
        .await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "USERNAME_TAKEN");

    // Re-saving one's current username is not a conflict.
    let res = app
        .patch_with_token(&routes::user(bob_id), &json!({"username": "bob"}), &admin)
        .await;
    assert_eq!(res.status, 200, "{}", res.text);
}

#[tokio::test]
async fn assigning_an_already_held_role_is_a_conflict() {
    let app = TestApp::spawn().await;
    let admin = app
        .create_user_with_role("the_admin", "admin_pass", "admin")
        .await;
    let alice = app.create_authenticated_user("alice", "alice_pass").await;
    let alice_id = user_id(&app, &alice).await;

    let res = app
        .post_with_token(
            &routes::user_roles(alice_id),
            &json!({"role": "contestant"}),
            &admin,
        )
        .await;
    assert_eq!(res.status, 409, "{}", res.text);
    assert_eq!(res.body["code"], "CONFLICT");
}
