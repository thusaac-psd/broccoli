use sea_orm_migration::prelude::*;

/// The schema the entity `sync()` cannot express: partial and named indexes,
/// CHECK constraints and column defaults, the
/// `submission_feedback` and `plugin_login_secret` tables, the optional
/// `pg_stat_statements` extension and the database's plugin group role with
/// its grants.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0001_supplementary_schema"
    }
}

/// Optional observability extension. Creating it needs privileges a managed
/// app role may lack, or contrib files a Postgres build may lack, so a failure
/// is a warning and boot continues.
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
    // The database's plugin group role (see `plugin_db::schema::group_role`).
    r#"CREATE TABLE plugin_group_role (name TEXT NOT NULL)"#,
    // Each plugin role's login password (see `plugin_db::schema::provision`).
    r#"CREATE TABLE plugin_login_secret (
        role TEXT PRIMARY KEY,
        password TEXT NOT NULL
    )"#,
];

/// Host relations every plugin may read: contest data with no secret in it.
/// Their content columns ([`broccoli_types::manifest::SQL_READ_SCOPES`]) are
/// left out and granted to a plugin only by its manifest's `[server.sql]
/// read` (see `plugin_db::schema::provision`). Every other host relation is
/// unreadable to plugins; the integration test
/// `plugin_db::every_host_relation_is_classified` requires each one to be
/// classified.
pub const PLUGIN_READABLE: &[&str] = &[
    "additional_file",
    "clarification",
    "clarification_reply",
    "code_run",
    "code_run_result",
    "contest",
    "contest_problem",
    "contest_user",
    "problem",
    "problem_attachment",
    "submission",
    "submission_judgement",
    "test_case",
    "test_case_result",
];

/// Host relations plugins may read only these columns of: an account's public
/// profile, never its credentials.
pub const PLUGIN_READABLE_COLUMNS: &[(&str, &[&str])] = &[(
    "user",
    &["id", "username", "display_name", "created_at", "deleted_at"],
)];

/// The database's plugin group role, `group`, every plugin role belongs to
/// (`plugin_db::schema::group_role`), and the only privileges
/// plugins hold outside their
/// own schemas: `USAGE` on schema `public` and `SELECT` on
/// [`PLUGIN_READABLE`] (but its content columns) and
/// [`PLUGIN_READABLE_COLUMNS`]. No write, `TRUNCATE`,
/// sequence or `CREATE` privilege on anything the host owns, so host state
/// changes only through host functions, with their guards.
///
/// `CREATE ROLE` needs CREATEROLE, which a managed app role may lack. Then the
/// block is skipped with a warning and boot continues; plugin roles cannot be
/// provisioned either, so raw plugin SQL fails.
fn plugin_role(group: &str) -> String {
    use crate::plugin_db::schema::{content_columns, quote_ident};
    let group_literal = format!("'{group}'");
    let group_table = crate::plugin_db::schema::GROUP_TABLE;
    let group = quote_ident(group);
    let mut grants = String::new();
    for table in PLUGIN_READABLE {
        let content = content_columns(table);
        if content.is_empty() {
            grants.push_str(&format!(
                "GRANT SELECT ON {} TO {group};\n",
                quote_ident(table)
            ));
            continue;
        }
        // Every column but the content ones, read from the catalog.
        let content = content
            .iter()
            .map(|c| format!("'{c}'"))
            .collect::<Vec<_>>()
            .join(", ");
        grants.push_str(&format!(
            "EXECUTE (SELECT format('GRANT SELECT (%s) ON %I TO %s', \
               string_agg(quote_ident(attname), ', ' ORDER BY attnum), '{table}', '{group}') \
             FROM pg_attribute WHERE attrelid = 'public.{t}'::regclass AND attnum > 0 \
               AND NOT attisdropped AND attname <> ALL (ARRAY[{content}]::name[]));\n",
            t = quote_ident(table),
        ));
    }
    for (table, columns) in PLUGIN_READABLE_COLUMNS {
        let columns = columns
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        grants.push_str(&format!(
            "GRANT SELECT ({columns}) ON {} TO {group};\n",
            quote_ident(table)
        ));
    }
    format!(
        r#"
DO $$
BEGIN
  BEGIN
    CREATE ROLE {group} NOLOGIN;
  EXCEPTION
    WHEN duplicate_object THEN NULL;
    WHEN insufficient_privilege THEN
      RAISE WARNING 'plugin group role not provisioned: the application role lacks CREATEROLE. Raw plugin SQL (host.db.*) will fail until the app role is granted CREATEROLE.';
      RETURN;
  END;
  INSERT INTO {group_table} (name) VALUES ({group_literal});

  BEGIN
    REVOKE CREATE ON SCHEMA public FROM PUBLIC;
    GRANT USAGE ON SCHEMA public TO {group};
    {grants}
  EXCEPTION
    WHEN insufficient_privilege THEN
      RAISE WARNING 'plugin group grants skipped (insufficient privilege): %', SQLERRM;
  END;
END $$;
"#
    )
}

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
        let database: String = db
            .query_one_raw(sea_orm_migration::sea_orm::Statement::from_string(
                manager.get_database_backend(),
                "SELECT current_database()::text AS database",
            ))
            .await?
            .ok_or_else(|| DbErr::Custom("current_database() returned no row".into()))?
            .try_get("", "database")?;
        let group = crate::plugin_db::schema::group_role(&database);
        db.execute_unprepared(&plugin_role(&group)).await?;
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
