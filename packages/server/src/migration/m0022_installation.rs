use sea_orm_migration::prelude::*;

/// The installation's id (`entity::installation`): one row, generated once
/// when the database is created or first migrated to this build, so a
/// bundle can tell the database that exported it from any other
/// (`crate::bundle`).
///
/// Additive (release/docs/upgrade.md): a new table servers of the previous
/// release never read.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0022_installation"
    }
}

const UP: &str = r#"
CREATE TABLE public.installation (
    id smallint NOT NULL,
    installation_id text NOT NULL,
    CONSTRAINT installation_pkey PRIMARY KEY (id)
);
INSERT INTO public.installation (id, installation_id) VALUES (1, gen_random_uuid()::text);
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP TABLE public.installation")
            .await?;
        Ok(())
    }
}
