use sea_orm_migration::prelude::*;

/// A stored password is a hash or `NULL` (no password), never empty: an empty
/// value is neither, and no password verifies against it. Any empty value
/// left becomes `NULL` first.
///
/// Rolling-safe: no build since `m0007_user_password_optional` writes an
/// empty password; an account without one is stored as `NULL`.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0008_user_password_not_blank"
    }
}

const UP: &str = r#"
UPDATE public."user" SET password = NULL WHERE password = '';
ALTER TABLE public."user" ADD CONSTRAINT user_password_not_blank
    CHECK (password IS NULL OR password <> '');
"#;

const DOWN: &str = r#"
ALTER TABLE public."user" DROP CONSTRAINT user_password_not_blank;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
