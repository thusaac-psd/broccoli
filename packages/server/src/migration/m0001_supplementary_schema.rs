use sea_orm_migration::prelude::*;

/// The schema the entity `sync()` cannot express: partial and named indexes,
/// CHECK constraints and column defaults, the `plugin_user_public` view, the
/// `submission_feedback` table, the optional `pg_stat_statements` extension
/// and the `broccoli_plugin` role with its grants.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0001_supplementary_schema"
    }
}

/// Optional observability extension. Creating it needs privileges a managed
/// app role may lack, or contrib files a Postgres build may lack, so a failure
/// is a warning and boot continues. It runs before [`PLUGIN_ROLE`] so the
/// extension's views get the same grants as every other relation.
const OPTIONAL_EXTENSIONS: &str = r#"
DO $$
BEGIN
  CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
EXCEPTION
  WHEN OTHERS THEN
    RAISE WARNING 'pg_stat_statements not installed: %', SQLERRM;
END $$;
"#;

/// Schema the application relies on. A failure here fails boot.
const SCHEMA: &[&str] = &[
    // New rows default to now(), so an insert that omits the column is valid.
    r#"ALTER TABLE "user" ALTER COLUMN credentials_changed_at SET DEFAULT now()"#,
    r#"ALTER TABLE "user" ADD CONSTRAINT user_display_name_not_blank
        CHECK (btrim(display_name) <> '')"#,
    // Partial UNIQUE indexes. Schema sync drops every unique index whose
    // column set its entity does not declare, so each entity declares a plain
    // index on the same columns. Sync creates that plain index with the table;
    // it is replaced here, and later syncs keep the partial one.
    //
    // One active account per username; a deleted account frees its name.
    r#"DROP INDEX "idx-user-username""#,
    r#"CREATE UNIQUE INDEX idx_user_username_active ON "user" (username)
        WHERE deleted_at IS NULL"#,
    r#"DROP INDEX "idx-submission_judgement-submission_id""#,
    r#"CREATE UNIQUE INDEX idx_submission_judgement_one_current
        ON submission_judgement (submission_id) WHERE is_current"#,
    // Lookups by problem alone are served by the leading column of the
    // problem_attachment and additional_file unique keys on their entities.
    r#"CREATE INDEX idx_submission_user_created ON submission (user_id, created_at)"#,
    r#"CREATE INDEX idx_dlq_resolved_created ON dead_letter_message (resolved, created_at)"#,
    r#"CREATE INDEX idx_clarification_contest_created ON clarification (contest_id, created_at)"#,
    r#"CREATE INDEX idx_test_case_result_judgement ON test_case_result (judgement_id)"#,
    // Problem-scoped lookups resolve the contests a problem belongs to; the
    // composite primary key leads with contest_id and cannot serve them.
    r#"CREATE INDEX contest_problem_problem_id_idx ON contest_problem (problem_id)"#,
    // The dispatcher's due-scan reads only unclaimed timers.
    r#"CREATE INDEX plugin_timer_due_idx ON plugin_timer (fire_at) WHERE claimed_at IS NULL"#,
    // The queued rows, oldest first: the claim scans and the queue-depth
    // counts read them through these, and each index stays as small as the
    // queue. The predicate is `dispatcher::QUEUED_PREDICATE`, which every
    // queued scan uses inline so the planner can match it. A judgement's
    // claim order puts live work before deferred rejudges.
    r#"CREATE INDEX idx_submission_judgement_claim
        ON submission_judgement (is_current DESC, created_at) WHERE status = 'Queued'"#,
    r#"CREATE INDEX idx_code_run_claim ON code_run (created_at) WHERE status = 'Queued'"#,
    r#"CREATE INDEX idx_submission_queued ON submission (created_at) WHERE status = 'Queued'"#,
    // A replica's in-flight code runs, counted for its claim budget. A
    // judgement's in-flight count uses its entity's `status` index.
    r#"CREATE INDEX idx_code_run_in_flight ON code_run (owner_server_id)
        WHERE status IN ('Pending', 'Compiling', 'Running')"#,
    // Curated, PII-free projection of `user` for plugin reads.
    r#"CREATE VIEW plugin_user_public AS
        SELECT id, username FROM "user" WHERE deleted_at IS NULL"#,
    // Withheld-feedback records owned by plugins; `metadata` is opaque to the
    // host. At most one pending record per plugin slot.
    r#"CREATE TABLE submission_feedback (
        submission_id INTEGER PRIMARY KEY REFERENCES submission(id) ON DELETE CASCADE,
        contest_id INTEGER NOT NULL REFERENCES contest(id) ON DELETE CASCADE,
        user_id INTEGER NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
        plugin_id TEXT NOT NULL,
        state_key TEXT NOT NULL,
        outcome TEXT NOT NULL DEFAULT 'pending'
            CONSTRAINT submission_feedback_outcome_check
            CHECK (outcome IN ('pending', 'retired', 'released')),
        metadata JSONB NOT NULL,
        slot_key TEXT,
        public_result JSONB
    )"#,
    r#"CREATE UNIQUE INDEX submission_feedback_one_pending
        ON submission_feedback (plugin_id, slot_key)
        WHERE outcome = 'pending' AND slot_key IS NOT NULL"#,
    r#"CREATE INDEX submission_feedback_match ON submission_feedback (plugin_id, state_key)"#,
    r#"CREATE INDEX submission_feedback_slot
        ON submission_feedback (plugin_id, slot_key, submission_id)"#,
    // Each plugin role's login password (see `plugin_db::schema::provision`).
    r#"CREATE TABLE plugin_login_secret (
        role TEXT PRIMARY KEY,
        password TEXT NOT NULL
    )"#,
];

