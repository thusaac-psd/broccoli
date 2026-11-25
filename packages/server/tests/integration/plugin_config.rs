use crate::common::{SpawnOptions, TestApp, routes};
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

/// A config whose namespace this server knows no schema for (its plugin's
/// upload removed, or the namespace renamed) is one no one here can say the
/// secret fields of: every read withholds it whole.
mod unknown_schema {
    use super::*;
    use sea_orm::ConnectionTrait;

    #[tokio::test]
    async fn a_config_whose_schema_is_unknown_is_never_read_back() {
        let app = TestApp::spawn_with_config_schemas().await;
        let admin = app
            .create_user_with_role("unknown_schema_admin", "password123", "admin")
            .await;
        let problem_id = app.create_problem(&admin, "P").await;
        let res = app
            .put_with_token(
                &routes::problem_config_ns(problem_id, "test-plugin", "checker"),
                &json!({ "config": { "k": "kept-value" } }),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        app.db
            .execute_unprepared(&format!(
                "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
                 VALUES ('problem', '{problem_id}', 'test-plugin:renamed', \
                 '{{\"token\": \"unknown-schema-secret\"}}', 0, now()), \
                 ('problem', '{problem_id}', 'gone-plugin:task', \
                 '{{\"token\": \"gone-plugin-secret\"}}', 0, now())"
            ))
            .await
            .unwrap();

        let res = app
            .get_with_token(&routes::problem_config(problem_id), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert!(!res.text.contains("unknown-schema-secret"), "{}", res.text);
        assert!(!res.text.contains("gone-plugin-secret"), "{}", res.text);
        let entry = |plugin: &str, namespace: &str| {
            res.body
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["plugin_id"] == plugin && e["namespace"] == namespace)
                .unwrap_or_else(|| panic!("no {plugin}/{namespace} in {}", res.text))
                .clone()
        };
        let renamed = entry("test-plugin", "renamed");
        assert_eq!(renamed["config"], json!(null), "{renamed}");
        assert_eq!(renamed["config_withheld"], json!(true), "{renamed}");
        let known = entry("test-plugin", "checker");
        assert_eq!(known["config_withheld"], json!(false), "{known}");
        assert!(
            res.text.contains("kept-value"),
            "a known schema's config is shown"
        );
    }
}

/// A read shows a set secret as its ref, `{"$secret": "<ref>"}`, and a
/// write keeps the stored secret only where it sends that ref back: any
/// other write that sends no secret where one is stored never read it, and
/// is refused. A string sets the secret, and `{"$clear": true}` clears it.
mod secret_ref {
    use super::*;

    async fn save(
        app: &TestApp,
        admin: &str,
        path: &str,
        config: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        let res = app
            .put_with_token(path, &json!({ "config": config }), admin)
            .await;
        (res.status, res.body)
    }

    fn refused(body: &serde_json::Value) -> (String, String) {
        let field = &body["details"]["fields"][0];
        (
            field["path"].as_str().unwrap_or_default().to_string(),
            field["kind"].as_str().unwrap_or_default().to_string(),
        )
    }

    #[tokio::test]
    async fn only_the_ref_a_read_showed_keeps_a_secret() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("secret_ref_admin", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let path = routes::problem_config_ns(problem_id, "server-plugin", "bundled");

        let (status, saved) = save(&app, &admin, &path, json!({ "token": "first" })).await;
        assert_eq!(status, 200, "{saved}");
        let first = saved["config"]["token"].clone();
        assert!(first["$secret"].is_string(), "{saved}");
        let read = app.get_with_token(&path, &admin).await;
        assert_eq!(read.body["config"]["token"], first, "{}", read.text);

        let (status, kept) = save(&app, &admin, &path, json!({ "token": first })).await;
        assert_eq!(status, 200, "{kept}");
        assert_eq!(kept["config"]["token"], first, "the echoed ref keeps it");

        for (what, config) in [
            ("null", json!({ "token": null })),
            ("left out", json!({ "from": "a file without the secret" })),
            ("a forged ref", json!({ "token": { "$secret": "forged" } })),
        ] {
            let (status, body) = save(&app, &admin, &path, config).await;
            assert_eq!(status, 400, "{what}: {body}");
            assert_eq!(
                refused(&body),
                ("token".to_string(), "secret_not_stored".to_string()),
                "{what}: {body}"
            );
        }

        let (status, rotated) = save(&app, &admin, &path, json!({ "token": "second" })).await;
        assert_eq!(status, 200, "{rotated}");
        assert_ne!(rotated["config"]["token"], first, "a new secret, a new ref");
        let (status, body) = save(&app, &admin, &path, json!({ "token": first })).await;
        assert_eq!(
            status, 400,
            "the ref of a secret set again since keeps nothing: {body}"
        );

        let (status, cleared) =
            save(&app, &admin, &path, json!({ "token": { "$clear": true } })).await;
        assert_eq!(status, 200, "{cleared}");
        assert_eq!(cleared["config"], json!({}), "{cleared}");
        let (status, unset) = save(&app, &admin, &path, json!({ "token": null })).await;
        assert_eq!(
            status, 200,
            "with nothing stored, no secret is no secret: {unset}"
        );
    }

