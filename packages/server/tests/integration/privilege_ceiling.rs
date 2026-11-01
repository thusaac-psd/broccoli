//! Privilege ceiling: "you cannot grant or revoke what you do not hold".
//!
//! Both privilege-moving surfaces go through
//! `AuthUser::require_within_ceiling`:
//! - role-permission grant/revoke (`/roles/{role}/permissions`, gated by
//!   `role:manage`)
//! - user-role assign/revoke (`/users/{id}/roles`, gated by `user:manage`)
//!
//! Neither gate permission is root-equivalent. Only `system:admin` bypasses the
//! ceiling.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use server::entity::role_permission;

use crate::common::{TestApp, TestResponse, routes};

const PASSWORD: &str = "securepass";

/// The permissions stored for `role`, sorted.
async fn role_grants(app: &TestApp, role: &str) -> Vec<String> {
    let mut grants: Vec<String> = role_permission::Entity::find()
        .filter(role_permission::Column::Role.eq(role))
        .all(&app.db)
        .await
        .expect("query role_permission")
        .into_iter()
        .map(|rp| rp.permission)
        .collect();
    grants.sort();
    grants
}

/// Logs `username` in again and returns the permissions in the fresh token.
async fn login_permissions(app: &TestApp, username: &str) -> Vec<Value> {
    let login = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": username, "password": PASSWORD}),
        )
        .await;
    assert_eq!(login.status, 200, "re-login failed: {}", login.text);
    login.body["permissions"]
        .as_array()
        .expect("login response carries permissions")
        .clone()
}

fn assert_permission_denied(res: &TestResponse, what: &str) {
    assert_eq!(res.status, 403, "{what}: {}", res.text);
    assert_eq!(
        res.body["code"], "PERMISSION_DENIED",
        "{what}: {}",
        res.text
    );
}

mod role_permission_grant {
    use super::*;

    /// The escalation this ceiling closes: a `role:manage`-only principal
    /// grants `system:admin` to its own role, logs in again, and holds it.
    #[tokio::test]
    async fn role_manager_cannot_grant_system_admin_to_own_role() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("escalator", PASSWORD, &["role:manage"])
            .await;

        let res = app
            .post_with_token(
                &routes::role_permissions("escalator_role"),
                &json!({"permission": "system:admin"}),
                &token,
            )
            .await;
        assert_permission_denied(&res, "granting system:admin to own role");

        assert_eq!(role_grants(&app, "escalator_role").await, ["role:manage"]);
        let perms = login_permissions(&app, "escalator").await;
        assert!(
            !perms.contains(&json!("system:admin")),
            "re-login must not carry system:admin: {perms:?}"
        );
    }

    /// The ceiling is not specific to `system:admin`: any permission the caller
    /// lacks is refused, on any role, including custom permission strings that
    /// plugins check.
    #[tokio::test]
    async fn role_manager_cannot_grant_any_permission_it_lacks() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("lacker", PASSWORD, &["role:manage"])
            .await;

        for (role, permission) in [
            ("lacker_role", "user:manage"),
            ("contestant", "problem:create"),
            ("contestant", "experimental:feature"),
        ] {
            let res = app
                .post_with_token(
                    &routes::role_permissions(role),
                    &json!({"permission": permission}),
                    &token,
                )
                .await;
            assert_permission_denied(&res, &format!("granting {permission} to {role}"));
            assert!(
                !role_grants(&app, role)
                    .await
                    .contains(&permission.to_string()),
                "{permission} must not have been granted to {role}"
            );
        }
    }

    #[tokio::test]
    async fn role_manager_can_grant_permission_it_holds() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("holder", PASSWORD, &["role:manage", "problem:create"])
            .await;

        let res = app
            .post_with_token(
                &routes::role_permissions("contestant"),
                &json!({"permission": "problem:create"}),
                &token,
            )
            .await;
        assert_eq!(res.status, 201, "{}", res.text);
        assert_eq!(
            role_grants(&app, "contestant").await,
            ["problem:create", "submission:submit"]
        );
    }

    /// `system:admin` is the one root-equivalent grant, so it bypasses the
    /// ceiling for permissions its holder does not otherwise carry.
    #[tokio::test]
    async fn system_admin_bypasses_ceiling() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("root", PASSWORD, &["role:manage", "system:admin"])
            .await;

        let res = app
            .post_with_token(
                &routes::role_permissions("contestant"),
                &json!({"permission": "user:manage"}),
                &token,
            )
            .await;
        assert_eq!(res.status, 201, "{}", res.text);
        assert!(
            role_grants(&app, "contestant")
                .await
                .contains(&"user:manage".to_string())
        );
    }
}

mod role_permission_revoke {
    use super::*;

