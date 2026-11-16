use sea_orm_migration::prelude::*;

/// The baseline schema: every host table with its indexes, constraints and
/// triggers ([`SCHEMA`]), the optional `pg_stat_statements` extension and the
/// database's plugin group role with its grants.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0001_baseline"
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

/// Every table, index, constraint, sequence, function and trigger of the
/// baseline schema. A failure here fails boot.
const SCHEMA: &str = include_str!("m0001_baseline.sql");

/// Host relations every plugin may read: contest data with no secret in it.
/// Their content columns ([`plugin_core::manifest::SQL_READ_SCOPES`]) are
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
        db.execute_unprepared(SCHEMA).await?;
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
