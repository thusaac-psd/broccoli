use sea_orm_migration::prelude::*;

/// The version of everything judging reads of a problem
/// (`entity::problem::Model::inputs_version`): its test cases, its time and
/// memory limits, its type, its checker format, its additional files (graders)
/// and its problem-scoped plugin configs (its checker source among them). The
/// triggers below count every change to any of them, whatever path makes it,
/// so no write path can forget to (`services::judging::inputs::expected`
/// states them as the code expects them); a rewrite that changes only a row's
/// `updated_at` counts nothing. A transaction that restores rows
/// as they were (a bundle import) sets `broccoli.restoring`, and the triggers
/// count nothing in it: a restore is no change.
/// A judgement records the version it was judged against
/// (`entity::submission_judgement::Model::inputs_version`) and whether it was
/// applied anyway after the problem's inputs changed
/// (`judged_on_earlier_inputs`).
///
/// Plugins read both tables, so the plugin group role (named in
/// `plugin_group_role`, see m0001) may read the new columns as it reads every
/// other column that is not content.
///
/// Additive (release/docs/upgrade.md): servers of the previous release keep
/// working against it, so a release that adds only migrations like it rolls
/// server by server.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0018_judging_inputs_version"
    }
}

const UP: &str = r#"
ALTER TABLE public.problem ADD COLUMN inputs_version integer NOT NULL DEFAULT 0;
ALTER TABLE public.submission_judgement ADD COLUMN inputs_version integer NOT NULL DEFAULT 0;
ALTER TABLE public.submission_judgement
    ADD COLUMN judged_on_earlier_inputs boolean NOT NULL DEFAULT false;
DO $$
DECLARE
  plugin_group name;
BEGIN
  SELECT g.name INTO plugin_group FROM public.plugin_group_role g LIMIT 1;
  IF plugin_group IS NULL THEN
    RETURN;
  END IF;
  EXECUTE format('GRANT SELECT (inputs_version) ON public.problem TO %I', plugin_group);
  EXECUTE format(
    'GRANT SELECT (inputs_version, judged_on_earlier_inputs) ON public.submission_judgement TO %I',
    plugin_group);
EXCEPTION
  WHEN insufficient_privilege THEN
    RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
END $$;
CREATE OR REPLACE FUNCTION public.broccoli_problem_inputs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF current_setting('broccoli.restoring', true) = 'on' THEN
    RETURN NEW;
  END IF;
  IF NEW.time_limit IS DISTINCT FROM OLD.time_limit
     OR NEW.memory_limit IS DISTINCT FROM OLD.memory_limit
     OR NEW.problem_type IS DISTINCT FROM OLD.problem_type
     OR NEW.checker_format IS DISTINCT FROM OLD.checker_format THEN
    NEW.inputs_version := OLD.inputs_version + 1;
  END IF;
  RETURN NEW;
END $$;
CREATE TRIGGER broccoli_inputs_changed BEFORE UPDATE ON public.problem
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_problem_inputs_changed();
CREATE OR REPLACE FUNCTION public.broccoli_inputs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  changed jsonb;
  target integer;
BEGIN
  IF current_setting('broccoli.restoring', true) = 'on' THEN
    RETURN NULL;
  END IF;
  IF TG_OP = 'UPDATE' AND to_jsonb(OLD) - 'updated_at' = to_jsonb(NEW) - 'updated_at' THEN
    RETURN NULL;
  END IF;
  FOR changed IN
    SELECT r FROM unnest(ARRAY[
      CASE WHEN TG_OP <> 'INSERT' THEN to_jsonb(OLD) END,
      CASE WHEN TG_OP <> 'DELETE' THEN to_jsonb(NEW) END]) AS r
    WHERE r IS NOT NULL
  LOOP
    IF TG_TABLE_NAME = 'plugin_config' THEN
      target := CASE changed->>'scope'
        WHEN 'problem' THEN substring(changed->>'ref_id' FROM '^([0-9]+)$')::integer
        WHEN 'contest_problem' THEN substring(changed->>'ref_id' FROM '^[0-9]+:([0-9]+)$')::integer
      END;
    ELSE
      target := (changed->>'problem_id')::integer;
    END IF;
    IF target IS NOT NULL THEN
      UPDATE public.problem SET inputs_version = inputs_version + 1 WHERE id = target;
    END IF;
  END LOOP;
  RETURN NULL;
END $$;
CREATE TRIGGER broccoli_inputs_changed AFTER INSERT OR UPDATE OR DELETE ON public.test_case
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed();
CREATE TRIGGER broccoli_inputs_changed AFTER INSERT OR UPDATE OR DELETE ON public.additional_file
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed();
CREATE TRIGGER broccoli_inputs_changed AFTER INSERT OR UPDATE OR DELETE ON public.plugin_config
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.problem;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.test_case;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.additional_file;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.plugin_config;
DROP FUNCTION IF EXISTS public.broccoli_problem_inputs_changed();
DROP FUNCTION IF EXISTS public.broccoli_inputs_changed();
ALTER TABLE public.submission_judgement DROP COLUMN judged_on_earlier_inputs;
ALTER TABLE public.submission_judgement DROP COLUMN inputs_version;
ALTER TABLE public.problem DROP COLUMN inputs_version;
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
