use serde_json::json;

use crate::common::{TestApp, routes};

mod response_binding {
    use super::*;
    use sea_orm::EntityTrait;
    use server::entity::idempotency_key;

    /// A response that carries a generated password is marked `no-store`: the
    /// idempotency cache records only that the request completed, and a replay
    /// answers 409 instead of re-sending the secret or creating the account twice.
    #[tokio::test]
    async fn created_user_password_is_not_persisted_by_idempotency_cache() {
        let app = TestApp::spawn().await;
        let admin = app
            .create_user_with_role("idem_admin", "password123", "admin")
            .await;
        let send = || {
            app.client
                .post(format!("http://{}{}", app.addr, routes::USERS))
                .header("Authorization", format!("Bearer {admin}"))
                .header("Idempotency-Key", "create-user-1")
                .json(&json!({"username": "idem_newbie"}))
                .send()
        };
        let res = send().await.expect("send");
        assert_eq!(res.status().as_u16(), 201);
        assert_eq!(
            res.headers()
                .get("cache-control")
                .and_then(|v| v.to_str().ok()),
            Some("no-store")
        );
        let body: serde_json::Value = res.json().await.expect("json");
        let password = body["password"].as_str().expect("generated password");

        let cached: Vec<String> = idempotency_key::Entity::find()
            .all(&app.db)
            .await
            .expect("db")
            .into_iter()
            .filter_map(|k| k.response_body)
            .collect();
        assert!(
            cached.iter().all(|b| !b.contains(password)),
            "plaintext password found in idempotency_key.response_body"
        );

        let replay = send().await.expect("send");
        assert_eq!(replay.status().as_u16(), 409);
        let replay: serde_json::Value = replay.json().await.expect("json");
        assert_eq!(replay["code"], "CONFLICT");
        assert!(replay.get("password").is_none());
    }
}
