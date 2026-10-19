use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        // Keep the recorded identity for databases started on the notification
        // branch before its file was renumbered during the master rebase.
        "m0011_plugin_notifications"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
            CREATE TABLE plugin_notification (
                id BIGSERIAL PRIMARY KEY,
                plugin_id TEXT NOT NULL,
                contest_id INTEGER NOT NULL REFERENCES contest(id) ON DELETE CASCADE,
                user_id INTEGER NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
                event_key TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload JSONB NOT NULL,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                UNIQUE (plugin_id, contest_id, user_id, event_key)
            );
            CREATE INDEX plugin_notification_replay
                ON plugin_notification (contest_id, user_id, plugin_id, id);
        "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE plugin_notification")
            .await?;
        Ok(())
    }
}
