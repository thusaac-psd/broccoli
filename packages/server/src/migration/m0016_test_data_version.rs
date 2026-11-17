use sea_orm_migration::prelude::*;

/// How many times a problem's test data changed
/// (`entity::problem::Model::test_data_version`), and the version a
/// judgement was judged against
/// (`entity::submission_judgement::Model::test_data_version`).
///
/// Plugins read both tables, so the plugin group role (named in
/// `plugin_group_role`, see m0001) may read the new columns as it reads every
/// other column that is not content.
///
/// Forward-only like every migration here.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0016_test_data_version"
    }
}

const UP: &str = r#"
ALTER TABLE public.problem ADD COLUMN test_data_version integer NOT NULL DEFAULT 0;
ALTER TABLE public.submission_judgement ADD COLUMN test_data_version integer NOT NULL DEFAULT 0;
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (test_data_version) ON public.problem TO %I', plugin_group);
  EXECUTE format('GRANT SELECT (test_data_version) ON public.submission_judgement TO %I', plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
END $$;
"#;

const DOWN: &str = r#"
ALTER TABLE public.submission_judgement DROP COLUMN test_data_version;
ALTER TABLE public.problem DROP COLUMN test_data_version;
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
