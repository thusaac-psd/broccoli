use serde_json::json;

use crate::common::{TestApp, routes};

mod list_users {
    use super::*;

    #[tokio::test]
    async fn admin_can_list_all_users_with_full_fields() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_user", "securepass", "admin")
            .await;
        let _alice_token = app.create_authenticated_user("alice", "securepass").await;

        let res = app.get_with_token(routes::USERS, &admin_token).await;

        assert_eq!(res.status, 200, "list users failed: {}", res.text);
        let users = res
            .body
            .as_array()
            .expect("users response should be an array");
        assert!(users.len() >= 2);

        let admin = users
            .iter()
            .find(|u| u["username"] == "admin_user")
            .expect("admin_user should exist");
        assert!(admin["roles"].as_array().unwrap().contains(&json!("admin")));
        assert!(admin["id"].is_number());
        assert!(admin["created_at"].is_string());
        // The user API must never expose the stored password hash (or any
        // password material) to clients.
        assert!(
            admin.get("password").is_none(),
            "user response must not include a password field"
        );

        let alice = users
            .iter()
            .find(|u| u["username"] == "alice")
            .expect("alice should exist");
        assert_eq!(alice["roles"], json!(["contestant"]));
    }

    #[tokio::test]
    async fn non_admin_cannot_list_all_users() {
        let app = TestApp::spawn().await;
        let token = app.create_authenticated_user("alice", "securepass").await;

        let res = app.get_with_token(routes::USERS, &token).await;

        assert_eq!(res.status, 403);
        assert_eq!(res.body["code"], "PERMISSION_DENIED");
    }

    #[tokio::test]
    async fn list_users_requires_token() {
        let app = TestApp::spawn().await;

        let res = app.get_without_token(routes::USERS).await;

        assert_eq!(res.status, 401);
        assert_eq!(res.body["code"], "TOKEN_MISSING");
    }
}

mod user_deletion {
    use super::*;

    #[tokio::test]
    async fn admin_can_soft_delete_user_and_hide_from_list() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_delete_1", "securepass", "admin")
            .await;
        let victim_token = app
            .create_authenticated_user("victim_delete_1", "securepass")
            .await;
        let victim_id = app.get_with_token(routes::ME, &victim_token).await.id();

        let delete_res = app
            .delete_with_token(&routes::user(victim_id), &admin_token)
            .await;
        assert_eq!(delete_res.status, 204, "delete failed: {}", delete_res.text);

        let list_res = app.get_with_token(routes::USERS, &admin_token).await;
        assert_eq!(list_res.status, 200, "list failed: {}", list_res.text);
        let users = list_res
            .body
            .as_array()
            .expect("users response should be an array");
        assert!(
            users.iter().all(|u| u["id"] != json!(victim_id)),
            "soft-deleted user should not be returned by /users"
        );
    }

    #[tokio::test]
    async fn soft_deleted_user_cannot_login_again() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_delete_2", "securepass", "admin")
            .await;
        let username = "victim_delete_2";
        let password = "securepass";
        let victim_token = app.create_authenticated_user(username, password).await;
        let victim_id = app.get_with_token(routes::ME, &victim_token).await.id();

        let delete_res = app
            .delete_with_token(&routes::user(victim_id), &admin_token)
            .await;
        assert_eq!(delete_res.status, 204, "delete failed: {}", delete_res.text);

        let login_res = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": username, "password": password}),
            )
            .await;
        assert_eq!(login_res.status, 401);
        assert_eq!(login_res.body["code"], "INVALID_CREDENTIALS");
    }

    #[tokio::test]
    async fn soft_deleted_user_cannot_refresh_token() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_delete_3", "securepass", "admin")
            .await;
        let username = "victim_delete_3";
        let password = "securepass";
        let victim_token = app.create_authenticated_user(username, password).await;
        let victim_id = app.get_with_token(routes::ME, &victim_token).await.id();

        let delete_res = app
            .delete_with_token(&routes::user(victim_id), &admin_token)
            .await;
        assert_eq!(delete_res.status, 204, "delete failed: {}", delete_res.text);

        let refresh_res = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(refresh_res.status, 401);
        assert_eq!(refresh_res.body["code"], "TOKEN_INVALID");
    }

    #[tokio::test]
    async fn can_register_same_username_after_soft_delete() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_delete_3", "securepass", "admin")
            .await;
        let username = "recyclable_user";
        let password = "securepass";

        let reg1 = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": username, "password": password}),
            )
            .await;
        assert_eq!(
            reg1.status, 201,
            "initial registration failed: {}",
            reg1.text
        );

        let login1 = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": username, "password": password}),
            )
            .await;
        assert_eq!(login1.status, 200, "initial login failed: {}", login1.text);
        let first_user_id = login1.body["id"]
            .as_i64()
            .expect("login response should contain user id") as i32;

        let delete_res = app
            .delete_with_token(&routes::user(first_user_id), &admin_token)
            .await;
        assert_eq!(delete_res.status, 204, "delete failed: {}", delete_res.text);

        let reg2 = app
            .post_without_token(
                routes::REGISTER,
                &json!({"username": username, "password": password}),
            )
            .await;
        assert_eq!(
            reg2.status, 201,
            "re-registration should be allowed after soft delete: {}",
            reg2.text
        );
        assert_ne!(reg2.body["id"], json!(first_user_id));
    }
}