    /// The lockout half: a `role:manage`-only principal strips `system:admin`
    /// from the seeded `admin` role.
    #[tokio::test]
    async fn role_manager_cannot_revoke_system_admin_from_admin_role() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("stripper", PASSWORD, &["role:manage"])
            .await;

        let res = app
            .delete_with_token(&routes::role_permission("admin", "system:admin"), &token)
            .await;
        assert_permission_denied(&res, "revoking system:admin from admin");
        assert!(
            role_grants(&app, "admin")
                .await
                .contains(&"system:admin".to_string())
        );
    }

    #[tokio::test]
    async fn role_manager_cannot_revoke_any_permission_it_lacks() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("revoker", PASSWORD, &["role:manage"])
            .await;

        let res = app
            .delete_with_token(
                &routes::role_permission("problem_setter", "problem:edit"),
                &token,
            )
            .await;
        assert_permission_denied(&res, "revoking problem:edit from problem_setter");
        assert!(
            role_grants(&app, "problem_setter")
                .await
                .contains(&"problem:edit".to_string())
        );
    }

    /// The ceiling runs before the existence lookup, so a caller cannot use
    /// 404-vs-204 to probe which grants exist above its ceiling.
    #[tokio::test]
    async fn ceiling_refusal_precedes_not_found() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("prober", PASSWORD, &["role:manage"])
            .await;

        let res = app
            .delete_with_token(
                &routes::role_permission("contestant", "system:admin"),
                &token,
            )
            .await;
        assert_permission_denied(&res, "revoking an absent grant above the ceiling");
    }

    #[tokio::test]
    async fn role_manager_can_revoke_permission_it_holds() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("trimmer", PASSWORD, &["role:manage", "problem:edit"])
            .await;

        let res = app
            .delete_with_token(
                &routes::role_permission("problem_setter", "problem:edit"),
                &token,
            )
            .await;
        assert_eq!(res.status, 204, "{}", res.text);
        assert!(
            !role_grants(&app, "problem_setter")
                .await
                .contains(&"problem:edit".to_string())
        );
    }

    #[tokio::test]
    async fn system_admin_bypasses_ceiling() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("root", PASSWORD, &["role:manage", "system:admin"])
            .await;

        let res = app
            .delete_with_token(
                &routes::role_permission("problem_setter", "problem:edit"),
                &token,
            )
            .await;
        assert_eq!(res.status, 204, "{}", res.text);
    }
}

/// Pins the user-role ceiling, which now shares its check with the
/// role-permission handlers.
mod user_role_assignment {
    use super::*;

    #[tokio::test]
    async fn user_manager_cannot_assign_role_above_ceiling() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("assigner", PASSWORD, &["user:manage"])
            .await;
        let me = app.get_with_token(routes::ME, &token).await.id();

        let res = app
            .post_with_token(&routes::user_roles(me), &json!({"role": "admin"}), &token)
            .await;
        assert_permission_denied(&res, "assigning admin to self");
        let perms = login_permissions(&app, "assigner").await;
        assert!(
            !perms.contains(&json!("system:admin")),
            "re-login must not carry system:admin: {perms:?}"
        );
    }

    #[tokio::test]
    async fn user_manager_cannot_revoke_role_above_ceiling() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("demoter", PASSWORD, &["user:manage"])
            .await;
        let admin_token = app
            .create_user_with_role("an_admin", PASSWORD, "admin")
            .await;
        let admin_id = app.get_with_token(routes::ME, &admin_token).await.id();

        let res = app
            .delete_with_token(&routes::user_role(admin_id, "admin"), &token)
            .await;
        assert_permission_denied(&res, "revoking admin from an admin");
    }

    #[tokio::test]
    async fn user_manager_can_assign_role_within_ceiling() {
        let app = TestApp::spawn().await;
        let token = app
            .create_user_with_permissions("granter", PASSWORD, &["user:manage", "problem:create"])
            .await;
        // Creates `setter_role`, whose permission set is a subset of the
        // caller's own.
        app.create_user_with_permissions("setter", PASSWORD, &["problem:create"])
            .await;
        // Change someone else's roles: changing the caller's own would make
        // its token stale for the next request.
        let recipient_token = app.create_authenticated_user("recipient", PASSWORD).await;
        let recipient_id = app.get_with_token(routes::ME, &recipient_token).await.id();

        let res = app
            .post_with_token(
                &routes::user_roles(recipient_id),
                &json!({"role": "setter_role"}),
                &token,
            )
            .await;
        assert_eq!(res.status, 201, "{}", res.text);

        let res = app
            .delete_with_token(&routes::user_role(recipient_id, "setter_role"), &token)
            .await;
        assert_eq!(res.status, 204, "{}", res.text);
    }
}
