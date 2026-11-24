use sea_orm_migration::prelude::*;

/// The locale whose host text a plugin provides as a language pack
/// (`entity::plugin::Model::host_locale`), and the rule that one enabled
/// plugin at a time provides a locale: a unique index over the enabled rows'
/// locales, so no write of any path can enable a second pack for one.
///
/// Forward-only like every migration here.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0016_plugin_host_locale"
    }
}

const UP: &str = r#"
ALTER TABLE public.plugin ADD COLUMN host_locale character varying;
CREATE UNIQUE INDEX plugin_one_enabled_per_host_locale
    ON public.plugin USING btree (host_locale) WHERE is_enabled;
"#;

const DOWN: &str = r#"
ALTER TABLE public.plugin DROP COLUMN host_locale;
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
