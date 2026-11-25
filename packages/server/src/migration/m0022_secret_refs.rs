use sea_orm_migration::prelude::*;

/// The refs of the secrets each stored config holds
/// (`entity::plugin_config::Model::secret_refs`): a read shows a set secret
/// as its ref, a write keeps the stored value only where it sends the ref
/// back, and a bundle import keeps a target's secret only where the target
/// holds the bundle's ref (`crate::services::plugin_config`).
///
/// Additive (release/docs/upgrade.md): a defaulted column servers of the
/// previous release never read.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0022_secret_refs"
    }
}

const UP: &str = r#"
ALTER TABLE public.plugin_config ADD COLUMN secret_refs jsonb NOT NULL DEFAULT '[]'::jsonb;
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
            .execute_unprepared("ALTER TABLE public.plugin_config DROP COLUMN secret_refs")
            .await?;
        Ok(())
    }
}