    /// The print station case: an API client that deletes a station and adds
    /// one of the same name gets no old token back, whatever it sends.
    #[tokio::test]
    async fn an_item_deleted_and_added_again_by_name_keeps_no_secret() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("secret_ref_readd", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let path = routes::problem_config_ns(problem_id, "server-plugin", "keyed");
        let (status, saved) = save(
            &app,
            &admin,
            &path,
            json!({ "stations": [{ "name": "desk", "token": "old" }] }),
        )
        .await;
        assert_eq!(status, 200, "{saved}");
        let read = saved["config"]["stations"][0]["token"].clone();

        for (what, desk) in [
            ("null", json!({ "name": "desk", "token": null })),
            ("left out", json!({ "name": "desk" })),
        ] {
            let (status, body) = save(&app, &admin, &path, json!({ "stations": [desk] })).await;
            assert_eq!(status, 400, "{what}: {body}");
            assert_eq!(
                refused(&body),
                (
                    "stations[0].token".to_string(),
                    "secret_required".to_string()
                ),
                "{what}: {body}"
            );
        }
        let (status, body) = save(
            &app,
            &admin,
            &path,
            json!({ "stations": [{ "name": "renamed", "token": read }] }),
        )
        .await;
        assert_eq!(status, 400, "a renamed station keeps nothing: {body}");
        assert_eq!(refused(&body).1, "secret_not_stored", "{body}");

        let (status, kept) = save(
            &app,
            &admin,
            &path,
            json!({ "stations": [{ "name": "desk", "token": read }] }),
        )
        .await;
        assert_eq!(status, 200, "{kept}");
        assert_eq!(kept["config"]["stations"][0]["token"], read);
    }
}

/// A secret its item requires is never cleared: the marker is refused with
/// its own error, so the form can say to replace it instead.
mod required_secret {
    use super::*;

    #[tokio::test]
    async fn clearing_a_required_secret_is_refused_with_its_own_error() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("required_secret_admin", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let path = routes::problem_config_ns(problem_id, "server-plugin", "keyed");
        let res = app
            .put_with_token(
                &path,
                &json!({ "config": { "stations": [{ "name": "desk", "token": "t" }] } }),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);

        let res = app
            .put_with_token(
                &path,
                &json!({ "config": { "stations": [
                    { "name": "desk", "token": { "$clear": true } },
                ] } }),
                &admin,
            )
            .await;
        assert_eq!(res.status, 400, "{}", res.text);
        let field = &res.body["details"]["fields"][0];
        assert_eq!(field["path"], "stations[0].token", "{}", res.text);
        assert_eq!(field["kind"], "secret_required", "{}", res.text);

        let res = app.get_with_token(&path, &admin).await;
        assert!(
            res.body["config"]["stations"][0]["token"]["$secret"].is_string(),
            "a refused clear keeps the secret: {}",
            res.text
        );
    }
}

/// A save reads the stored secrets it keeps under the row's lock, so a save
/// that keeps a secret while another rotates it waits for the rotation and
/// then keeps nothing by the old ref, never reverting the rotation.
mod concurrent_secret {
    use super::*;
    use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

