use sea_orm_migration::prelude::*;

/// Active usernames are unique case-insensitively: `idx_user_username_active`
/// is on `lower(username)` (`entity::user::username_key_sql`), so `Admin`
/// cannot register next to `admin`.
///
/// Rolling-safe: the index only narrows which inserts succeed, and an older
/// replica answers its unique violation as a taken username. A database whose
/// active accounts already differ only in case cannot take the index; the
/// migration names those accounts and stops, changing nothing, so an operator
/// renames or deletes them first.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0003_username_case_insensitive"
    }
}

const UP: &str = r#"
DO $$
DECLARE
  clashes text;
BEGIN
  SELECT string_agg(names, '; ') INTO clashes FROM (
    SELECT string_agg(username, ', ' ORDER BY username) AS names
    FROM "user" WHERE deleted_at IS NULL
    GROUP BY lower(username) HAVING count(*) > 1
  ) dup;
  IF clashes IS NOT NULL THEN
    RAISE EXCEPTION 'active accounts whose usernames differ only in case: %; rename or delete all but one of each before upgrading', clashes;
  END IF;
END $$;
DROP INDEX idx_user_username_active;
CREATE UNIQUE INDEX idx_user_username_active ON "user" (lower((username)::text))
  WHERE deleted_at IS NULL;
"#;

const DOWN: &str = r#"
DROP INDEX idx_user_username_active;
CREATE UNIQUE INDEX idx_user_username_active ON "user" (username) WHERE deleted_at IS NULL;
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
