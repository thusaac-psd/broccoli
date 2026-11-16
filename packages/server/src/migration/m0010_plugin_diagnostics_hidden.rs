use sea_orm_migration::prelude::*;

/// The `error_message` diagnostics of a failed submission, judgement or code
/// run are content columns (`plugin_core::manifest::SQL_READ_SCOPES`): the
/// host shows them only to staff with `system:view`, so a plugin reads them
/// only by declaring them. The plugin group role (named in
/// `plugin_group_role`, see m0001) gives up its grant on them; a plugin that
/// declares one is granted it when its role is provisioned.
///
/// Rolling-safe: a privilege change no replica's own statements depend on.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0010_plugin_diagnostics_hidden"
    }
}

const UP: &str = r#"
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('REVOKE SELECT (error_message) ON public.submission FROM %I', plugin_group);
  EXECUTE format('REVOKE SELECT (error_message) ON public.submission_judgement FROM %I', plugin_group);
  EXECUTE format('REVOKE SELECT (error_message) ON public.code_run FROM %I', plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group revokes skipped (insufficient privilege): %', SQLERRM;
END $$;
"#;

const DOWN: &str = r#"
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (error_message) ON public.submission TO %I', plugin_group);
  EXECUTE format('GRANT SELECT (error_message) ON public.submission_judgement TO %I', plugin_group);
  EXECUTE format('GRANT SELECT (error_message) ON public.code_run TO %I', plugin_group);
END $$;
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