    #[tokio::test(flavor = "multi_thread")]
    async fn a_save_that_keeps_a_secret_waits_for_a_rotation_in_flight() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("concurrent_secret_admin", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let path = routes::problem_config_ns(problem_id, "server-plugin", "bundled");
        let res = app
            .put_with_token(&path, &json!({ "config": { "token": "old" } }), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        let old_ref = res.body["config"]["token"].clone();

        // The test's own connections, beside the server's pool.
        let own = sea_orm::Database::connect(app.database.url())
            .await
            .unwrap();
        // A rotation in flight: the row is locked and holds the new token.
        let rotation = own.begin().await.unwrap();
        rotation
            .execute_unprepared(&format!(
                "UPDATE plugin_config SET config = '{{\"token\": \"rotated\"}}', \
                 secret_refs = '[{{\"identity\": [{{\"field\": \"token\"}}], \"ref\": \"new\"}}]' \
                 WHERE scope = 'problem' AND ref_id = '{problem_id}' \
                 AND namespace = 'server-plugin:bundled'"
            ))
            .await
            .unwrap();

        let keep = {
            let (url, admin, client) = (app.url(&path), admin.clone(), app.client.clone());
            tokio::spawn(async move {
                client
                    .put(url)
                    .bearer_auth(admin)
                    .json(&json!({ "config": { "token": old_ref, "from": "kept" } }))
                    .send()
                    .await
                    .unwrap()
                    .status()
                    .as_u16()
            })
        };
        // The save waits on the rotation's row lock.
        loop {
            let waiting = own
                .query_one_raw(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT count(*) AS n FROM pg_stat_activity \
                     WHERE wait_event_type = 'Lock' AND datname = current_database()",
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "n")
                .unwrap();
            if waiting > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        rotation.commit().await.unwrap();
        assert_eq!(keep.await.unwrap(), 400, "the old ref keeps nothing");

        let stored = own
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT config->>'token' AS v FROM plugin_config \
                     WHERE scope = 'problem' AND ref_id = '{problem_id}' \
                     AND namespace = 'server-plugin:bundled'"
                ),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<String>("", "v")
            .unwrap();
        assert_eq!(stored, "rotated", "the rotated token stays");
    }
}

/// Every config write and delete takes the rows it locks in one order: the
/// problems its triggers count on, by id, before any config row. Each test
/// holds one row from its own connection, lets the server's write wait on
/// it, and reads which rows the waiting write already holds.
mod lock_order {
    use super::*;
    use sea_orm::{
        ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
        TransactionTrait,
    };

    async fn own(app: &TestApp) -> DatabaseConnection {
        sea_orm::Database::connect(app.database.url())
            .await
            .unwrap()
    }

    /// Holds `problem_id`'s row until the returned transaction ends.
    async fn hold_problem(db: &DatabaseConnection, problem_id: i32) -> DatabaseTransaction {
        let txn = db.begin().await.unwrap();
        txn.execute_unprepared(&format!(
            "SELECT id FROM problem WHERE id = {problem_id} FOR UPDATE"
        ))
        .await
        .unwrap();
        txn
    }

