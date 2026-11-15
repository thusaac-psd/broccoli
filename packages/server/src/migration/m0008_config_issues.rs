use sea_orm_migration::prelude::*;

/// Whether a problem's stored configs have issues
/// (`entity::problem::Model::config_issues`), and whether a judgement was
/// claimed while they did (`entity::submission_judgement::Model::config_issues`).
///
/// Plugins read both tables, so the plugin group role (named in
/// `plugin_group_role`, see m0001) may read the new columns as it reads
/// every other column that is not content.
///
/// Rolling-safe: defaulted columns older replicas never read or write.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0008_config_issues"
    }
}

const UP: &str = r#"
ALTER TABLE public.problem ADD COLUMN config_issues boolean NOT NULL DEFAULT false;
ALTER TABLE public.submission_judgement ADD COLUMN config_issues boolean NOT NULL DEFAULT false;
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (config_issues) ON public.problem TO %I', plugin_group);
  EXECUTE format('GRANT SELECT (config_issues) ON public.submission_judgement TO %I', plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
END $$;
"#;

const DOWN: &str = r#"
ALTER TABLE public.submission_judgement DROP COLUMN config_issues;
ALTER TABLE public.problem DROP COLUMN config_issues;
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
