use sea_orm::DatabaseConnection;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ADD_COLUMN: &str =
    r#"ALTER TABLE IF EXISTS "user" ADD COLUMN IF NOT EXISTS display_name TEXT"#;

/// Stage the nullable column before entity sync, which otherwise tries to add
/// a NOT NULL column to populated tables. The migration below backfills it.
pub async fn prepare_column(db: &DatabaseConnection) -> Result<(), DbErr> {
    let manager = SchemaManager::new(db);
    if manager.has_table("user").await? && !manager.has_column("user", "display_name").await? {
        db.execute_unprepared(ADD_COLUMN).await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(ADD_COLUMN).await?;
        db.execute_unprepared(
            r#"
            UPDATE "user" SET display_name = username
                WHERE display_name IS NULL OR btrim(display_name) = '';
            ALTER TABLE "user" ALTER COLUMN display_name SET NOT NULL;
            ALTER TABLE "user" DROP CONSTRAINT IF EXISTS user_display_name_not_blank;
            ALTER TABLE "user" ADD CONSTRAINT user_display_name_not_blank
                CHECK (btrim(display_name) <> '');
            "#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"ALTER TABLE "user" DROP COLUMN IF EXISTS display_name"#)
            .await?;
        Ok(())
    }
}
