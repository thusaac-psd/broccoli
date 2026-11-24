use sea_orm_migration::prelude::*;

/// Whether a problem waits for a config issues recheck
/// (`entity::problem::Model::config_recheck`): a bundle import marks the
/// problems it restores, and every running server rechecks the marked ones.
///
/// Plugins read the problem table, so the plugin group role (named in
/// `plugin_group_role`, see m0001) may read the new column as it reads every
/// other column that is not content.
///
/// Additive (release/docs/upgrade.md): servers of the previous release keep
/// working against it, so a release that adds only migrations like it rolls
/// server by server.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0015_config_recheck"
    }
}

const UP: &str = r#"
ALTER TABLE public.problem ADD COLUMN config_recheck boolean NOT NULL DEFAULT false;
CREATE INDEX "idx-problem-config_recheck"
    ON public.problem USING btree (id) WHERE config_recheck;
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (config_recheck) ON public.problem TO %I', plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
END $$;
"#;

const DOWN: &str = r#"
ALTER TABLE public.problem DROP COLUMN config_recheck;
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