mod user_modification {
    use super::*;

    #[tokio::test]
    async fn admin_can_update_user_password_and_login_works() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_mod", "securepass", "admin")
            .await;
        let victim_token = app.create_authenticated_user("victim", "old_pass").await;
        let victim_id = app.get_with_token(routes::ME, &victim_token).await.id();

        let res = app
            .patch_with_token(
                &routes::user(victim_id),
                &json!({"password": "new_secure_pass"}),
                &admin_token,
            )
            .await;
        assert_eq!(res.status, 200);

        let refresh_res = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(refresh_res.status, 401);
        assert_eq!(refresh_res.body["code"], "TOKEN_INVALID");

        let login_old = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "victim", "password": "old_pass"}),
            )
            .await;
        assert_eq!(login_old.status, 401);

        let login_new = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "victim", "password": "new_secure_pass"}),
            )
            .await;
        assert_eq!(login_new.status, 200);
    }

    #[tokio::test]
    async fn user_cannot_modify_themselves_without_permission() {
        let app = TestApp::spawn().await;
        let token = app
            .create_authenticated_user("self_hacker", "securepass")
            .await;
        let id = app.get_with_token(routes::ME, &token).await.id();

        let res = app
            .patch_with_token(&routes::user(id), &json!({"username": "new_name"}), &token)
            .await;

        assert_eq!(res.status, 403);
    }

    #[tokio::test]
    async fn admin_can_assign_and_unassign_roles() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("admin_role_mgr", "securepass", "admin")
            .await;
        let user_token = app
            .create_authenticated_user("role_victim", "securepass")
            .await;
        let user_id = app.get_with_token(routes::ME, &user_token).await.id();

        let assign_res = app
            .post_with_token(
                &routes::user_roles(user_id),
                &json!({"role": "problem_setter"}),
                &admin_token,
            )
            .await;
        assert_eq!(assign_res.status, 201);

        let refresh_res = app.post_without_token(routes::REFRESH, &json!({})).await;
        assert_eq!(refresh_res.status, 401);
        assert_eq!(refresh_res.body["code"], "TOKEN_INVALID");

        let login_res = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "role_victim", "password": "securepass"}),
            )
            .await;
        assert_eq!(login_res.status, 200);
        let roles = login_res.body["roles"].as_array().unwrap();
        assert!(roles.contains(&json!("problem_setter")));

        let unassign_res = app
            .delete_with_token(&routes::user_role(user_id, "problem_setter"), &admin_token)
            .await;
        assert_eq!(unassign_res.status, 204);

        let me_res_after = app.get_with_token(routes::ME, &user_token).await;
        assert!(
            !me_res_after.body["roles"]
                .as_array()
                .unwrap()
                .contains(&json!("problem_setter"))
        );
    }
}

