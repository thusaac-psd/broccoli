//! Lockout guards on the authorization tables.
//!
//! Soft-deleting a user, revoking a role from a user, and revoking a
//! permission from a role can each leave no active user holding
//! `system:admin`. All three go through
//! `services::privileges::SystemAdminLock`, which refuses that outcome with 409
//! `CONFLICT` and serializes the three writes on one advisory lock.

use std::future::Future;
use std::time::Duration;

use broccoli_server_sdk::permissions as perm;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, Database, DbBackend, EntityTrait, Set, Statement,
    TransactionTrait,
};
use serde_json::json;
use server::entity::{role, role_permission, user_role};
use server::services::privileges::SystemAdminLock;

use crate::common::{TestApp, TestResponse, routes};

const PASSWORD: &str = "securepass";

async fn user_id(app: &TestApp, token: &str) -> i32 {
    app.get_with_token(routes::ME, token).await.id()
}

async fn login(app: &TestApp, username: &str) -> String {
    let res = app
        .post_without_token(
            routes::LOGIN,
            &json!({"username": username, "password": PASSWORD}),
        )
        .await;
    assert_eq!(res.status, 200, "login failed: {}", res.text);
    res.body["token"].as_str().unwrap().to_string()
}

/// Active users holding `system:admin` through any role, read straight from
/// the tables so the oracle does not share code with the guard under test.
async fn active_admin_ids(app: &TestApp) -> Vec<i32> {
    app.db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT DISTINCT u.id FROM "user" u
               JOIN user_role ur ON ur.user_id = u.id
               JOIN role_permission rp ON rp.role = ur.role
               WHERE u.deleted_at IS NULL AND rp.permission = $1
               ORDER BY u.id"#,
            [perm::SYSTEM_ADMIN.into()],
        ))
        .await
        .unwrap()
        .iter()
        .map(|row| row.try_get::<i32>("", "id").unwrap())
        .collect()
}

async fn role_grants(app: &TestApp, role_name: &str, permission: &str) -> bool {
    role_permission::Entity::find_by_id((role_name.to_string(), permission.to_string()))
        .one(&app.db)
        .await
        .unwrap()
        .is_some()
}

async fn user_has_role(app: &TestApp, user_id: i32, role_name: &str) -> bool {
    user_role::Entity::find_by_id((user_id, role_name.to_string()))
        .one(&app.db)
        .await
        .unwrap()
        .is_some()
}

#[track_caller]
fn assert_conflict(res: &TestResponse) {
    assert_eq!(res.status, 409, "expected 409, got: {}", res.text);
    assert_eq!(res.body["code"], "CONFLICT", "body: {}", res.text);
}

