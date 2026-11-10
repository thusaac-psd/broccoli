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