mod role_management {
    use super::*;

    #[tokio::test]
    async fn admin_can_list_all_roles() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("super_admin", "securepass", "admin")
            .await;

        let res = app.get_with_token(routes::ROLES, &admin_token).await;
        assert_eq!(res.status, 200);
        let roles = res.body.as_array().unwrap();
        assert!(roles.contains(&json!("admin")));
        assert!(roles.contains(&json!("contestant")));
    }

    #[tokio::test]
    async fn admin_can_manage_role_permissions() {
        let app = TestApp::spawn().await;
        let admin_token = app
            .create_user_with_role("super_admin", "securepass", "admin")
            .await;
        let test_role = "contestant";

        let grant_res = app
            .post_with_token(
                &routes::role_permissions(test_role),
                &json!({"permission": "experimental:feature"}),
                &admin_token,
            )
            .await;
        assert_eq!(grant_res.status, 201);

        let list_res = app
            .get_with_token(&routes::role_permissions(test_role), &admin_token)
            .await;
        let perms = list_res.body.as_array().unwrap();
        assert!(perms.contains(&json!("experimental:feature")));

        let revoke_res = app
            .delete_with_token(
                &routes::role_permission(test_role, "experimental:feature"),
                &admin_token,
            )
            .await;
        assert_eq!(revoke_res.status, 204);

        let list_res_final = app
            .get_with_token(&routes::role_permissions(test_role), &admin_token)
            .await;
        assert!(
            !list_res_final
                .body
                .as_array()
                .unwrap()
                .contains(&json!("experimental:feature"))
        );
    }

    #[tokio::test]
    async fn regular_user_cannot_access_role_permissions() {
        let app = TestApp::spawn().await;
        let token = app.create_authenticated_user("peasant", "securepass").await;

        let res = app
            .get_with_token(&routes::role_permissions("admin"), &token)
            .await;
        assert_eq!(res.status, 403);
    }
}

