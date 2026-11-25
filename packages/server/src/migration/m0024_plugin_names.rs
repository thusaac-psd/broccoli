use sea_orm_migration::prelude::*;

/// Each plugin's display name per locale (`entity::plugin::Model::names`),
/// as its own pack gives it (`plugin.<id>.name`), recorded with its governs
/// by the replica that publishes it, so a replica without the plugin's files,
/// or a fleet where no replica loads it, still names it.
///
/// Additive (release/docs/upgrade.md): a nullable column servers of the
/// previous release keep working against.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0024_plugin_names"
    }
}

const UP: &str = r#"
ALTER TABLE public.plugin ADD COLUMN names jsonb;
"#;

const DOWN: &str = r#"
ALTER TABLE public.plugin DROP COLUMN names;
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
