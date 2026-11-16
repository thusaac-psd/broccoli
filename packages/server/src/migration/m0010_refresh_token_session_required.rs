use sea_orm_migration::prelude::*;

/// Every refresh token belongs to a sign-in session
/// (`entity::refresh_token::Model::session_id`). A row without one can never
/// be presented successfully, so it is deleted.
///
/// Rolling-safe: every replica of this baseline inserts refresh tokens with a
/// session.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0010_refresh_token_session_required"
    }
}

const UP: &str = r#"
DELETE FROM public.refresh_tokens WHERE session_id IS NULL;
ALTER TABLE public.refresh_tokens ALTER COLUMN session_id SET NOT NULL;
"#;

const DOWN: &str = r#"
ALTER TABLE public.refresh_tokens ALTER COLUMN session_id DROP NOT NULL;
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