    /// Returns once some statement of the database waits on a lock.
    async fn until_waiting(db: &DatabaseConnection) {
        loop {
            let waiting = db
                .query_one_raw(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT count(*) AS n FROM pg_stat_activity \
                     WHERE wait_event_type = 'Lock' AND datname = current_database()",
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get::<i64>("", "n")
                .unwrap();
            if waiting > 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    /// Whether another transaction holds a row `select` reads.
    async fn held(db: &DatabaseConnection, select: &str) -> bool {
        let txn = db.begin().await.unwrap();
        let result = txn
            .execute_unprepared(&format!("{select} FOR UPDATE NOWAIT"))
            .await;
        txn.rollback().await.unwrap();
        match result {
            Ok(_) => false,
            Err(error) => {
                assert!(
                    error.to_string().contains("could not obtain lock"),
                    "{error}"
                );
                true
            }
        }
    }

    fn problem_row(problem_id: i32) -> String {
        format!("SELECT id FROM problem WHERE id = {problem_id}")
    }

    fn config_row(scope: &str, ref_id: i32) -> String {
        format!(
            "SELECT 1 FROM plugin_config WHERE scope = '{scope}' AND ref_id = '{ref_id}' \
             AND namespace = 'server-plugin:judged'"
        )
    }

    /// A contest of problems `low` < `high`, each stored so that reading the
    /// tables in the order they lie meets `high` first.
    async fn contest_of_two(app: &TestApp, admin: &str) -> (i32, i32, i32) {
        let low = app.insert_public_problem("Low").await;
        let high = app.insert_public_problem("High").await;
        let contest_id = app.create_contest(admin, "Locks", true, true).await;
        let db = own(app).await;
        db.execute_unprepared(&format!(
            "INSERT INTO contest_problem (contest_id, problem_id, label, position) \
             VALUES ({contest_id}, {high}, 'A', 0), ({contest_id}, {low}, 'B', 1); \
             UPDATE problem SET title = 'Low' WHERE id = {low}"
        ))
        .await
        .unwrap();
        (contest_id, low, high)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_waiting_for_its_problem_holds_no_config_row_a_save_waits_on() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("lock_order_delete", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let path = routes::problem_config_ns(problem_id, "server-plugin", "judged");
        let res = app
            .put_with_token(&path, &json!({ "config": { "level": 1 } }), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);

        let db = own(&app).await;
        // A save in flight holds the problem.
        let save = hold_problem(&db, problem_id).await;
        let delete = {
            let (url, admin, client) = (app.url(&path), admin.clone(), app.client.clone());
            tokio::spawn(async move {
                let res = client.delete(url).bearer_auth(admin).send().await.unwrap();
                res.status().as_u16()
            })
        };
        until_waiting(&db).await;
        assert!(
            !held(&db, &config_row("problem", problem_id)).await,
            "the delete holds the config row while it waits for the problem"
        );
        save.commit().await.unwrap();
        assert_eq!(delete.await.unwrap(), 204);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_contest_config_write_locks_its_problems_by_id_before_its_row() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("lock_order_contest", "password123", "admin")
            .await;
        let (contest_id, low, high) = contest_of_two(&app, &admin).await;
        let path = routes::contest_config_ns(contest_id, "server-plugin", "judged");
        let res = app
            .put_with_token(&path, &json!({ "config": { "level": 1 } }), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);

        let db = own(&app).await;
        // An overlapping write holds the contest's last problem.
        let other = hold_problem(&db, high).await;
        let write = {
            let (url, admin, client) = (app.url(&path), admin.clone(), app.client.clone());
            tokio::spawn(async move {
                let res = client
                    .put(url)
                    .bearer_auth(admin)
                    .json(&json!({ "config": { "level": 2 } }))
                    .send()
                    .await
                    .unwrap();
                res.status().as_u16()
            })
        };
        until_waiting(&db).await;
        assert!(
            held(&db, &problem_row(low)).await,
            "the write takes the lower problem first"
        );
        assert!(
            !held(&db, &config_row("contest", contest_id)).await,
            "the write holds the config row while it waits for a problem"
        );
        other.commit().await.unwrap();
        assert_eq!(write.await.unwrap(), 200);
    }

    /// The counting triggers lock the problems they count on by id, whatever
    /// statement fires them.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_inputs_triggers_lock_a_contests_problems_by_id() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("lock_order_trigger", "password123", "admin")
            .await;
        let (contest_id, low, high) = contest_of_two(&app, &admin).await;
        let res = app
            .put_with_token(
                &routes::contest_config_ns(contest_id, "server-plugin", "judged"),
                &json!({ "config": { "level": 1 } }),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);

        let db = own(&app).await;
        for (what, statement) in [
            (
                "a contest config change",
                format!(
                    "UPDATE plugin_config SET config = '{{\"level\": 3}}' \
                     WHERE scope = 'contest' AND ref_id = '{contest_id}' \
                     AND namespace = 'server-plugin:judged'"
                ),
            ),
            (
                "a contest type change",
                format!(
                    "UPDATE contest SET contest_type = contest_type || '-next' \
                     WHERE id = {contest_id}"
                ),
            ),
        ] {
            let other = hold_problem(&db, high).await;
            let fired = {
                let db = own(&app).await;
                tokio::spawn(async move {
                    let txn = db.begin().await.unwrap();
                    txn.execute_unprepared(&statement).await.unwrap();
                    txn.rollback().await.unwrap();
                })
            };
            until_waiting(&db).await;
            assert!(
                held(&db, &problem_row(low)).await,
                "{what} takes the lower problem first"
            );
            other.commit().await.unwrap();
            fired.await.unwrap();
        }
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

/// A plugin's own writes (`config_set`) follow the rules every config write
/// does: its namespace's validate function checks them before they are
/// stored, and the problem's `config_issues` flag is rechecked after.
mod plugin_writes {
    use super::*;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    async fn stored(app: &TestApp, problem_id: i32) -> Option<serde_json::Value> {
        app.db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT config FROM plugin_config WHERE scope = 'problem' \
                     AND ref_id = '{problem_id}' AND namespace = 'server-plugin:checked'"
                ),
            ))
            .await
            .unwrap()
            .map(|row| row.try_get("", "config").unwrap())
    }

    async fn flag(app: &TestApp, problem_id: i32) -> bool {
        app.db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT config_issues FROM problem WHERE id = {problem_id}"),
            ))
            .await
            .unwrap()
            .expect("the problem")
            .try_get("", "config_issues")
            .unwrap()
    }

