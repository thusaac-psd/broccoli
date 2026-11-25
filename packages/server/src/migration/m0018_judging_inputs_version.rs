use sea_orm_migration::prelude::*;

/// The version of everything judging reads of a problem
/// (`entity::problem::Model::inputs_version`): its test cases, its time and
/// memory limits, its type, its checker format, its additional files
/// (graders), the type of each contest it is in, and the stored configs of
/// every namespace a plugin declares judging reads (`judging_input`, recorded
/// in `plugin.governs -> 'judging_inputs'`; a checker source is one) at every
/// scope the config cascade reads for it: its own, each contest problem's,
/// each contest's it is in, and the plugin's own. The triggers below count
/// every change to any of them, whatever path makes it, so no write path can
/// forget to (`services::judging::inputs::expected` states them as the code
/// expects them); a config of any other namespace, or a rewrite that changes
/// only a row's `updated_at`, counts nothing. A transaction that restores rows
/// as they were (a bundle import) sets `broccoli.restoring`, and the triggers
/// count nothing in it: a restore is no change.
/// A judgement records the version it was judged against
/// (`entity::submission_judgement::Model::inputs_version`) and whether it was
/// applied anyway after the problem's inputs changed
/// (`judged_on_earlier_inputs`). The version is stamped from the snapshot the
/// dispatch read the problem in, and a plugin reads its configs later, while
/// it judges: a change between the two counts after the stamp, so the
/// judgement reads as judged on earlier inputs even if it read the new
/// config. That only over-labels; a judgement is never labelled current
/// when it read inputs older than its stamp.
///
/// `problem.checker_language` is the language of the problem's checker
/// source: the language its first stored file in a `checker_source`
/// namespace compiles in, by the extensions the plugin declares
/// (`plugin.governs -> 'checker_languages'`), kept by the
/// `broccoli_checker_language` trigger on every write of such a config, and
/// a need of every unit of the problem whose checker format's provider
/// declares languages (`providers::Needs::checker_language`).
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
ALTER TABLE public.problem ADD COLUMN checker_language character varying;
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
  EXECUTE format('GRANT SELECT (inputs_version, checker_language) ON public.problem TO %I',
    plugin_group);
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
  config_plugin text;
  config_namespace text;
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
    IF TG_TABLE_NAME <> 'plugin_config' THEN
      UPDATE public.problem SET inputs_version = inputs_version + 1
       WHERE id = (changed->>'problem_id')::integer;
      CONTINUE;
    END IF;
    IF changed->>'scope' = 'plugin' THEN
      config_plugin := changed->>'ref_id';
      config_namespace := changed->>'namespace';
    ELSE
      config_plugin := split_part(changed->>'namespace', ':', 1);
      config_namespace := substring(changed->>'namespace' FROM '^[^:]*:(.*)$');
    END IF;
    CONTINUE WHEN NOT EXISTS (
      SELECT 1 FROM public.plugin p
       WHERE p.id = config_plugin AND p.governs -> 'judging_inputs' ? config_namespace);
    CASE changed->>'scope'
      WHEN 'problem' THEN
        UPDATE public.problem SET inputs_version = inputs_version + 1
         WHERE id = substring(changed->>'ref_id' FROM '^([0-9]+)$')::integer;
      WHEN 'contest_problem' THEN
        UPDATE public.problem SET inputs_version = inputs_version + 1
         WHERE id = substring(changed->>'ref_id' FROM '^[0-9]+:([0-9]+)$')::integer;
      WHEN 'contest' THEN
        UPDATE public.problem SET inputs_version = inputs_version + 1
         WHERE id IN (SELECT cp.problem_id FROM public.contest_problem cp
                       WHERE cp.contest_id
                             = substring(changed->>'ref_id' FROM '^([0-9]+)$')::integer);
      WHEN 'plugin' THEN
        UPDATE public.problem SET inputs_version = inputs_version + 1;
      ELSE
        NULL;
    END CASE;
  END LOOP;
  RETURN NULL;
