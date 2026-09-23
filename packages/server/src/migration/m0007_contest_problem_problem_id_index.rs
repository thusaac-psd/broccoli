use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // `contest_problem`'s primary key is (contest_id, problem_id), which
        // Postgres backs with a composite B-tree keyed on contest_id FIRST.
        // That index cannot serve a lookup filtered on problem_id alone.
        //
        // Such a lookup is now on a hot path: the afternoon-bracket plugin's
        // `decide_visibility` resolves context-free problem ids (the
        // `Resource::Problem { contest_id: None }` raised by
        // `GET /problems/{id}`, attachment download, test-case reads and
        // `create_submission`) to their owning bracket contests with
        //
        //   SELECT ... FROM contest_problem cp JOIN contest c ON c.id = cp.contest_id
        //   WHERE c.contest_type = '...' AND cp.problem_id IN (...)
        //
        // Because a registered visibility querier is consulted for EVERY
        // problem decision platform-wide, that query runs on every standalone
        // problem read by every user, whether or not any bracket contest
        // exists. Without this index it degrades to a scan of contest_problem,
        // which grows with (contests x problems).
        //
        // Index only, no behaviour change. Named explicitly and created
        // IF NOT EXISTS so it is idempotent on an already-migrated deployment.
        manager
            .get_connection()
            .execute_unprepared(
                "CREATE INDEX IF NOT EXISTS contest_problem_problem_id_idx \
                 ON contest_problem (problem_id);",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS contest_problem_problem_id_idx;")
            .await?;
        Ok(())
    }
}
