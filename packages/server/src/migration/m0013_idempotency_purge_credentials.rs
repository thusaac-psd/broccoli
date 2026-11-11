use sea_orm_migration::prelude::*;

/// Before `NoStore` existed the idempotency cache persisted the full body of
/// credential-bearing responses (generated plaintext passwords from user and
/// participant creation). Strip those bodies from rows already stored, keeping
/// the rows as `completed_not_stored` so an in-window retry is still answered
/// with 409 instead of being executed a second time.
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                UPDATE idempotency_key
                   SET status = 'completed_not_stored', response_body = NULL
                 WHERE response_body IS NOT NULL
                   AND response_body ~ '"(password|access_token|refresh_token|token)"\s*:'
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // Purged secrets cannot (and must not) be restored.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait};
    use testcontainers::ImageExt;
    use testcontainers::runners::AsyncRunner;
    use testcontainers_modules::postgres::Postgres;

    use crate::entity::idempotency_key;

    async fn seed(db: &DatabaseConnection, key: &str, body: &str) {
        db.execute_unprepared(&format!(
            "INSERT INTO idempotency_key (key, user_id, request_path, request_method, status, \
             response_status, response_body, created_at) \
             VALUES ('{key}', 1, '/v1/users', 'POST', 'completed', 201, '{body}', NOW())"
        ))
        .await
        .expect("seed row");
    }

    #[tokio::test]
    async fn strips_credential_bodies_and_keeps_the_rest() {
        let container = Postgres::default()
            .with_tag("17-alpine")
            .start()
            .await
            .expect("start postgres container");
        let port = container.get_host_port_ipv4(5432).await.expect("port");
        let url = format!("postgres://postgres:postgres@127.0.0.1:{port}/postgres");
        let db = crate::database::init_db(&url).await.expect("init schema");

        seed(&db, "with-password", r#"{"id":1,"password":"hunter2"}"#).await;
        seed(&db, "with-token", r#"{"access_token": "abc"}"#).await;
        seed(&db, "plain", r#"{"id":2,"title":"password reset guide"}"#).await;

        Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect("run migration");

        for key in ["with-password", "with-token"] {
            let row = idempotency_key::Entity::find_by_id((key.to_string(), 1))
                .one(&db)
                .await
                .expect("db")
                .expect("row kept so a retry is still refused");
            assert_eq!(row.status, "completed_not_stored", "{key}");
            assert_eq!(row.response_body, None, "{key}");
        }
        let plain = idempotency_key::Entity::find_by_id(("plain".to_string(), 1))
            .one(&db)
            .await
            .expect("db")
            .expect("row");
        assert_eq!(plain.status, "completed");
        assert!(plain.response_body.is_some());
    }
}
