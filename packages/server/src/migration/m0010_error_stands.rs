use sea_orm_migration::prelude::*;

/// Whether a judgement's system error stands
/// (`entity::submission_judgement::Model::error_stands`).
///
/// Plugins read the table, so the plugin group role (named in
/// `plugin_group_role`, see m0001) may read the new column as it reads every
/// other column that is not content.
///
/// Every system error decided before the column existed stands: one that
/// is not current (no retry takes it), and one its submission already shows
/// as its final result (a terminal status at the judgement's epoch). A
/// current one whose submission still shows the attempt in flight awaits the
/// system-error retry's decision and keeps `false`.
///
/// Rolling-safe: a defaulted column older replicas never read or write. A
/// system error an older replica writes meanwhile is mirrored onto its
/// submission at once, which the retry reads as decided
/// (`judging::unit::awaits_retry_decision`).
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0010_error_stands"
    }
}

const UP: &str = r#"
ALTER TABLE public.submission_judgement ADD COLUMN error_stands boolean NOT NULL DEFAULT false;
UPDATE public.submission_judgement j SET error_stands = true
  FROM public.submission s
 WHERE s.id = j.submission_id AND j.is_finalized
   AND (j.status = 'SystemError'
        OR (j.status IN ('Judged', 'CompilationError') AND j.verdict = 'SystemError'))
   AND (NOT j.is_current
        OR (s.status IN ('Judged', 'CompilationError', 'SystemError')
            AND s.judge_epoch = j.judge_epoch));
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (error_stands) ON public.submission_judgement TO %I', plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
END $$;
"#;

const DOWN: &str = r#"
ALTER TABLE public.submission_judgement DROP COLUMN error_stands;
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
