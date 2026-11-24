//! The version of a problem's judging inputs.
//!
//! Everything judging reads of a problem is one versioned whole: its test
//! cases, its time and memory limits, its type, its checker format, its
//! additional files (graders) and its problem-scoped plugin configs (its
//! checker source among them, and a contest's per-problem configs).
//! `problem.inputs_version` counts every change to any of them, counted by
//! database triggers (see [`expected`]), so every write path, a bundle
//! import included, counts it without knowing to. The dispatch reads the
//! problem and its test cases in one snapshot and stamps the judgement with
//! that snapshot's version ([`stamp_inputs_version`]), so the version names
//! the inputs the judgement was judged with. Applying a judgement of another
//! version needs `force` and marks it (`judging::apply_judgement`).

use common::db::DatabaseConnection;
use sea_orm::{ConnectionTrait, DbBackend, DbErr, Statement};

/// Records that judgement `judgement_id`, at epoch `judge_epoch`, is judged
/// against inputs version `version`, read in the same snapshot as the
/// inputs. Fenced on the epoch: a dispatch the platform took back stamps
/// nothing.
pub(crate) async fn stamp_inputs_version(
    db: &DatabaseConnection,
    judgement_id: i32,
    judge_epoch: i32,
    version: i32,
) -> Result<(), DbErr> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE submission_judgement SET inputs_version = $3 \
          WHERE id = $1 AND judge_epoch = $2 AND NOT is_finalized",
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
  IF NEW.time_limit IS DISTINCT FROM OLD.time_limit
     OR NEW.memory_limit IS DISTINCT FROM OLD.memory_limit
     OR NEW.problem_type IS DISTINCT FROM OLD.problem_type
     OR NEW.checker_format IS DISTINCT FROM OLD.checker_format THEN
    NEW.inputs_version := OLD.inputs_version + 1;
  END IF;
  RETURN NEW;
END $$";

    /// Counts a change to a row of an input table on the problem it belongs
    /// to: a test case's or additional file's `problem_id`, a problem- or
    /// contest-problem-scoped config's problem.
    const ROW_FUNCTION: &str =
        "CREATE OR REPLACE FUNCTION public.broccoli_inputs_changed() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  changed jsonb;
  target integer;
BEGIN
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
END $$";

    async fn has_trigger<C: ConnectionTrait>(db: &C, table: &str) -> Result<bool, DbErr> {
        Ok(db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT 1 FROM pg_trigger WHERE tgname = $1 AND tgrelid = $2::regclass",
                [TRIGGER.into(), table.into()],
            ))
            .await?
            .is_some())
    }

    /// Replaces the trigger functions and creates each table's trigger if it
    /// is missing.
    pub async fn install<C: ConnectionTrait>(db: &C) -> Result<(), DbErr> {
        db.execute_unprepared(PROBLEM_FUNCTION).await?;
        db.execute_unprepared(ROW_FUNCTION).await?;
        if !has_trigger(db, "problem").await? {
            db.execute_unprepared(&format!(
                "CREATE TRIGGER {TRIGGER} BEFORE UPDATE ON public.problem \
                 FOR EACH ROW EXECUTE FUNCTION public.broccoli_problem_inputs_changed()"
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
