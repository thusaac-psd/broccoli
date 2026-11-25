use sea_orm_migration::prelude::*;

/// How many times a unit's attempt was handed back because its plugins found
/// no plugin SQL connection free (`contended_handbacks`, on a judgement and
/// on a code run): contention with the work beside it hands an attempt back
/// spending nothing, but only up to `judging::fault::CONTENTION_HANDBACKS`
/// times per unit, after which it is a judging failure within the
/// submission's retry budget. Counted on the row, so the cap holds across
/// replicas and restarts. Plugins read both tables, so the plugin group role
/// may read the new columns as it reads every other column that is not
/// content.
///
/// The plugin group role holds no grant to signal a backend
/// (`pg_terminate_backend`, `pg_cancel_backend`, `pg_signal_backend`): a
/// plugin that cut a connection could pass its own failure off as the
/// database lost. The plugin SQL guard refuses any statement naming them
/// (`host_funcs::sql`), and a lost plugin connection is confirmed against the
/// host pool (`plugin_db::PluginSqlPools::confirmed`), since PUBLIC keeps
/// its default right to the functions.
///
/// Additive (release/docs/upgrade.md): defaulted columns servers of the
/// previous release keep working against.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0023_dispatch_contention"
    }
}

const UP: &str = r#"
ALTER TABLE public.submission_judgement
    ADD COLUMN contended_handbacks integer NOT NULL DEFAULT 0;
ALTER TABLE public.code_run ADD COLUMN contended_handbacks integer NOT NULL DEFAULT 0;
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (contended_handbacks) ON public.submission_judgement TO %I',
    plugin_group);
  EXECUTE format('GRANT SELECT (contended_handbacks) ON public.code_run TO %I', plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
END $$;
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format(
    'REVOKE EXECUTE ON FUNCTION pg_catalog.pg_terminate_backend(integer, bigint), '
    || 'pg_catalog.pg_cancel_backend(integer) FROM %I', plugin_group);
  IF EXISTS (SELECT 1 FROM pg_auth_members m
              WHERE m.roleid = 'pg_signal_backend'::regrole
                AND m.member = plugin_group::regrole) THEN
    EXECUTE format('REVOKE pg_signal_backend FROM %I', plugin_group);
  END IF;
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group revokes skipped (insufficient privilege): %', SQLERRM;
END $$;
"#;

const DOWN: &str = r#"
ALTER TABLE public.code_run DROP COLUMN contended_handbacks;
ALTER TABLE public.submission_judgement DROP COLUMN contended_handbacks;
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