/// The `broccoli_plugin` role that raw plugin SQL runs under.
///
/// It is a NOLOGIN group role the app role is a member of, with read and
/// write DML on every table and sequence, now and future: contest admins are
/// trusted and install plugins deliberately. It owns no core table, so core
/// schema DDL (`DROP`/`ALTER`) and `TRUNCATE` stay with the app role. Each
/// plugin logs in as its own role, a member of this one, and owns its own
/// schema (see `plugin_db::schema`).
///
/// `CREATE ROLE` needs CREATEROLE, which a managed app role may lack. Then the
/// whole block is skipped with a warning and boot continues degraded: the
/// plugin roles cannot be provisioned either, so raw plugin SQL fails.
const PLUGIN_ROLE: &str = r#"
DO $$
BEGIN
  BEGIN
    CREATE ROLE broccoli_plugin NOLOGIN;
  EXCEPTION
    WHEN duplicate_object THEN NULL;
    WHEN insufficient_privilege THEN
      RAISE WARNING 'broccoli_plugin role not provisioned: the application role lacks CREATEROLE. Raw plugin SQL (host.db.*) will fail until the app role is granted CREATEROLE.';
      RETURN;
  END;

  BEGIN
    GRANT broccoli_plugin TO CURRENT_USER;
    GRANT USAGE, CREATE ON SCHEMA public TO broccoli_plugin;
    GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO broccoli_plugin;
    ALTER DEFAULT PRIVILEGES IN SCHEMA public
      GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO broccoli_plugin;
    GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA public TO broccoli_plugin;
    ALTER DEFAULT PRIVILEGES IN SCHEMA public
      GRANT USAGE, SELECT, UPDATE ON SEQUENCES TO broccoli_plugin;
  EXCEPTION
    WHEN insufficient_privilege THEN
      RAISE WARNING 'broccoli_plugin grants skipped (insufficient privilege): %', SQLERRM;
  END;
END $$;
"#;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(OPTIONAL_EXTENSIONS).await?;
        for stmt in SCHEMA {
            db.execute_unprepared(stmt).await?;
        }
        db.execute_unprepared(&format!(
            "CREATE TABLE {table} (stamp text NOT NULL); \
             INSERT INTO {table} VALUES ('{stamp}')",
            table = super::STAMP_TABLE,
            stamp = super::SCHEMA_STAMP,
        ))
        .await?;
        // Last, so the grants cover every relation created above.
        db.execute_unprepared(PLUGIN_ROLE).await?;
        // Plugins must not rewrite what boot trusts (the schema stamp and the
        // record of applied migrations) nor read the plugin logins.
        db.execute_unprepared(&format!(
            "DO $$ BEGIN \
               IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'broccoli_plugin') THEN \
                 REVOKE INSERT, UPDATE, DELETE ON {table}, seaql_migrations FROM broccoli_plugin; \
                 REVOKE ALL ON plugin_login_secret FROM broccoli_plugin; \
               END IF; \
             END $$",
            table = super::STAMP_TABLE,
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "the baseline schema is not reversible".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use common::StatusPhase;

    use super::SCHEMA;

    fn statement(index: &str) -> &'static str {
        SCHEMA
            .iter()
            .find(|stmt| stmt.split_whitespace().nth(2) == Some(index))
            .unwrap_or_else(|| panic!("{index} is not in the schema"))
    }

    #[test]
    fn queued_indexes_use_the_queued_predicate() {
        for index in [
            "idx_submission_judgement_claim",
            "idx_code_run_claim",
            "idx_submission_queued",
        ] {
            assert!(
                statement(index)
                    .ends_with(&format!("WHERE {}", crate::dispatcher::QUEUED_PREDICATE)),
                "{index}"
            );
        }
    }

    #[test]
    fn in_flight_index_covers_the_in_flight_statuses() {
        assert!(statement("idx_code_run_in_flight").ends_with(&format!(
            "WHERE status IN {}",
            StatusPhase::InFlight.sql_list()
        )));
    }
}
