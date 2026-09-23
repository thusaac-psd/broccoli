use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Measured, not assumed. An earlier version of this comment claimed
        // the composite primary key (contest_id, problem_id) "cannot serve a
        // lookup filtered on problem_id alone", implying a table scan. That
        // is wrong: the query below JOINs `contest`, so the planner drives
        // from the (small) set of bracket contests and the join supplies
        // contest_id as the leading column, making the PK perfectly usable.
        //
        // The index still earns its place, for a different reason. Measured
        // on 80k contest_problem rows / 2000 contests / 40 of them brackets,
        // each problem belonging to one contest:
        //
        //   without index: 0.626 ms, 92 buffers -- Index Only Scan on the PK
        //                  executed ONCE PER BRACKET CONTEST (40 loops)
        //   with index:    0.153 ms, 14 buffers -- 3 bitmap lookups, one per
        //                  problem id actually asked about
        //
        // So the real cost without it is O(number of bracket contests) rather
        // than O(problems queried): 4x here at 40 bracket contests, and it
        // degrades linearly as more are created, while the indexed plan stays
        // flat.
        //
        // Counter-case worth recording: if a problem belonged to MANY
        // contests, the index would invert -- the planner then fetches every
        // row matching problem_id before filtering by contest_type, which
        // measured 2.3x SLOWER than the PK plan. That distribution is not the
        // real one (a problem belongs to one contest, occasionally two), but
        // it is why this index is justified by the measurement above and not
        // by a general "index the filtered column" instinct.
        //
        // The lookup is on a hot path: the afternoon-bracket plugin's
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
        // exists -- which is what makes a per-bracket-contest loop worth
        // removing.
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