mod user_creation {
    use super::*;
    use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};
    use server::entity::user;

    #[tokio::test]
    async fn manager_can_create_user_with_default_roles_and_supplied_password() {
        let app = TestApp::spawn().await;
        let manager = app
            .create_user_with_permissions("manager", "securepass", &["user:manage"])
            .await;
        let response = app
            .post_with_token(
                routes::USERS,
                &json!({"username": "  new_user  ", "password": " supplied_pass "}),
                &manager,
            )
            .await;
        assert_eq!(response.status, 201, "{}", response.text);
        assert_eq!(response.body["username"], "new_user");
        assert_eq!(response.body["roles"], json!(["contestant"]));
        assert!(response.body["created_at"].is_string());
        let login = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "new_user", "password": " supplied_pass "}),
            )
            .await;
        assert_eq!(login.status, 200, "{}", login.text);
        assert_eq!(login.body["roles"], json!(["contestant"]));
        let stored = user::Entity::find_by_id(response.id())
            .one(&app.db)
            .await
            .unwrap()
            .unwrap();
        assert_ne!(stored.password, " supplied_pass ");
        let list = app.get_with_token(routes::USERS, &manager).await;
        assert!(
            list.body
                .as_array()
                .unwrap()
                .iter()
                .all(|u| u.get("password").is_none())
        );
    }

    #[tokio::test]
    async fn bulk_create_returns_working_generated_and_supplied_credentials() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "securepass", "admin")
            .await;
        let response = app.post_with_token("/api/v1/users/bulk", &json!({"users": [
            {"username": "generated"}, {"username": "supplied", "password": "custom_pass123"}
        ]}), &admin).await;
        assert_eq!(response.status, 201, "{}", response.text);
        let users = response.body.as_array().unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0]["password"].as_str().unwrap().len(), 16);
        assert_eq!(users[1]["password"], "custom_pass123");
        for user in users {
            let login = app
                .post_without_token(
                    routes::LOGIN,
                    &json!({"username": user["username"], "password": user["password"]}),
                )
                .await;
            assert_eq!(login.status, 200, "{}", login.text);
            assert_eq!(login.body["roles"], json!(["contestant"]));
        }
    }

    #[tokio::test]
    async fn creation_requires_user_manage_and_authentication() {
        let app = TestApp::spawn().await;
        let contestant = app
            .create_authenticated_user("contestant", "securepass")
            .await;
        let role_manager = app
            .create_user_with_permissions("role_manager", "securepass", &["role:manage"])
            .await;
        for (path, body) in [
            (routes::USERS, json!({"username": "forbidden"})),
            (
                "/api/v1/users/bulk",
                json!({"users": [{"username": "forbidden"}]}),
            ),
        ] {
            assert_eq!(app.post_without_token(path, &body).await.status, 401);
            for token in [&contestant, &role_manager] {
                let response = app.post_with_token(path, &body, token).await;
                assert_eq!(response.status, 403, "{}", response.text);
                assert_eq!(response.body["code"], "PERMISSION_DENIED");
            }
        }
        assert_eq!(
            user::Entity::find()
                .filter(user::Column::Username.eq("forbidden"))
                .count(&app.db)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn invalid_batches_create_no_users() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "securepass", "admin")
            .await;
        let invalid_batches = vec![
            json!([]),
            json!([
                {"username": "valid"}, {"username": "bad-name"}
            ]),
            json!([
                {"username": "duplicate"}, {"username": " Duplicate "}
            ]),
            json!([
                {"username": "valid"}, {"username": "other", "password": "short"}
            ]),
            json!([
                {"username": "valid"}, {"username": "other", "password": "é".repeat(65)}
            ]),
            json!(
                (0..101)
                    .map(|i| json!({"username": format!("user_{i}")}))
                    .collect::<Vec<_>>()
            ),
        ];
        for users in invalid_batches {
            let response = app
                .post_with_token("/api/v1/users/bulk", &json!({"users": users}), &admin)
                .await;
            assert_eq!(response.status, 400, "{}", response.text);
            assert_eq!(response.body["code"], "VALIDATION_ERROR");
        }
        assert_eq!(user::Entity::find().count(&app.db).await.unwrap(), 1);
        for body in [
            json!({"username": ""}),
            json!({"username": "x".repeat(33)}),
            json!({"username": "invalid name"}),
            json!({"username": "valid", "password": ""}),
        ] {
            assert_eq!(
                app.post_with_token(routes::USERS, &body, &admin)
                    .await
                    .status,
                400
            );
        }
    }

    #[tokio::test]
    async fn existing_user_conflicts_never_modify_credentials_or_create_partial_batch() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "securepass", "admin")
            .await;
        app.create_authenticated_user("existing", "original_pass")
            .await;
        for (path, body) in [
            (
                routes::USERS,
                json!({"username": "existing", "password": "replacement_pass"}),
            ),
            (
                "/api/v1/users/bulk",
                json!({"users": [
                    {"username": "fresh"}, {"username": "existing", "password": "replacement_pass"}
                ]}),
            ),
        ] {
            let response = app.post_with_token(path, &body, &admin).await;
            assert_eq!(response.status, 409, "{}", response.text);
            assert_eq!(response.body["code"], "USERNAME_TAKEN");
        }
        assert!(
            user::Entity::find()
                .filter(user::Column::Username.eq("fresh"))
                .one(&app.db)
                .await
                .unwrap()
                .is_none()
        );
        let login = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "existing", "password": "original_pass"}),
            )
            .await;
        assert_eq!(login.status, 200);
    }

    #[tokio::test]
    async fn failed_role_assignment_rolls_back_new_account() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "securepass", "admin")
            .await;
        server::entity::role_permission::Entity::delete_many()
            .filter(server::entity::role_permission::Column::Role.eq("contestant"))
            .exec(&app.db)
            .await
            .unwrap();
        server::entity::role::Entity::delete_by_id("contestant")
            .exec(&app.db)
            .await
            .unwrap();
        let response = app
            .post_with_token(routes::USERS, &json!({"username": "rolled_back"}), &admin)
            .await;
        assert_eq!(response.status, 500, "{}", response.text);
        assert!(
            user::Entity::find()
                .filter(user::Column::Username.eq("rolled_back"))
                .one(&app.db)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn bulk_insert_failure_rolls_back_earlier_accounts() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "securepass", "admin")
            .await;
        // Simulate a constraint failure after the early username check, on the
        // second insert, to exercise rollback of the first account and its roles.
        app.db.execute_raw(Statement::from_string(DbBackend::Postgres, r#"
            CREATE FUNCTION reject_late_account() RETURNS trigger AS $$
            BEGIN
                IF NEW.username = 'late_conflict' THEN
                    RAISE EXCEPTION 'simulated concurrent username collision' USING ERRCODE = '23505';
                END IF;
                RETURN NEW;
            END;
            $$ LANGUAGE plpgsql;
        "#)).await.unwrap();
        app.db
            .execute_raw(Statement::from_string(
                DbBackend::Postgres,
                r#"
            CREATE TRIGGER reject_late_account BEFORE INSERT ON "user"
            FOR EACH ROW EXECUTE FUNCTION reject_late_account();
        "#,
            ))
            .await
            .unwrap();
        let response = app
            .post_with_token(
                "/api/v1/users/bulk",
                &json!({"users": [
                    {"username": "first_account"}, {"username": "late_conflict"}
                ]}),
                &admin,
            )
            .await;
        assert_eq!(response.status, 409, "{}", response.text);
        assert_eq!(response.body["code"], "USERNAME_TAKEN");
        assert_eq!(user::Entity::find().count(&app.db).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn stale_manager_token_cannot_create_accounts() {
        use sea_orm::{ActiveModelTrait, Set};
        let app = TestApp::spawn().await;
        let manager = app
            .create_user_with_permissions("manager", "securepass", &["user:manage"])
            .await;
        let id = app.get_with_token(routes::ME, &manager).await.id();
        let mut account: user::ActiveModel = user::Entity::find_by_id(id)
            .one(&app.db)
            .await
            .unwrap()
            .unwrap()
            .into();
        account.credentials_changed_at = Set(chrono::Utc::now() + chrono::Duration::seconds(5));
        account.update(&app.db).await.unwrap();
        for (path, body) in [
            (routes::USERS, json!({"username": "forbidden"})),
            (
                "/api/v1/users/bulk",
                json!({"users": [{"username": "forbidden"}]}),
            ),
        ] {
            let response = app.post_with_token(path, &body, &manager).await;
            assert_eq!(response.status, 401, "{}", response.text);
            assert_eq!(response.body["code"], "TOKEN_INVALID");
        }
    }

    #[tokio::test]
    async fn admin_can_reuse_a_soft_deleted_username() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("admin", "securepass", "admin")
            .await;
        let old = app
            .create_authenticated_user("reusable", "original_pass")
            .await;
        let id = app.get_with_token(routes::ME, &old).await.id();
        assert_eq!(
            app.delete_with_token(&routes::user(id), &admin)
                .await
                .status,
            204
        );
        let response = app
            .post_with_token(routes::USERS, &json!({"username": "reusable"}), &admin)
            .await;
        assert_eq!(response.status, 201, "{}", response.text);
        assert_ne!(response.id(), id);
    }
}

/// Looks up the id of the user named `username`.
async fn user_id(app: &TestApp, username: &str) -> i32 {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    use server::entity::user;

    user::Entity::find()
        .filter(user::Column::Username.eq(username))
        .one(&app.db)
        .await
        .expect("db")
        .expect("user exists")
        .id
}

mod account_management_ceiling {
    use super::*;
    use broccoli_server_sdk::permissions as perm;

    /// A `user:manage` holder cannot reset the password of a user whose roles exceed
    /// its own permissions, so it cannot log in as an admin. Update and delete apply
    /// the same privilege ceiling as role assignment.
    #[tokio::test]
    async fn user_manager_cannot_take_over_an_admin() {
        let app = TestApp::spawn().await;
        let mgr = app
            .create_user_with_permissions("aa_mgr", "password123", &[perm::USER_MANAGE])
            .await;
        app.create_user_with_role("aa_boss", "password123", "admin")
            .await;
        let boss = user_id(&app, "aa_boss").await;

        let res = app
            .patch_with_token(
                &routes::user(boss),
                &json!({"password": "takeover123"}),
                &mgr,
            )
            .await;
        assert_eq!(
            res.status, 403,
            "user:manage reset an admin's password: {}",
            res.text
        );

        let login = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "aa_boss", "password": "takeover123"}),
            )
            .await;
        assert_ne!(login.status, 200, "attacker logged in as the admin");
    }

    /// A `user:manage` holder cannot delete a user whose roles exceed its own
    /// permissions.
    #[tokio::test]
    async fn user_manager_cannot_delete_an_admin() {
        let app = TestApp::spawn().await;
        let mgr = app
            .create_user_with_permissions("aa_mgr", "password123", &[perm::USER_MANAGE])
            .await;
        app.create_user_with_role("aa_boss", "password123", "admin")
            .await;
        let boss = user_id(&app, "aa_boss").await;

        let res = app.delete_with_token(&routes::user(boss), &mgr).await;
        assert_eq!(
            res.status, 403,
            "user:manage deleted an admin: {}",
            res.text
        );
    }

    /// The ceiling allows what the caller does hold: a user manager can still
    /// reset a contestant's password and delete a contestant.
    #[tokio::test]
    async fn user_manager_can_manage_accounts_within_its_ceiling() {
        let app = TestApp::spawn().await;
        let mgr = app
            .create_user_with_permissions(
                "aa_mgr",
                "password123",
                &[perm::USER_MANAGE, perm::SUBMISSION_SUBMIT],
            )
            .await;
        app.create_authenticated_user("aa_pupil", "password123")
            .await;
        let pupil = user_id(&app, "aa_pupil").await;

        let res = app
            .patch_with_token(
                &routes::user(pupil),
                &json!({"password": "newpass123"}),
                &mgr,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        let res = app.delete_with_token(&routes::user(pupil), &mgr).await;
        assert_eq!(res.status, 204, "{}", res.text);
    }
}

