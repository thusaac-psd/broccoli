//! The version of a problem's judging inputs.
//!
//! Everything judging reads of a problem is one versioned whole: its test
//! cases, its time and memory limits, its type, its checker format, its
//! additional files (graders), its default contest type, which contests it
//! is in and each one's type, and the stored configs of every namespace a
//! plugin declares judging reads (`ConfigNamespace::is_judging_input`,
//! recorded in its `Governs`) at the problem's, a contest problem's and a
//! contest's scope (every problem of the contest). A plugin-scope config of
//! one moves its plugin's `inputs_epoch`, which the stamp records for each
//! plugin the judgement is judged by (`Unit::plugin_epochs`) and the apply
//! check compares. A config of any other namespace counts nothing.
//! `problem.inputs_version` counts every change to any of them, counted by
//! database triggers (see [`expected`]), so every write path counts it
//! without knowing to. A transaction that restores rows as they were (a
//! bundle import) sets [`RESTORING`] and the triggers count nothing in it:
//! the restored problems and judgements keep the versions they were saved
//! with. The dispatch reads the
//! problem and its test cases in one snapshot and stamps the judgement with
//! that snapshot's version ([`stamp_inputs_version`]), so the version names
//! the inputs the judgement was judged with. A config a plugin reads while it
//! judges may change after the stamp; that change counts after it, so the
//! judgement reads as judged on earlier inputs: it only over-labels. Applying a judgement of another
//! version needs `force` and marks it (`judging::apply_judgement`).

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

/// The statement that marks the running transaction as restoring rows as
/// they were, so the inputs triggers count nothing in it.
pub const RESTORING: &str = "SET LOCAL broccoli.restoring = 'on'";

/// Records that judgement `judgement_id`, at epoch `judge_epoch`, is judged
/// against inputs version `version`, read in the same snapshot as the
/// inputs, and the inputs epoch of each plugin it is judged by
/// (`Unit::plugin_epochs`). Fenced on the epoch: a dispatch the platform
/// took back stamps nothing.
pub(crate) async fn stamp_inputs_version(
    db: &DatabaseConnection,
    judgement_id: i32,
    judge_epoch: i32,
    version: i32,
) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        format!(
            "UPDATE submission_judgement u SET inputs_version = $3, plugin_epochs = {epochs} \
               FROM submission s \
              WHERE s.id = u.submission_id AND u.id = $1 AND u.judge_epoch = $2 \
                AND NOT u.is_finalized",
            epochs = super::Unit::plugin_epochs(),
        ),
        [judgement_id.into(), judge_epoch.into(), version.into()],
    ))
    .await?;
    Ok(())
}

/// The trigger definitions that count a problem's input changes, as code.
/// Migration m0018 creates them in every database; the schema test
/// `the_migrations_create_the_schema_the_code_expects` runs [`install`]
/// against a migrated database and fails if it changes anything, so a change
/// here ships with a migration that makes the same change.
#[cfg(test)]
pub(crate) mod expected {
    use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

    /// The tables whose rows belong to one problem's inputs.
    const INPUT_TABLES: [&str; 3] = ["test_case", "additional_file", "plugin_config"];

    const TRIGGER: &str = "broccoli_inputs_changed";

    /// Counts a change to the problem's own inputs (its limits, type and
    /// checker format) on the row being written.
    const PROBLEM_FUNCTION: &str =
        "CREATE OR REPLACE FUNCTION public.broccoli_problem_inputs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF current_setting('broccoli.restoring', true) = 'on' THEN
    RETURN NEW;
  END IF;
  IF NEW.time_limit IS DISTINCT FROM OLD.time_limit
     OR NEW.memory_limit IS DISTINCT FROM OLD.memory_limit
     OR NEW.problem_type IS DISTINCT FROM OLD.problem_type
     OR NEW.checker_format IS DISTINCT FROM OLD.checker_format
     OR NEW.default_contest_type IS DISTINCT FROM OLD.default_contest_type THEN
    NEW.inputs_version := OLD.inputs_version + 1;
  END IF;
  RETURN NEW;
END $$";

