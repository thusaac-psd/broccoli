use sea_orm_migration::prelude::*;

/// What each replica has loaded, of every kind of key plugins provide
/// (`entity::plugin_replica::Model::provides`, a `providers::Loaded`):
/// contest types, problem types and checker formats, each with the plugins
/// that registered it there, which the claim reads to take only work this
/// replica runs on each key's fleet-wide provider. It takes the place of the
/// contest types alone. Every live replica rewrites its row on its next
/// report, so the rows are reset.
///
/// Not additive (release/docs/upgrade.md): it renames a column servers of the
/// previous release write, so they stop before it runs.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0019_replica_provides"
    }
}

const UP: &str = r#"
ALTER TABLE public.plugin_replica RENAME COLUMN contest_types TO provides;
UPDATE public.plugin_replica SET provides = NULL;
"#;

const DOWN: &str = r#"
ALTER TABLE public.plugin_replica RENAME COLUMN provides TO contest_types;
UPDATE public.plugin_replica SET contest_types = NULL;
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