/// Holds the admin-set advisory lock from a connection outside the app's
/// pool, starts both requests, and waits until Postgres shows both of them
/// queued on an advisory lock in this test's database. Only then does it
/// release the lock and let them finish.
///
/// Neither request can commit before the other has passed authentication,
/// so the outcome depends on the guard alone. Without the lock, both
/// requests would finish while the test still holds it, which fails here.
async fn race_behind_admin_lock(
    app: &TestApp,
    first: impl Future<Output = TestResponse>,
    second: impl Future<Output = TestResponse>,
) -> [TestResponse; 2] {
    let blocker_db = Database::connect(&app.state.config.database.url)
        .await
        .unwrap();
    let blocker = blocker_db.begin().await.unwrap();
    // The lock belongs to `blocker`'s transaction; the rollback below frees it.
    SystemAdminLock::acquire(&blocker).await.unwrap();

    let both = async { tokio::join!(first, second) };
    tokio::pin!(both);

    let queued = async {
        loop {
            let waiting: i64 = blocker
                .query_one_raw(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT count(*) AS waiting FROM pg_locks
                     WHERE locktype = 'advisory' AND NOT granted
                       AND database = (SELECT oid FROM pg_database WHERE datname = current_database())",
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "waiting")
                .unwrap();
            if waiting >= 2 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };

    tokio::select! {
        _ = &mut both => panic!("an admin-set write finished while the advisory lock was held"),
        queued = tokio::time::timeout(Duration::from_secs(15), queued) => {
            queued.expect("both writes should queue on the admin-set advisory lock");
        }
    }

    blocker.rollback().await.unwrap();
    let (first, second) = both.await;
    blocker_db.close().await.unwrap();
    [first, second]
}

#[track_caller]
fn assert_one_won(results: &[TestResponse; 2]) {
    let mut statuses = [results[0].status, results[1].status];
    statuses.sort_unstable();
    assert_eq!(
        statuses,
        [204, 409],
        "exactly one write should win: {} / {}",
        results[0].text,
        results[1].text
    );
    let loser = results.iter().find(|r| r.status == 409).unwrap();
    assert_eq!(loser.body["code"], "CONFLICT", "body: {}", loser.text);
}

mod revoke_role {
    use super::*;

    #[tokio::test]
    async fn sole_admin_cannot_revoke_own_admin_role() {
        let app = TestApp::spawn().await;
        let token = app.create_user_with_role("solo", PASSWORD, "admin").await;
        let id = user_id(&app, &token).await;

        let res = app
            .delete_with_token(&routes::user_role(id, "admin"), &token)
            .await;

        assert_conflict(&res);
        assert!(user_has_role(&app, id, "admin").await);
        assert_eq!(active_admin_ids(&app).await, vec![id]);
    }

    #[tokio::test]
    async fn admin_role_is_revocable_until_one_holder_remains() {
        let app = TestApp::spawn().await;
        let a_token = app.create_user_with_role("adm_a", PASSWORD, "admin").await;
        let b_token = app.create_user_with_role("adm_b", PASSWORD, "admin").await;
        let a = user_id(&app, &a_token).await;
        let b = user_id(&app, &b_token).await;

        let res = app
            .delete_with_token(&routes::user_role(b, "admin"), &a_token)
            .await;
        assert_eq!(res.status, 204, "revoke failed: {}", res.text);

        let res = app
            .delete_with_token(&routes::user_role(a, "admin"), &a_token)
            .await;
        assert_conflict(&res);
        assert_eq!(active_admin_ids(&app).await, vec![a]);
    }

    /// The guard counts grant paths, not role names: dropping one role that
    /// carries `system:admin` is fine while another still grants it.
    #[tokio::test]
    async fn holder_keeps_system_admin_through_another_role() {
        let app = TestApp::spawn().await;
        let token = app.create_user_with_role("dual", PASSWORD, "admin").await;
        let id = user_id(&app, &token).await;
        role::ActiveModel {
            name: Set("root".into()),
        }
        .insert(&app.db)
        .await
        .unwrap();
        role_permission::ActiveModel {
            role: Set("root".into()),
            permission: Set(perm::SYSTEM_ADMIN.into()),
        }
        .insert(&app.db)
        .await
        .unwrap();
        user_role::ActiveModel {
            user_id: Set(id),
            role: Set("root".into()),
        }
        .insert(&app.db)
        .await
        .unwrap();

        let res = app
            .delete_with_token(&routes::user_role(id, "admin"), &token)
            .await;

        assert_eq!(res.status, 204, "revoke failed: {}", res.text);
        assert_eq!(active_admin_ids(&app).await, vec![id]);
    }
}

mod revoke_role_permission {
    use super::*;

    /// Two admins, but both hold `system:admin` only through `admin`, so
    /// removing it from the role would de-privilege both at once.
    #[tokio::test]
    async fn system_admin_cannot_leave_its_only_granting_role() {
        let app = TestApp::spawn().await;
        let a_token = app.create_user_with_role("adm_a", PASSWORD, "admin").await;
        let b_token = app.create_user_with_role("adm_b", PASSWORD, "admin").await;
        let a = user_id(&app, &a_token).await;
        let b = user_id(&app, &b_token).await;

        let res = app
            .delete_with_token(
                &routes::role_permission("admin", perm::SYSTEM_ADMIN),
                &a_token,
            )
            .await;

        assert_conflict(&res);
        assert!(role_grants(&app, "admin", perm::SYSTEM_ADMIN).await);
        assert_eq!(active_admin_ids(&app).await, vec![a, b]);
    }

    #[tokio::test]
    async fn system_admin_is_revocable_while_another_role_grants_it() {
        let app = TestApp::spawn().await;
        let a_token = app.create_user_with_role("adm_a", PASSWORD, "admin").await;
        let ops_token = app
            .create_user_with_permissions("ops", PASSWORD, &[perm::SYSTEM_ADMIN])
            .await;
        let ops = user_id(&app, &ops_token).await;

        let res = app
            .delete_with_token(
                &routes::role_permission("admin", perm::SYSTEM_ADMIN),
                &a_token,
            )
            .await;
        assert_eq!(res.status, 204, "revoke failed: {}", res.text);
        assert_eq!(active_admin_ids(&app).await, vec![ops]);

        // `adm_a` keeps `role:manage` but no longer holds `system:admin`, so
        // the privilege ceiling refuses it before the guard is reached.
        let a_token = login(&app, "adm_a").await;
        let res = app
            .delete_with_token(
                &routes::role_permission("ops_role", perm::SYSTEM_ADMIN),
                &a_token,
            )
            .await;
        assert_eq!(
            res.status, 403,
            "expected the ceiling to refuse: {}",
            res.text
        );
        assert!(role_grants(&app, "ops_role", perm::SYSTEM_ADMIN).await);
        assert_eq!(active_admin_ids(&app).await, vec![ops]);
    }
}

mod delete_user {
    use super::*;

    /// Also pins that soft-deleted users no longer count, even though their
    /// `user_role` rows are kept.
    #[tokio::test]
    async fn last_active_admin_cannot_be_deleted_or_demoted() {
        let app = TestApp::spawn().await;
        let a_token = app.create_user_with_role("adm_a", PASSWORD, "admin").await;
        let b_token = app.create_user_with_role("adm_b", PASSWORD, "admin").await;
        let a = user_id(&app, &a_token).await;
        let b = user_id(&app, &b_token).await;

        let res = app.delete_with_token(&routes::user(b), &a_token).await;
        assert_eq!(res.status, 204, "delete failed: {}", res.text);
        assert!(user_has_role(&app, b, "admin").await);

        assert_conflict(&app.delete_with_token(&routes::user(a), &a_token).await);
        assert_conflict(
            &app.delete_with_token(&routes::user_role(a, "admin"), &a_token)
                .await,
        );
        assert_eq!(active_admin_ids(&app).await, vec![a]);
    }
}

/// With no `system:admin` holder at all, there is nothing to protect, and
/// the guard must not block ordinary management.
#[tokio::test]
async fn changes_that_remove_no_holder_are_allowed() {
    let app = TestApp::spawn().await;
    let manager_token = app
        .create_user_with_permissions(
            "manager",
            PASSWORD,
            &[
                perm::USER_MANAGE,
                perm::ROLE_MANAGE,
                perm::SUBMISSION_SUBMIT,
            ],
        )
        .await;
    let victim_token = app.create_authenticated_user("victim", PASSWORD).await;
    let victim = user_id(&app, &victim_token).await;
    assert!(active_admin_ids(&app).await.is_empty());

    let res = app
        .delete_with_token(&routes::user_role(victim, "contestant"), &manager_token)
        .await;
    assert_eq!(res.status, 204, "revoke role failed: {}", res.text);

    let res = app
        .delete_with_token(
            &routes::role_permission("contestant", perm::SUBMISSION_SUBMIT),
            &manager_token,
        )
        .await;
    assert_eq!(res.status, 204, "revoke permission failed: {}", res.text);

    let res = app
        .delete_with_token(&routes::user(victim), &manager_token)
        .await;
    assert_eq!(res.status, 204, "delete failed: {}", res.text);
}

mod concurrency {
    use super::*;

    /// A demotes B while B deletes A. Each write alone leaves one admin.
    #[tokio::test]
    async fn revocation_and_deletion_cannot_both_win() {
        let app = TestApp::spawn().await;
        let a_token = app.create_user_with_role("adm_a", PASSWORD, "admin").await;
        let b_token = app.create_user_with_role("adm_b", PASSWORD, "admin").await;
        let a = user_id(&app, &a_token).await;
        let b = user_id(&app, &b_token).await;
        let demote_b = routes::user_role(b, "admin");
        let delete_a = routes::user(a);

        let results = race_behind_admin_lock(
            &app,
            app.delete_with_token(&demote_b, &a_token),
            app.delete_with_token(&delete_a, &b_token),
        )
        .await;

        assert_one_won(&results);
        assert_eq!(active_admin_ids(&app).await.len(), 1);
    }

    /// A strips `system:admin` from B's only role while B demotes A.
    #[tokio::test]
    async fn permission_and_role_revocations_cannot_both_win() {
        let app = TestApp::spawn().await;
        let a_token = app.create_user_with_role("adm_a", PASSWORD, "admin").await;
        let b_token = app
            .create_user_with_permissions("ops", PASSWORD, &[perm::SYSTEM_ADMIN, perm::USER_MANAGE])
            .await;
        let a = user_id(&app, &a_token).await;
        let strip_ops = routes::role_permission("ops_role", perm::SYSTEM_ADMIN);
        let demote_a = routes::user_role(a, "admin");

        let results = race_behind_admin_lock(
            &app,
            app.delete_with_token(&strip_ops, &a_token),
            app.delete_with_token(&demote_a, &b_token),
        )
        .await;

        assert_one_won(&results);
        assert_eq!(active_admin_ids(&app).await.len(), 1);
    }
}
