use sea_orm_migration::prelude::*;

/// An account may have no password (`entity::user::Model::password` is
/// `None`): one a bundle brought without its secrets. It cannot sign in until
/// an admin sets one. A blank hash, which no password verifies against,
/// becomes `NULL`.
///
/// Rolling-safe for every account with a password. An older replica cannot
/// load an account without one, which it could not sign in either.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0007_user_password_optional"
    }
}

const UP: &str = r#"
ALTER TABLE public."user" ALTER COLUMN password DROP NOT NULL;
UPDATE public."user" SET password = NULL WHERE password = '';
"#;

const DOWN: &str = r#"
UPDATE public."user" SET password = '' WHERE password IS NULL;
ALTER TABLE public."user" ALTER COLUMN password SET NOT NULL;
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