    /// Counts a change to a row of an input table on the problems it
    /// reaches: a test case's or additional file's `problem_id`; for a
    /// config of a namespace its plugin's recorded `governs` names a judging
    /// input, the problem- or contest-problem-scoped config's problem, each
    /// problem of a contest-scoped config's contest, and every problem for a
    /// plugin-scoped one. A config of another namespace, or a rewrite that
    /// changes only `updated_at` (a config saved as it was), changes nothing
    /// judging reads, so it counts nothing.
    ///
    /// A contest's problems are locked by id before they are counted, here
    /// and in [`CONTEST_FUNCTION`], so two statements that count on
    /// overlapping contests take their rows in one order and never deadlock.
    const ROW_FUNCTION: &str =
        "CREATE OR REPLACE FUNCTION public.broccoli_inputs_changed() RETURNS trigger
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
         WHERE id IN (SELECT p.id FROM public.problem p
                        JOIN public.contest_problem cp ON cp.problem_id = p.id
                       WHERE cp.contest_id
                             = substring(changed->>'ref_id' FROM '^([0-9]+)$')::integer
                       ORDER BY p.id FOR UPDATE OF p);
      WHEN 'plugin' THEN
        UPDATE public.plugin SET inputs_epoch = inputs_epoch + 1 WHERE id = config_plugin;
      ELSE
        NULL;
    END CASE;
  END LOOP;
  RETURN NULL;
END $$";

    /// Counts a change of a contest's type on every problem of the contest:
    /// the contest type's plugin judges them.
    const CONTEST_FUNCTION: &str =
        "CREATE OR REPLACE FUNCTION public.broccoli_contest_inputs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  IF current_setting('broccoli.restoring', true) = 'on' THEN
    RETURN NULL;
  END IF;
  IF NEW.contest_type IS DISTINCT FROM OLD.contest_type THEN
    UPDATE public.problem SET inputs_version = inputs_version + 1
     WHERE id IN (SELECT p.id FROM public.problem p
                    JOIN public.contest_problem cp ON cp.problem_id = p.id
                   WHERE cp.contest_id = NEW.id
                   ORDER BY p.id FOR UPDATE OF p);
  END IF;
  RETURN NULL;
END $$";

    async fn has_trigger<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, DbErr> {
        has_named_trigger(db, TRIGGER, table).await
    }

    async fn has_named_trigger<C: ConnectionTrait>(
        db: &C,
        trigger: &str,
        table: &str,
    ) -> Result<bool, DbErr> {
        Ok(db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM pg_trigger WHERE tgname = $1 AND tgrelid = $2::regclass",
                [trigger.into(), table.into()],
            ))
            .await?
            .is_some())
    }

    /// Replaces the trigger functions and creates each table's trigger if it
    /// is missing.
    pub async fn install<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
        db.execute_unprepared(PROBLEM_FUNCTION).await?;
        db.execute_unprepared(ROW_FUNCTION).await?;
        db.execute_unprepared(CONTEST_FUNCTION).await?;
        if !has_trigger(db, "contest").await? {
            db.execute_unprepared(&format!(
                "CREATE TRIGGER {TRIGGER} AFTER UPDATE ON public.contest \
                 FOR EACH ROW EXECUTE FUNCTION public.broccoli_contest_inputs_changed()"
            ))
            .await?;
        }
        if !has_trigger(db, "problem").await? {
            db.execute_unprepared(&format!(
                "CREATE TRIGGER {TRIGGER} BEFORE UPDATE ON public.problem \
                 FOR EACH ROW EXECUTE FUNCTION public.broccoli_problem_inputs_changed()"
            ))
            .await?;
        }
        if !has_trigger(db, "contest_problem").await? {
            db.execute_unprepared(&format!(
                "CREATE TRIGGER {TRIGGER} \
                 AFTER INSERT OR DELETE OR UPDATE OF contest_id, problem_id \
                 ON public.contest_problem \
                 FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed()"
            ))
            .await?;
        }
        for table in INPUT_TABLES {
            if !has_trigger(db, table).await? {
                db.execute_unprepared(&format!(
                    "CREATE TRIGGER {TRIGGER} AFTER INSERT OR UPDATE OR DELETE ON public.{table} \
                     FOR EACH ROW EXECUTE FUNCTION public.broccoli_inputs_changed()"
                ))
                .await?;
            }
        }
        Ok(())
    }
}