mod user_update_validation {
    use super::*;

    /// Renaming a user to a username another account holds is a 409
    /// `USERNAME_TAKEN`, not a server error.
    #[tokio::test]
    async fn update_user_to_taken_username_is_a_conflict() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("aa_admin", "password123", "admin")
            .await;
        app.create_authenticated_user("aa_alice", "password123")
            .await;
        app.create_authenticated_user("aa_bob", "password123").await;
        let bob = user_id(&app, "aa_bob").await;

        let res = app
            .patch_with_token(&routes::user(bob), &json!({"username": "aa_alice"}), &admin)
            .await;
        assert_eq!(res.status, 409, "{}", res.text);
        assert_eq!(res.body["code"], "USERNAME_TAKEN");
    }

    /// `PATCH /users/{id}` validates the username and password with the same rules
    /// as registration and trims surrounding whitespace from the username.
    #[tokio::test]
    async fn update_user_validates_like_registration() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("aa_admin", "password123", "admin")
            .await;
        app.create_authenticated_user("aa_carol", "password123")
            .await;
        let carol = user_id(&app, "aa_carol").await;

        for body in [json!({"password": "x"}), json!({"username": "has space!"})] {
            let res = app
                .patch_with_token(&routes::user(carol), &body, &admin)
                .await;
            assert_eq!(res.status, 400, "{body} accepted: {}", res.text);
        }

        // Surrounding whitespace is trimmed, as at registration and login.
        let res = app
            .patch_with_token(
                &routes::user(carol),
                &json!({"username": " aa_carol2 "}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(res.body["username"], "aa_carol2");
        let login = app
            .post_without_token(
                routes::LOGIN,
                &json!({"username": "aa_carol2", "password": "password123"}),
            )
            .await;
        assert_eq!(login.status, 200, "{}", login.text);
    }
}