    #[tokio::test]
    async fn a_plugin_write_its_validate_function_refuses_is_not_stored() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("plugin_write_admin", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let path = routes::plugin_proxy("server-plugin", &format!("config/problems/{problem_id}"));

        let res = app
            .put_with_token(&path, &json!({ "refuse": true }), &admin)
            .await;
        assert_eq!(
            res.status, 500,
            "a refused write fails the call: {}",
            res.text
        );
        assert_eq!(stored(&app, problem_id).await, None);

        let res = app
            .put_with_token(&path, &json!({ "refuse": false }), &admin)
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(
            stored(&app, problem_id).await,
            Some(json!({ "refuse": false }))
        );
    }

    #[tokio::test]
    async fn a_plugin_write_rechecks_the_problems_config_issues() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("plugin_write_flag_admin", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        app.db
            .execute_unprepared(&format!(
                "UPDATE problem SET config_issues = true WHERE id = {problem_id}"
            ))
            .await
            .unwrap();

        let res = app
            .put_with_token(
                &routes::plugin_proxy("server-plugin", &format!("config/problems/{problem_id}")),
                &json!({ "refuse": false }),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert!(
            !flag(&app, problem_id).await,
            "no stored config has issues, so the stale flag clears"
        );
    }
}

/// Only a replica that loads a plugin can run its checks. One where a plugin
/// enabled fleet-wide is not loaded cannot say what the plugin would find,
/// and unknown is not clean: it leaves the problem flagged, marked for a
/// replica that loads the plugin, and answers the issues route with a retry.
mod unknown_checks {
    use super::*;
    use sea_orm::{ConnectionTrait, DbBackend, Statement};

    async fn flags(app: &TestApp, problem_id: i32) -> String {
        app.db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT config_issues::text || '/' || coalesce(config_recheck::text, 'none') \
                     AS v FROM problem WHERE id = {problem_id}"
                ),
            ))
            .await
            .unwrap()
            .expect("the problem")
            .try_get("", "v")
            .unwrap()
    }

    /// A problem whose `server-plugin` config the plugin refuses, flagged by
    /// the replica that checked it.
    async fn problem_with_a_refused_config(app: &TestApp) -> i32 {
        let problem_id = app.insert_public_problem("P").await;
        app.db
            .execute_unprepared(&format!(
                "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
                 VALUES ('problem', '{problem_id}', 'server-plugin:checked', \
                 '{{\"refuse\": true}}', 0, now()); \
                 UPDATE problem SET config_issues = true WHERE id = {problem_id}"
            ))
            .await
            .unwrap();
        problem_id
    }

    /// Only a target's providers decide its checks: a plugin that also
    /// records providing the problem's types, but is not their fleet-wide
    /// provider, is never run on them, so its being loaded nowhere leaves
    /// nothing undecided.
    #[tokio::test]
    async fn a_plugin_that_is_not_the_types_provider_leaves_nothing_undecided() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("second_governor_admin", "password123", "admin")
            .await;
        let problem_id = app.insert_public_problem("P").await;
        let row = app
            .db
            .query_one_raw(Statement::from_string(
                DbBackend::Postgres,
                format!(
                    "SELECT problem_type, default_contest_type FROM problem WHERE id = {problem_id}"
                ),
            ))
            .await
            .unwrap()
            .expect("the problem");
        let problem_type: String = row.try_get("", "problem_type").unwrap();
        let contest_type: String = row.try_get("", "default_contest_type").unwrap();
        // A plugin whose id sorts after every shipped one records providing
        // both types; it has no files, so it is loaded on no replica.
        app.db
            .execute_unprepared(&format!(
                "INSERT INTO plugin (id, is_enabled, generation, updated_at, governs) VALUES \
                 ('zzz-governor', TRUE, 1, NOW(), '{{\"visibility\": false, \
                   \"contest_types\": [\"{contest_type}\"], \"problem_types\": [\"{problem_type}\"], \
                   \"checker_formats\": [], \"languages\": [], \"unchecked_problem_types\": [], \
                   \"hooks\": [], \"judging_inputs\": [], \"checker_languages\": []}}')"
            ))
            .await
            .unwrap();

