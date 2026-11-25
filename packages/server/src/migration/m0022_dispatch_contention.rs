use sea_orm_migration::prelude::*;

/// How many times a unit's attempt was handed back because its plugins found
/// no plugin SQL connection free (`contended_handbacks`, on a judgement and
/// on a code run): contention with the work beside it hands an attempt back
/// spending nothing, but only up to `judging::fault::CONTENTION_HANDBACKS`
/// times per unit, after which it is a judging failure within the
/// submission's retry budget. Counted on the row, so the cap holds across
/// replicas and restarts.
///
/// Additive (release/docs/upgrade.md): defaulted columns servers of the
/// previous release keep working against.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m0022_dispatch_contention"
    }
}

const UP: &str = r#"
ALTER TABLE public.submission_judgement
    ADD COLUMN contended_handbacks integer NOT NULL DEFAULT 0;
ALTER TABLE public.code_run ADD COLUMN contended_handbacks integer NOT NULL DEFAULT 0;
"#;

const DOWN: &str = r#"
ALTER TABLE public.code_run DROP COLUMN contended_handbacks;
ALTER TABLE public.submission_judgement DROP COLUMN contended_handbacks;
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