END $$;
CREATE TRIGGER broccoli_inputs_changed AFTER INSERT OR UPDATE OR DELETE ON public.test_case
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed();
CREATE TRIGGER broccoli_inputs_changed AFTER INSERT OR UPDATE OR DELETE ON public.additional_file
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed();
CREATE TRIGGER broccoli_inputs_changed AFTER INSERT OR UPDATE OR DELETE ON public.plugin_config
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed();
CREATE OR REPLACE FUNCTION public.broccoli_contest_inputs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF current_setting('broccoli.restoring', true) = 'on' THEN
    RETURN NULL;
  END IF;
  IF NEW.contest_type IS DISTINCT FROM OLD.contest_type THEN
    UPDATE public.problem SET inputs_version = inputs_version + 1
     WHERE id IN (SELECT cp.problem_id FROM public.contest_problem cp
                   WHERE cp.contest_id = NEW.id);
  END IF;
  RETURN NULL;
END $$;
CREATE TRIGGER broccoli_inputs_changed AFTER UPDATE ON public.contest
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_contest_inputs_changed();
CREATE OR REPLACE FUNCTION public.broccoli_checker_language() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  changed jsonb;
  target integer;
  config_plugin text;
  config_namespace text;
  language text;
BEGIN
  IF current_setting('broccoli.restoring', true) = 'on' THEN
    RETURN NULL;
  END IF;
  changed := CASE WHEN TG_OP = 'DELETE' THEN to_jsonb(OLD) ELSE to_jsonb(NEW) END;
  IF changed->>'scope' IS DISTINCT FROM 'problem' THEN
    RETURN NULL;
  END IF;
  target := substring(changed->>'ref_id' FROM '^([0-9]+)$')::integer;
  config_plugin := split_part(changed->>'namespace', ':', 1);
  config_namespace := substring(changed->>'namespace' FROM '^[^:]*:(.*)$');
  IF target IS NULL OR NOT EXISTS (
       SELECT 1 FROM public.plugin p, jsonb_array_elements(p.governs -> 'checker_languages') l
        WHERE p.id = config_plugin AND l->>'namespace' = config_namespace) THEN
    RETURN NULL;
  END IF;
  IF TG_OP <> 'DELETE' THEN
    SELECT l->>'language' INTO language
      FROM jsonb_array_elements(changed->'config'->'files') WITH ORDINALITY AS f(file, n)
      JOIN public.plugin p ON p.id = config_plugin
      CROSS JOIN jsonb_array_elements(p.governs -> 'checker_languages') l
     WHERE l->>'namespace' = config_namespace
       AND lower(f.file->>'filename') LIKE '%.' || (l->>'extension')
     ORDER BY f.n
     LIMIT 1;
  END IF;
  UPDATE public.problem SET checker_language = language
   WHERE id = target AND checker_language IS DISTINCT FROM language;
  RETURN NULL;
END $$;
CREATE TRIGGER broccoli_checker_language AFTER INSERT OR UPDATE OR DELETE ON public.plugin_config
  FOR EACH ROW EXECUTE FUNCTION public.broccoli_checker_language();
"#;

const DOWN: &str = r#"
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.problem;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.test_case;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.additional_file;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.plugin_config;
DROP TRIGGER IF EXISTS broccoli_inputs_changed ON public.contest;
DROP TRIGGER IF EXISTS broccoli_checker_language ON public.plugin_config;
DROP FUNCTION IF EXISTS public.broccoli_checker_language();
DROP FUNCTION IF EXISTS public.broccoli_contest_inputs_changed();
DROP FUNCTION IF EXISTS public.broccoli_problem_inputs_changed();
DROP FUNCTION IF EXISTS public.broccoli_inputs_changed();
ALTER TABLE public.submission_judgement DROP COLUMN judged_on_earlier_inputs;
ALTER TABLE public.submission_judgement DROP COLUMN inputs_version;
ALTER TABLE public.problem DROP COLUMN checker_language;
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