        // A test-data change rechecks the problem on this replica.
        app.create_test_case(problem_id, &admin).await;
        assert_eq!(
            flags(&app, problem_id).await,
            "false/none",
            "a plugin that does not provide the problem's types decides nothing of it"
        );
    }

    #[tokio::test]
    async fn a_replica_without_the_plugin_leaves_the_problem_flagged_and_marked() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("unknown_check_admin", "password123", "admin")
            .await;
        let problem_id = problem_with_a_refused_config(&app).await;
        app.state
            .plugins
            .unload_plugin("server-plugin")
            .expect("unload here only");

        // A test-data change rechecks the problem on this replica.
        app.create_test_case(problem_id, &admin).await;
        assert_eq!(
            flags(&app, problem_id).await,
            r#"true/["server-plugin"]"#,
            "the flag stays and the mark names the plugin a replica must load"
        );
        let res = app
            .get_with_token(
                &format!("/api/v1/problems/{problem_id}/config/issues"),
                &admin,
            )
            .await;
        assert_eq!(res.status, 503, "{}", res.text);
        assert_eq!(res.body["code"], "PLUGIN_UNAVAILABLE", "{}", res.text);

        server::services::recheck_marked(&app.state).await;
        assert_eq!(
            flags(&app, problem_id).await,
            r#"true/["server-plugin"]"#,
            "a replica that cannot decide leaves the mark alone"
        );

        app.state
            .plugins
            .load_plugin("server-plugin")
            .expect("load it again");
        server::services::recheck_marked(&app.state).await;
        assert_eq!(
            flags(&app, problem_id).await,
            "true/none",
            "a replica that loads the plugin decides: the config is refused"
        );
    }

    #[tokio::test]
    async fn replicas_that_each_lack_a_plugin_the_other_loads_leave_the_mark_for_one_that_loads_both()
     {
        let a = TestApp::spawn_with_plugins().await;
        let admin = a
            .create_user_with_role("split_fleet_admin", "password123", "admin")
            .await;
        let problem_id = problem_with_a_refused_config(&a).await;
        a.db.execute_unprepared(&format!(
            "INSERT INTO plugin_config (scope, ref_id, namespace, config, position, updated_at) \
             VALUES ('problem', '{problem_id}', 'web-plugin:settings', '{{}}', 0, now()); \
             UPDATE plugin SET is_enabled = true WHERE id IN ('server-plugin', 'web-plugin')"
        ))
        .await
        .unwrap();
        let b = a.spawn_replica(SpawnOptions::default()).await;
        a.state
            .plugins
            .unload_plugin("web-plugin")
            .expect("unload on a only");
        b.state
            .plugins
            .unload_plugin("server-plugin")
            .expect("unload on b only");

        a.create_test_case(problem_id, &admin).await;
        assert_eq!(flags(&a, problem_id).await, r#"true/["web-plugin"]"#);

        // b loads the plugin the mark names but not the one a decided, so it
        // cannot decide either: the mark names both.
        server::services::recheck_marked(&b.state).await;
        let both = r#"true/["server-plugin", "web-plugin"]"#;
        assert_eq!(flags(&a, problem_id).await, both);
        server::services::recheck_marked(&a.state).await;
        server::services::recheck_marked(&b.state).await;
        assert_eq!(
            flags(&a, problem_id).await,
            both,
            "neither replica can decide, so neither takes the mark"
        );

        a.state
            .plugins
            .load_plugin("web-plugin")
            .expect("a loads both");
        server::services::recheck_marked(&a.state).await;
        assert_eq!(
            flags(&a, problem_id).await,
            "true/none",
            "a replica that loads both decides: the config is refused"
        );
    }

    #[tokio::test]
    async fn a_plugin_disabled_fleet_wide_is_not_checked_anywhere() {
        let app = TestApp::spawn_with_plugins().await;
        let admin = app
            .create_user_with_role("disabled_check_admin", "password123", "admin")
            .await;
        let problem_id = problem_with_a_refused_config(&app).await;
        let res = app
            .post_with_token(
                "/api/v1/admin/plugins/server-plugin/disable",
                &json!({}),
                &admin,
            )
            .await;
        assert_eq!(res.status, 200, "{}", res.text);
        assert_eq!(flags(&app, problem_id).await, "false/none");
    }
}
